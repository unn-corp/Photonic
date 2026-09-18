//! Clip-lane rendering (04 §2.2): visible-range culling + batched rect painting.
//!
//! Each clip renders as a rounded rect with a name label; video clips sample
//! frame thumbnails across their body and audio clips draw a min/max+RMS
//! waveform envelope (spec 15 — NLE parity gap 10), both backed by the
//! background-generated caches in `photonic_video::media::thumbnails`
//! (`ThumbnailCache`/`WaveformCache`). The draw path here only ever *reads*
//! those caches (`request` never blocks); a miss draws the flat fill it
//! already painted underneath, never stalling the frame.
//!
//! **Wiring seam:** `paint_lane` takes `Option<&ThumbnailCache>` /
//! `Option<&WaveformCache>` so this module is ready to render real
//! thumbnails/waveforms the moment a live cache exists, but no such cache is
//! constructed anywhere yet — that requires a field on `PhotonicApp` /
//! `EngineBridge` (`app/mod.rs`, `app/engine.rs`), both outside this story's
//! territory (`clips.rs`/`mod.rs`/`tracks.rs` only). Until that lands, the one
//! call site (`mod.rs::draw_timeline_panel`) passes `None` for both, which is
//! exactly today's flat-fill-only behavior.
use super::layout::TimelineView;
use photonic_core::timeline::{
    AssetId, Clip, ClipId, ClipSource, SpeedMap, Tick, Track, TrackKind, TICKS_PER_SECOND,
};
use photonic_video::audio::{PeakBucket, WaveformLevel, WaveformPyramid};
use photonic_video::{ThumbHandle, ThumbnailCache, WaveformCache};
use std::collections::HashMap;
use std::sync::Arc;

/// Palette for lane painting, resolved from the active egui theme by the caller
/// so this module stays theme-agnostic.
#[derive(Clone, Copy)]
pub(crate) struct LaneColors {
    pub clip_fill: egui::Color32,
    pub clip_stroke: egui::Color32,
    pub selected_fill: egui::Color32,
    pub selected_stroke: egui::Color32,
    pub label: egui::Color32,
    pub transition: egui::Color32,
    pub offline: egui::Color32,
    /// Diagonal-hatch overlay stroke for a locked track's lane (14-nle-parity
    /// QW-2) — a shape-only cue (13 §1.6), same convention as the offline
    /// per-clip stripe, so lock state reads without relying on color alone.
    pub locked_hatch: egui::Color32,
    /// Sync-lock lane cue (14-nle-parity M-9): a thin diagonal-tick band
    /// confined to a sync-locked track's top edge — deliberately much
    /// smaller than `locked_hatch`'s full-lane coverage so the two concepts
    /// ("can't edit" vs. "ripples in lock-step with other tracks") don't read
    /// as the same affordance.
    pub sync_lock_seam: egui::Color32,
    /// Audio-clip waveform min/max envelope stroke (spec 15 §3).
    pub waveform_fill: egui::Color32,
    /// Audio-clip waveform RMS overlay stroke, drawn brighter/narrower inside
    /// the min/max envelope (spec 15 §3 "two-tone with RMS overlay").
    pub waveform_rms: egui::Color32,
    /// FX-badge dot for a clip that carries an effect stack (14-nle-parity
    /// §M-8). A functional data-coding hue (same "not chrome" precedent as
    /// `CLIP_LABEL_SWATCHES`), deliberately distinct from the selection accent
    /// so an effected clip never reads as a selected one.
    pub fx_badge: egui::Color32,
}

/// A painted clip's screen rect, returned for hit-testing in `interact.rs`.
pub(crate) struct PaintedClip {
    pub clip: ClipId,
    pub rect: egui::Rect,
}

/// Fixed swatch palette for `Clip::color_label` (14-nle-parity §M-1, gap #7's
/// UI half) — `(display name, color)` indexed by the label's `u8`. This is
/// deliberately a *separate* set of hues from the `DESIGN.md` theme tokens,
/// not derived from the active egui visuals: the label is organizational
/// data the user assigns, not chrome, the same "functional data-coding, not
/// chrome accent" precedent `DESIGN.md` documents for the node-editor port
/// sockets. `primary`'s violet is deliberately excluded so a label swatch
/// never reads as the selection cue.
pub(crate) const CLIP_LABEL_SWATCHES: &[(&str, egui::Color32)] = &[
    ("Red", egui::Color32::from_rgb(0xE5, 0x48, 0x4D)),
    ("Orange", egui::Color32::from_rgb(0xF5, 0xA5, 0x24)),
    ("Yellow", egui::Color32::from_rgb(0xE8, 0xD7, 0x4A)),
    ("Green", egui::Color32::from_rgb(0x4C, 0xC3, 0x8A)),
    ("Cyan", egui::Color32::from_rgb(0x14, 0xB8, 0xA6)),
    ("Blue", egui::Color32::from_rgb(0x3B, 0x82, 0xF6)),
    ("Pink", egui::Color32::from_rgb(0xEC, 0x48, 0x99)),
    ("Gray", egui::Color32::from_rgb(0x8B, 0x8F, 0xA3)),
];

/// Resolve a `color_label` index to its swatch color; tolerant of an
/// out-of-range index (e.g. a doc saved against a larger future palette)
/// rather than panicking on it.
pub(crate) fn label_color(label: u8) -> Option<egui::Color32> {
    CLIP_LABEL_SWATCHES.get(label as usize).map(|(_, c)| *c)
}

/// Speed-badge text color (14-nle-parity G-11): reuses the `warning` token
/// (`DESIGN.md` `#FBBF24`) rather than inventing a new hue — same "functional
/// data-coding, not chrome" precedent as `CLIP_LABEL_SWATCHES` and the
/// node-editor port colors. A local const (not a `LaneColors` field) because
/// this story's territory doesn't reach `app/timeline/mod.rs::lane_colors`,
/// which is the only place a `LaneColors` value is constructed.
const SPEED_BADGE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xFB, 0xBF, 0x24);
const SPEED_CURVE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xE9, 0xE5, 0xFF);

/// Compact on-clip speed cue text (14-nle-parity G-11): `None` for the
/// default 100%-forward case and an empty ramp (both read as "no override"),
/// same "cue absent = default" convention as the FX badge only appearing
/// when the effect stack is non-empty.
fn speed_badge_text(clip: &Clip) -> Option<String> {
    match &clip.speed {
        SpeedMap::Constant(r) if r.num == 0 => Some("FREEZE".to_string()),
        SpeedMap::Constant(r) => {
            let pct = (r.as_f64().abs() * 100.0).round() as i64;
            let reversed = r.num < 0;
            if pct == 100 && !reversed {
                None
            } else if reversed {
                Some(format!("R{pct}%"))
            } else {
                Some(format!("{pct}%"))
            }
        }
        // Piecewise-constant ramp (`clip.rs`'s `integrate_ramp`): an empty
        // key list is identity (1×), so it reads as "no override" just like
        // `Constant(1×)` does above.
        SpeedMap::Keyframed { keys } => (!keys.is_empty()).then(|| "RAMP".to_string()),
    }
}

fn speed_curve_y(speed: f64, rect: egui::Rect) -> f32 {
    let normalized = speed.signum() * (1.0 + speed.abs()).ln() / 11.0_f64.ln();
    rect.center().y - normalized as f32 * rect.height() * 0.42
}

/// Paint a read-only speed band when the user has expanded a timeline track.
/// Editing stays in the inspector curve so normal clip trim/move hit targets
/// retain their established priority.
fn paint_speed_curve_band(shapes: &mut Vec<egui::Shape>, clip: &Clip, rect: egui::Rect) {
    let SpeedMap::Keyframed { keys } = &clip.speed else {
        return;
    };
    if keys.is_empty() || rect.height() < 56.0 || rect.width() < 36.0 {
        return;
    }
    let band = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 2.0, rect.bottom() - 42.0),
        egui::pos2(rect.right() - 2.0, rect.bottom() - 2.0),
    );
    shapes.push(egui::Shape::rect_filled(
        band,
        egui::Rounding::same(2.0),
        egui::Color32::BLACK.gamma_multiply(0.22),
    ));
    let zero = speed_curve_y(0.0, band);
    shapes.push(egui::Shape::line_segment(
        [
            egui::pos2(band.left(), zero),
            egui::pos2(band.right(), zero),
        ],
        egui::Stroke::new(1.0, SPEED_BADGE_COLOR.gamma_multiply(0.65)),
    ));
    let mut sorted = keys.clone();
    sorted.sort_by_key(|key| key.at.0);
    let points: Vec<_> = sorted
        .iter()
        .map(|key| {
            let x = band.left() + band.width() * key.at.0 as f32 / clip.duration.0.max(1) as f32;
            egui::pos2(x, speed_curve_y(key.ratio.as_f64(), band))
        })
        .collect();
    if points.len() > 1 {
        shapes.push(egui::Shape::line(
            points.clone(),
            egui::Stroke::new(1.2, SPEED_CURVE_COLOR),
        ));
    }
    shapes.extend(
        points
            .into_iter()
            .map(|point| egui::Shape::circle_filled(point, 2.5, SPEED_CURVE_COLOR)),
    );
}

/// The visible slice of a track's clips for `[first, last]` lane ticks, via the
/// binary-search cull 04 §2.2 mandates (the `Vec<Clip>` is sorted + non-overlapping).
pub(crate) fn visible_clips(track: &Track, first: Tick, last: Tick) -> &[Clip] {
    let start_idx = track
        .clips
        .partition_point(|c| c.start + c.duration < first);
    let end_idx = track.clips.partition_point(|c| c.start <= last);
    let end_idx = end_idx.max(start_idx);
    &track.clips[start_idx..end_idx]
}

/// Whether a clip's source media is unreachable (offline). The media pool /
/// asset probing lands with import (05); until then no clip is offline.
fn is_offline(_clip: &Clip) -> bool {
    // P3/05 seam: resolve `ClipSource::Asset`/`Vector` against the media pool
    // and return true when the file is missing.
    false
}

// ── Thumbnails & waveforms (spec 15) ────────────────────────────────────────
//
// Pure slice/source-tick math lives here, fully unit-tested and independent
// of `egui`/the cache types below — `clip_thumbnail_slices` and
// `select_waveform_level` are the two functions spec 15 §5 calls out by name.

/// Spacing between sampled thumbnails along a video clip's body (spec 15 §3:
/// "one every ~120 px").
const THUMB_SLICE_SPACING_PX: f32 = 120.0;
/// Target thumbnail height in px (spec 15 §2: "~64").
const THUMB_TARGET_HEIGHT_PX: u32 = 64;
/// Target on-screen width of one waveform peak bucket (spec 15 §1: "≈ 1–2 px").
const WAVEFORM_TARGET_BUCKET_PX: f32 = 1.5;
/// Minimum screen-pixel stride between sampled waveform columns — coarser than
/// 1px so the envelope shape count stays bounded on a wide, long clip.
const WAVEFORM_COLUMN_STRIDE_PX: f32 = 2.0;
/// Hard ceiling for waveform columns in a single visible clip segment. At 4K
/// widths this prevents waveform painting from becoming thousands of individual
/// shapes per lane while preserving the same 2px detail for normal clips.
const MAX_WAVEFORM_COLUMNS_PER_SEGMENT: f32 = 512.0;

fn waveform_column_stride(visible_width: f32) -> f32 {
    WAVEFORM_COLUMN_STRIDE_PX.max(visible_width.max(0.0) / MAX_WAVEFORM_COLUMNS_PER_SEGMENT)
}
/// Per-frame cap on new egui texture registrations from thumbnail requests
/// (spec 15 §4). `mod.rs::draw_timeline_panel` owns one per frame and threads
/// it through every `paint_lane` call so the cap is global across all visible
/// lanes, not reset per-lane.
pub(crate) const DEFAULT_THUMBNAIL_BUDGET: u32 = 48;

/// One sampled thumbnail slot along a video clip's body: the clip-local pixel
/// span (`0` = clip start) it paints into, and the source-media tick to
/// request for it — mapped via `source_in + speed.source_delta(dt)` (01
/// §5.1) so trims, reverses, and speed ramps are all respected (spec 15 §3).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ThumbSlice {
    pub x0: f32,
    pub x1: f32,
    pub source_tick: Tick,
}

/// Sample points across a clip's rendered width at `spacing_px` intervals,
/// each mapped to a source-media tick. Pure — no `egui`/cache access — so
/// it's directly unit-testable (spec 15 §5).
pub(crate) fn clip_thumbnail_slices(
    clip: &Clip,
    view: &TimelineView,
    spacing_px: f32,
) -> Vec<ThumbSlice> {
    let width_px = view.ticks_to_px(clip.duration);
    if width_px <= 0.0 || clip.duration.0 <= 0 {
        return Vec::new();
    }
    let spacing = spacing_px.max(1.0);
    let mut slices = Vec::new();
    let mut x0 = 0.0_f32;
    while x0 < width_px {
        let x1 = (x0 + spacing).min(width_px);
        let mid = (x0 + x1) * 0.5;
        let dt = view.px_to_ticks(mid);
        let source_tick = clip.source_in + clip.speed.source_delta(dt);
        slices.push(ThumbSlice {
            x0,
            x1,
            source_tick,
        });
        x0 = x1;
    }
    slices
}

/// Pick the pyramid level whose bucket width is the finest that's still ≥
/// `target_px` screen pixels at the current zoom (spec 15 §1), falling back
/// to the coarsest level if even that one's buckets are sub-pixel (deeply
/// zoomed out). Monotone in zoom: every level's bucket px-width scales the
/// same direction with `pixels_per_tick`, so the first index clearing the
/// threshold can only move toward index 0 as zoom increases and toward the
/// last index as zoom decreases — never non-monotonically (spec 15 §5).
pub(crate) fn select_waveform_level(
    pyramid: &WaveformPyramid,
    pixels_per_tick: f64,
    target_px: f32,
) -> usize {
    if pyramid.levels.is_empty() {
        return 0;
    }
    let ticks_per_sample = TICKS_PER_SECOND as f64 / pyramid.source_sample_rate.max(1) as f64;
    for (i, level) in pyramid.levels.iter().enumerate() {
        let bucket_px = level.samples_per_bucket as f64 * ticks_per_sample * pixels_per_tick;
        if bucket_px >= target_px as f64 {
            return i;
        }
    }
    pyramid.levels.len() - 1
}

/// Map a clip-local pixel x (`0` = clip start) to the peak-bucket index
/// within `level`, honoring trim/speed exactly like `clip_thumbnail_slices`
/// does for video. `sample_rate` is `WaveformPyramid::source_sample_rate`.
/// `None` when the mapped source position is negative (shouldn't happen for
/// an `x` inside the clip body; guards an out-of-range caller `x`).
fn waveform_bucket_index(
    clip: &Clip,
    view: &TimelineView,
    x_local_px: f32,
    level: &WaveformLevel,
    sample_rate: u32,
) -> Option<usize> {
    let dt = view.px_to_ticks(x_local_px);
    let source_tick = clip.source_in + clip.speed.source_delta(dt);
    if source_tick.0 < 0 {
        return None;
    }
    let samples_per_bucket = (level.samples_per_bucket.max(1)) as i128;
    let sample = (source_tick.0 as i128 * sample_rate.max(1) as i128) / TICKS_PER_SECOND as i128;
    Some((sample / samples_per_bucket) as usize)
}

/// Merge one bucket across every channel of `level` into a single display
/// envelope (widest min/max span, loudest RMS) — 09 §8.1 keeps peaks
/// per-channel for a future two-lane stereo view; the single clip-lane
/// waveform here shows the merged loudness instead of picking one channel
/// arbitrarily.
fn merged_peak(level: &WaveformLevel, bucket: usize) -> Option<PeakBucket> {
    let mut acc: Option<PeakBucket> = None;
    for ch in &level.channels {
        let Some(b) = ch.get(bucket) else {
            continue;
        };
        acc = Some(match acc {
            None => *b,
            Some(a) => PeakBucket {
                min: a.min.min(b.min),
                max: a.max.max(b.max),
                rms: a.rms.max(b.rms),
            },
        });
    }
    acc
}

/// Per-frame cap on NEW egui texture registrations from thumbnail requests
/// (spec 15 §4). Reusing an already-registered texture never consumes
/// budget — only a first-time upload does. Logs once (via `Drop`) if any
/// request was denied this frame, so an over-budget frame is never silent
/// (spec 15 §3).
pub(crate) struct ThumbnailBudget {
    remaining: u32,
    exceeded: bool,
}

impl ThumbnailBudget {
    pub(crate) fn new(cap: u32) -> Self {
        ThumbnailBudget {
            remaining: cap,
            exceeded: false,
        }
    }

    fn take(&mut self) -> bool {
        if self.remaining == 0 {
            self.exceeded = true;
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
}

impl Drop for ThumbnailBudget {
    fn drop(&mut self) {
        if self.exceeded {
            tracing::warn!(
                "timeline thumbnail registration budget exceeded this frame; \
                 some clip thumbnails fell back to the flat fill"
            );
        }
    }
}

/// Cross-frame egui-texture cache for resident clip thumbnails, keyed by the
/// backing `RgbaThumb`'s stable `Arc` pointer identity (spec 15 §2: "the same
/// pointer identity is stable while the thumbnail stays resident"). Lives in
/// egui's own persistent memory (`Context::data_mut`) rather than a new
/// `PhotonicApp` field, since this story's territory doesn't reach
/// `app/mod.rs`.
///
/// Capped independently of `ThumbnailCache`'s own LRU: a decode-cache
/// eviction mints a fresh `Arc` on re-fetch, so this registry's key space
/// can grow across a long session even though the source cache doesn't — the
/// cap + least-recently-touched eviction below bounds resident egui texture
/// memory regardless.
#[derive(Clone, Default)]
struct ThumbTextureRegistry {
    entries: HashMap<usize, (egui::TextureHandle, u64)>,
    clock: u64,
}

/// Registry cap (spec 15 §4), comfortably above `ThumbnailCache`'s own
/// default 256-entry LRU so a live session doesn't evict-and-reupload a
/// texture that's still actually resident in the source cache.
const THUMB_TEXTURE_REGISTRY_CAP: usize = 512;

impl ThumbTextureRegistry {
    /// The egui texture id already resident for `handle` (by `Arc` identity),
    /// touching its LRU recency. `None` when not yet uploaded — the caller then
    /// uploads (outside the registry lock — see [`Self::insert`]) and inserts.
    fn get(&mut self, handle: &ThumbHandle) -> Option<egui::TextureId> {
        let key = Arc::as_ptr(handle) as usize;
        self.clock += 1;
        let clock = self.clock;
        let (tex, used) = self.entries.get_mut(&key)?;
        *used = clock;
        Some(tex.id())
    }

    /// Store an already-uploaded `tex` for `handle`, evicting the
    /// least-recently-used entry first if at capacity. Returns its texture id.
    ///
    /// The upload (`ctx.load_texture`) MUST happen before calling this and
    /// OUTSIDE the `ctx.data_mut` guard that `with_thumb_registry` holds:
    /// `load_texture` re-acquires the egui `Context` lock, which is not
    /// reentrant, so uploading while that guard is held deadlocks.
    fn insert(&mut self, handle: &ThumbHandle, tex: egui::TextureHandle) -> egui::TextureId {
        let key = Arc::as_ptr(handle) as usize;
        self.clock += 1;
        if !self.entries.contains_key(&key) && self.entries.len() >= THUMB_TEXTURE_REGISTRY_CAP {
            if let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(k, _)| *k)
            {
                self.entries.remove(&victim);
            }
        }
        let id = tex.id();
        self.entries.insert(key, (tex, self.clock));
        id
    }
}

fn thumb_registry_id() -> egui::Id {
    egui::Id::new("timeline_thumb_texture_registry")
}

fn with_thumb_registry<R>(
    ctx: &egui::Context,
    f: impl FnOnce(&mut ThumbTextureRegistry) -> R,
) -> R {
    ctx.data_mut(|d| {
        let reg = d.get_temp_mut_or_insert_with(thumb_registry_id(), ThumbTextureRegistry::default);
        f(reg)
    })
}

/// Sample + draw thumbnails across a video clip's body (spec 15 §3). A cache
/// miss or over-budget slice leaves the flat fill (already painted beneath)
/// showing through — never blocks, never panics.
#[allow(clippy::too_many_arguments)]
fn paint_thumbnails(
    shapes: &mut Vec<egui::Shape>,
    ctx: &egui::Context,
    clip: &Clip,
    asset: AssetId,
    view: &TimelineView,
    clip_left_x: f32,
    visible_rect: egui::Rect,
    cache: &ThumbnailCache,
    budget: &mut ThumbnailBudget,
) {
    for slice in clip_thumbnail_slices(clip, view, THUMB_SLICE_SPACING_PX) {
        let sx0 = (clip_left_x + slice.x0).max(visible_rect.left());
        let sx1 = (clip_left_x + slice.x1).min(visible_rect.right());
        if sx1 <= sx0 {
            continue;
        }
        let Some(handle) = cache.request(asset, slice.source_tick, THUMB_TARGET_HEIGHT_PX) else {
            continue; // miss — background decode enqueued, flat fill shows through
        };
        // Resident already? The registry lock is held only for the lookup. A
        // cold miss uploads the texture OUTSIDE that lock: `ctx.load_texture`
        // re-enters the `Context` lock, so uploading inside `with_thumb_registry`
        // (which holds `ctx.data_mut`) would deadlock the draw thread.
        let tex_id = if let Some(id) = with_thumb_registry(ctx, |reg| reg.get(&handle)) {
            id
        } else {
            if handle.width == 0 || handle.height == 0 {
                continue; // malformed — flat fill shows through
            }
            if handle.rgba.len() != handle.width as usize * handle.height as usize * 4 {
                continue; // malformed — never surfaced by decode/disk, but don't trust blindly
            }
            if !budget.take() {
                continue; // over budget this frame — flat fill shows through
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [handle.width as usize, handle.height as usize],
                &handle.rgba,
            );
            let tex = ctx.load_texture("timeline-clip-thumb", image, egui::TextureOptions::LINEAR);
            with_thumb_registry(ctx, |reg| reg.insert(&handle, tex))
        };
        let slice_rect = egui::Rect::from_min_max(
            egui::pos2(sx0, visible_rect.top()),
            egui::pos2(sx1, visible_rect.bottom()),
        );
        shapes.push(egui::Shape::image(
            tex_id,
            slice_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        ));
    }
}

/// Draw a min/max+RMS waveform envelope across an audio clip's body (spec 15
/// §3), sampled at [`WAVEFORM_COLUMN_STRIDE_PX`] intervals and batched into
/// the caller's `shapes` (04 §7b — one draw call per lane).
fn paint_waveform(
    shapes: &mut Vec<egui::Shape>,
    clip: &Clip,
    view: &TimelineView,
    clip_left_x: f32,
    visible_rect: egui::Rect,
    pyramid: &WaveformPyramid,
    colors: &LaneColors,
) {
    if visible_rect.height() < 4.0 {
        return;
    }
    let level_idx = select_waveform_level(pyramid, view.pixels_per_tick, WAVEFORM_TARGET_BUCKET_PX);
    let Some(level) = pyramid.levels.get(level_idx) else {
        return;
    };
    if level.channels.is_empty() {
        return;
    }
    let mid_y = visible_rect.center().y;
    let amp_px = (visible_rect.height() * 0.5 - 1.0).max(0.0);
    // The visible segment is the only part that can affect this frame. Scale
    // the sampling stride on very wide displays rather than emitting an
    // unbounded number of tiny line shapes (a common 4K timeline hot path).
    let column_stride = waveform_column_stride(visible_rect.width());
    let mut x = visible_rect.left();
    while x <= visible_rect.right() {
        let x_local = x - clip_left_x;
        if let Some(bucket) =
            waveform_bucket_index(clip, view, x_local, level, pyramid.source_sample_rate)
        {
            if let Some(peak) = merged_peak(level, bucket) {
                let max = peak.max.clamp(-1.0, 1.0);
                let min = peak.min.clamp(-1.0, 1.0);
                let rms = peak.rms.clamp(0.0, 1.0);
                shapes.push(egui::Shape::line_segment(
                    [
                        egui::pos2(x, mid_y - max * amp_px),
                        egui::pos2(x, mid_y - min * amp_px),
                    ],
                    egui::Stroke::new(1.0, colors.waveform_fill),
                ));
                if rms > 0.0 {
                    shapes.push(egui::Shape::line_segment(
                        [
                            egui::pos2(x, mid_y - rms * amp_px),
                            egui::pos2(x, mid_y + rms * amp_px),
                        ],
                        egui::Stroke::new(1.0, colors.waveform_rms),
                    ));
                }
            }
        }
        x += column_stride;
    }
}

/// Paint one track's visible clips into `lane_rect`, batching all rects through a
/// single `painter.extend` (04 §7a-b) and returning their screen rects for
/// hit-testing. Labels are drawn per-clip after the batch (bounded by the cull).
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_lane(
    painter: &egui::Painter,
    view: &TimelineView,
    track: &Track,
    lane_rect: egui::Rect,
    selection: &[ClipId],
    colors: &LaneColors,
    thumbnails: Option<&ThumbnailCache>,
    waveforms: Option<&WaveformCache>,
    budget: &mut ThumbnailBudget,
) -> Vec<PaintedClip> {
    let first = view.scroll_ticks;
    let last = view.x_to_tick(lane_rect.width(), 0.0);
    let clips = visible_clips(track, first, last);

    let rounding = egui::Rounding::same(3.0); // `sm` per DESIGN.md / 13 §1.5
    let mut shapes: Vec<egui::Shape> = Vec::with_capacity(clips.len() * 2);
    let mut painted = Vec::with_capacity(clips.len());
    let top = lane_rect.top() + 2.0;
    let bottom = lane_rect.bottom() - 2.0;

    for c in clips {
        let x0 = view.tick_to_x(c.start, lane_rect.left());
        let x1 = view.tick_to_x(c.end(), lane_rect.left());
        // Clamp to the lane so a partly-scrolled clip still paints its visible part.
        let rx0 = x0.max(lane_rect.left());
        let rx1 = x1.min(lane_rect.right());
        if rx1 <= rx0 {
            continue;
        }
        let rect = egui::Rect::from_min_max(egui::pos2(rx0, top), egui::pos2(rx1, bottom));
        let selected = selection.contains(&c.id);

        let mut fill = if selected {
            colors.selected_fill
        } else {
            colors.clip_fill
        };
        if !c.enabled {
            // Disabled clips render at ~50% opacity (13 §1.2).
            fill = fill.gamma_multiply(0.5);
        }
        shapes.push(egui::Shape::rect_filled(rect, rounding, fill));

        // Thumbnails (video) / waveform (audio) — spec 15. Drawn right over
        // the flat fill so a cache miss or a non-asset source (Vector/
        // NestedSequence/SolidColor/Adjustment) just leaves that fill showing.
        match track.kind {
            TrackKind::Video | TrackKind::Text => {
                if let (ClipSource::Asset { asset }, Some(cache)) = (&c.source, thumbnails) {
                    paint_thumbnails(
                        &mut shapes,
                        painter.ctx(),
                        c,
                        *asset,
                        view,
                        x0,
                        rect,
                        cache,
                        budget,
                    );
                }
            }
            TrackKind::Audio => {
                if let (ClipSource::Asset { asset }, Some(cache)) = (&c.source, waveforms) {
                    if let Some(pyramid) = cache.request(*asset) {
                        paint_waveform(&mut shapes, c, view, x0, rect, &pyramid, colors);
                    }
                }
            }
        }

        if is_offline(c) {
            // Offline: diagonal-stripe placeholder (01 §3, shape-only per 13 §1.6).
            let step = 8.0;
            let mut x = rect.left();
            while x < rect.right() {
                shapes.push(egui::Shape::line_segment(
                    [
                        egui::pos2(x, rect.bottom()),
                        egui::pos2((x + rect.height()).min(rect.right()), rect.top()),
                    ],
                    egui::Stroke::new(1.0, colors.offline),
                ));
                x += step;
            }
        }

        let stroke = if selected {
            egui::Stroke::new(1.5, colors.selected_stroke)
        } else {
            egui::Stroke::new(1.0, colors.clip_stroke)
        };
        shapes.push(egui::Shape::rect_stroke(rect, rounding, stroke));

        // Organizational color label (14-nle-parity §M-1): a thin cap along
        // the clip's top edge, rounded to match the clip's own `sm` corners.
        // Additive over the selected/disabled fill above — it never replaces
        // those states, only decorates them (13 §1's "shape+color, not
        // color-only" spirit extended to a genuinely-colored affordance).
        if let Some(label) = c.color_label {
            if let Some(mut swatch) = label_color(label) {
                if !c.enabled {
                    swatch = swatch.gamma_multiply(0.5);
                }
                let stripe_h = 3.0_f32.min(rect.height());
                let stripe_rect = egui::Rect::from_min_max(
                    rect.left_top(),
                    egui::pos2(rect.right(), rect.top() + stripe_h),
                );
                let stripe_rounding = egui::Rounding {
                    nw: rounding.nw,
                    ne: rounding.ne,
                    sw: 0.0,
                    se: 0.0,
                };
                shapes.push(egui::Shape::rect_filled(
                    stripe_rect,
                    stripe_rounding,
                    swatch,
                ));
            }
        }

        // Transition badges: small triangles at the edge(s) with a transition
        // (drawn shape-only so they read without color, 13 §1.6).
        if c.transition_in.is_some() {
            shapes.push(edge_triangle(rect, true, colors.transition));
        }
        if c.transition_out.is_some() {
            shapes.push(edge_triangle(rect, false, colors.transition));
        }

        // FX badge (14-nle-parity §M-8): a small filled dot in the clip's
        // bottom-right corner when it carries an effect stack, so effected
        // clips read at a glance without opening the inspector. Additive over
        // every state above; `clip_has_effects` is the single shared signal the
        // inspector exposes so the two surfaces never disagree.
        if crate::panels::video::clip_inspector::clip_has_effects(c) {
            let r = 2.5_f32.min(rect.width() * 0.5).min(rect.height() * 0.5);
            if r > 0.5 {
                let center = egui::pos2(rect.right() - r - 2.0, rect.bottom() - r - 2.0);
                shapes.push(egui::Shape::circle_filled(center, r, colors.fx_badge));
            }
        }

        paint_speed_curve_band(&mut shapes, c, rect);

        painted.push(PaintedClip { clip: c.id, rect });
    }

    // One batched draw call for the whole lane (04 §7b) — fills, strokes,
    // thumbnails/waveform, and labels-stripe/transition shapes all ride the
    // same `extend`.
    painter.extend(shapes);

    // Labels, clipped to each rect (bounded by the cull, so per-clip is fine).
    for pc in &painted {
        if pc.rect.width() < 18.0 {
            continue;
        }
        if let Some(name) = clip_label(track, pc.clip) {
            let text_pos = egui::pos2(pc.rect.left() + 4.0, pc.rect.top() + 2.0);
            painter.text(
                text_pos,
                egui::Align2::LEFT_TOP,
                elide(&name, pc.rect.width()),
                egui::FontId::proportional(11.0),
                colors.label,
            );
        }
    }

    // Speed badge (14-nle-parity G-11): a small text chip in the clip's
    // bottom-left corner — the opposite corner from the FX badge dot, so the
    // two cues never collide — when the clip's speed isn't the default 100%
    // forward. Same immediate-`painter.text` shape as the label loop above
    // (not batched into `shapes`, which only takes pre-laid-out galleys).
    for pc in &painted {
        if pc.rect.width() < 28.0 || pc.rect.height() < 12.0 {
            continue;
        }
        let Some(c) = track.clips.iter().find(|c| c.id == pc.clip) else {
            continue;
        };
        if let Some(text) = speed_badge_text(c) {
            let text_pos = egui::pos2(pc.rect.left() + 3.0, pc.rect.bottom() - 2.0);
            painter.text(
                text_pos,
                egui::Align2::LEFT_BOTTOM,
                text,
                egui::FontId::proportional(9.0),
                SPEED_BADGE_COLOR,
            );
        }
    }

    // Locked-track cue (14-nle-parity QW-2): a diagonal-hatch overlay across
    // the whole lane, drawn last so it reads over clips/labels and empty
    // space alike — lock state is visible independent of what's under it.
    // Interaction is gated separately (`interact.rs::hit_at`); this is
    // paint-only, clips stay fully visible underneath.
    if track.locked {
        paint_locked_hatch(painter, lane_rect, colors.locked_hatch);
    }

    // Sync-lock cue (14-nle-parity M-9): independent of `locked` above — a
    // track can be sync-locked and fully editable at the same time, so this
    // is a much smaller cue confined to the lane's top edge rather than a
    // full-lane hatch (see `LaneColors::sync_lock_seam`'s doc). The
    // ripple-propagation this toggle is meant to drive lives in
    // `ops_bridge.rs`'s ripple ops — out of this story's territory, reported
    // as a seam; this is paint-only.
    if track.sync_lock {
        paint_sync_lock_seam(painter, lane_rect, colors.sync_lock_seam);
    }

    painted
}

/// Thin diagonal-tick band along a sync-locked track's top edge (14-nle-parity
/// M-9) — a shape-only cue confined to a few px so it reads distinctly from
/// `paint_locked_hatch`'s full-lane coverage.
fn paint_sync_lock_seam(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let band_h = 5.0_f32.min(rect.height());
    let band = egui::Rect::from_min_max(
        rect.left_top(),
        egui::pos2(rect.right(), rect.top() + band_h),
    );
    let clipped = painter.with_clip_rect(band);
    let step = 6.0;
    let mut shapes = Vec::new();
    let mut x = band.left();
    while x < band.right() {
        shapes.push(egui::Shape::line_segment(
            [
                egui::pos2(x, band.bottom()),
                egui::pos2(x + band_h, band.top()),
            ],
            egui::Stroke::new(1.0, color),
        ));
        x += step;
    }
    clipped.extend(shapes);
}

/// A small right-angle triangle hugging a clip's left (`inbound`) or right edge.
fn edge_triangle(rect: egui::Rect, inbound: bool, color: egui::Color32) -> egui::Shape {
    let s = 8.0_f32.min(rect.width() * 0.4);
    let pts = if inbound {
        vec![
            rect.left_top(),
            egui::pos2(rect.left() + s, rect.top()),
            egui::pos2(rect.left(), rect.top() + s),
        ]
    } else {
        vec![
            rect.right_top(),
            egui::pos2(rect.right() - s, rect.top()),
            egui::pos2(rect.right(), rect.top() + s),
        ]
    };
    egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE)
}

/// Diagonal-hatch overlay for a locked track's whole lane (14-nle-parity
/// QW-2). Draws full-height diagonal strokes clipped to `rect` — same stripe
/// geometry as the per-clip offline placeholder, just spanning the lane
/// instead of one clip.
fn paint_locked_hatch(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let step = 10.0;
    let clipped = painter.with_clip_rect(rect);
    let mut shapes = Vec::new();
    let mut x = rect.left() - rect.height();
    while x < rect.right() {
        shapes.push(egui::Shape::line_segment(
            [
                egui::pos2(x, rect.bottom()),
                egui::pos2(x + rect.height(), rect.top()),
            ],
            egui::Stroke::new(1.0, color),
        ));
        x += step;
    }
    clipped.extend(shapes);
}

fn clip_label(track: &Track, id: ClipId) -> Option<String> {
    let c = track.clips.iter().find(|c| c.id == id)?;
    if !c.name.is_empty() {
        return Some(c.name.clone());
    }
    Some(
        match &c.source {
            ClipSource::Asset { .. } => "Clip",
            ClipSource::Vector { .. } => "Vector",
            ClipSource::NestedSequence { .. } => "Sequence",
            ClipSource::SolidColor { .. } => "Solid",
            ClipSource::Adjustment => "Adjustment",
            // `ClipSource::Text` (G-12 title/graphics clips) landed in core
            // after this match was written — closing the gap here since it's
            // a compile-blocking non-exhaustive match in this story's own
            // territory file, not a new feature of this story.
            ClipSource::Text { .. } => "Title",
            // Forward-compat (39 §2.2): a source kind this build does not
            // understand shows its preserved tag verbatim; it behaves as an
            // opaque clip everywhere else (trim/move/split), never guessed.
            ClipSource::Unknown(_) => c.source.unknown_tag().unwrap_or("Unknown"),
        }
        .to_string(),
    )
}

/// Roughly elide a label to fit `width_px` (≈6px/char at 11pt proportional).
fn elide(s: &str, width_px: f32) -> String {
    let max_chars = ((width_px - 8.0) / 6.0).floor().max(1.0) as usize;
    if s.chars().count() <= max_chars {
        s.to_string()
    } else if max_chars <= 1 {
        "…".to_string()
    } else {
        let keep: String = s.chars().take(max_chars - 1).collect();
        format!("{keep}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::clip::SpeedKey;
    use photonic_core::timeline::{FrameRate, Ratio, Sequence};

    fn track_with(spans: &[(i64, i64)]) -> Track {
        let mut t = Track::new(TrackKind::Video, "V1");
        for (start, dur) in spans {
            t.clips
                .push(Clip::new(ClipSource::Adjustment, Tick(*start), Tick(*dur)));
        }
        t
    }

    #[test]
    fn cull_returns_only_overlapping_clips() {
        // Clips at [0,100), [100,150), [200,210), [1000,1100).
        let t = track_with(&[(0, 100), (100, 50), (200, 10), (1000, 100)]);
        // Window [120, 205] should include the [100,150) and [200,210) clips only.
        let vis = visible_clips(&t, Tick(120), Tick(205));
        assert_eq!(vis.len(), 2);
        assert_eq!(vis[0].start, Tick(100));
        assert_eq!(vis[1].start, Tick(200));
    }

    #[test]
    fn cull_empty_window_before_first_clip() {
        let t = track_with(&[(500, 100), (700, 100)]);
        let vis = visible_clips(&t, Tick(0), Tick(100));
        assert!(vis.is_empty());
    }

    #[test]
    fn cull_window_after_last_clip() {
        let t = track_with(&[(0, 100), (100, 100)]);
        let vis = visible_clips(&t, Tick(500), Tick(600));
        assert!(vis.is_empty());
    }

    #[test]
    fn cull_full_range_returns_all() {
        let t = track_with(&[(0, 100), (100, 100), (200, 100)]);
        let vis = visible_clips(&t, Tick(0), Tick(1000));
        assert_eq!(vis.len(), 3);
    }

    #[test]
    fn label_falls_back_to_source_kind() {
        let mut s = Sequence::new("s", FrameRate::FPS_30, 1920, 1080);
        let t = track_with(&[(0, 100)]);
        let id = t.clips[0].id;
        s.video_tracks.push(t);
        assert_eq!(
            clip_label(&s.video_tracks[0], id).as_deref(),
            Some("Adjustment")
        );
    }

    #[test]
    fn color_label_resolves_every_swatch_in_range() {
        for i in 0..CLIP_LABEL_SWATCHES.len() {
            assert_eq!(label_color(i as u8), Some(CLIP_LABEL_SWATCHES[i].1));
        }
    }

    #[test]
    fn color_label_out_of_range_is_none() {
        assert_eq!(label_color(CLIP_LABEL_SWATCHES.len() as u8), None);
        assert_eq!(label_color(u8::MAX), None);
    }

    #[test]
    fn color_label_palette_excludes_the_selection_accent_violet() {
        // DESIGN.md reserves `primary` (#6E56CF) for the selection cue; a
        // label swatch must not collide with it (see the module doc comment
        // on `CLIP_LABEL_SWATCHES`).
        let primary = egui::Color32::from_rgb(0x6E, 0x56, 0xCF);
        assert!(!CLIP_LABEL_SWATCHES.iter().any(|(_, c)| *c == primary));
    }

    // ── Thumbnail slice math (spec 15 §5) ───────────────────────────────────

    #[test]
    fn thumbnail_slices_cover_the_clip_width_with_no_gaps_or_overlaps() {
        let view = TimelineView::default();
        let clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(2));
        let slices = clip_thumbnail_slices(&clip, &view, 120.0);
        let width = view.ticks_to_px(clip.duration);
        assert!(!slices.is_empty());
        assert_eq!(slices[0].x0, 0.0);
        assert_eq!(slices.last().unwrap().x1, width);
        for w in slices.windows(2) {
            assert_eq!(
                w[0].x1, w[1].x0,
                "no gap/overlap between consecutive slices"
            );
        }
    }

    #[test]
    fn thumbnail_slices_empty_for_zero_duration_clip() {
        let view = TimelineView::default();
        let clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::ZERO);
        assert!(clip_thumbnail_slices(&clip, &view, 120.0).is_empty());
    }

    #[test]
    fn thumbnail_slices_map_source_tick_via_source_in_at_unit_speed() {
        let view = TimelineView::default();
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.source_in = Tick::from_seconds(10);
        let slices = clip_thumbnail_slices(&clip, &view, 120.0);
        assert!(!slices.is_empty());
        for s in &slices {
            let mid = (s.x0 + s.x1) * 0.5;
            let dt = view.px_to_ticks(mid);
            assert_eq!(s.source_tick, clip.source_in + dt);
        }
    }

    #[test]
    fn thumbnail_slices_respect_speed_ramp() {
        let view = TimelineView::default();
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.speed = SpeedMap::Constant(Ratio::new(2, 1));
        let slices = clip_thumbnail_slices(&clip, &view, 120.0);
        assert!(!slices.is_empty());
        for s in &slices {
            let mid = (s.x0 + s.x1) * 0.5;
            let dt = view.px_to_ticks(mid);
            assert_eq!(s.source_tick, clip.source_in + Tick(dt.0 * 2));
        }
    }

    // ── Waveform level selection (spec 15 §5) ───────────────────────────────

    fn fixture_pyramid(levels_spb: &[u32]) -> WaveformPyramid {
        WaveformPyramid {
            asset_hash: "h".into(),
            channels: 1,
            source_sample_rate: 48_000,
            total_frames: 1_000_000,
            levels: levels_spb
                .iter()
                .map(|&spb| WaveformLevel {
                    samples_per_bucket: spb,
                    channels: vec![vec![
                        PeakBucket {
                            min: -0.5,
                            max: 0.5,
                            rms: 0.25
                        };
                        4
                    ]],
                })
                .collect(),
        }
    }

    #[test]
    fn waveform_level_selection_is_monotone_in_zoom() {
        let pyramid = fixture_pyramid(&[256, 1024, 4096, 16384]);
        let zooms = [1e-9, 1e-8, 1e-7, 1e-6, 1e-5];
        let levels: Vec<usize> = zooms
            .iter()
            .map(|&z| select_waveform_level(&pyramid, z, 1.5))
            .collect();
        for w in levels.windows(2) {
            assert!(
                w[1] <= w[0],
                "level index must be non-increasing as zoom increases: {levels:?}"
            );
        }
        assert_eq!(
            *levels.first().unwrap(),
            3,
            "coarsest at the widest zoom-out"
        );
        assert_eq!(*levels.last().unwrap(), 0, "finest once zoomed in enough");
    }

    #[test]
    fn waveform_level_selection_falls_back_to_coarsest_when_all_are_subpixel() {
        let pyramid = fixture_pyramid(&[256, 1024, 4096]);
        assert_eq!(select_waveform_level(&pyramid, 1e-12, 1.5), 2);
    }

    #[test]
    fn waveform_level_selection_empty_pyramid_returns_zero() {
        let pyramid = fixture_pyramid(&[]);
        assert_eq!(select_waveform_level(&pyramid, 1.0, 1.5), 0);
    }

    #[test]
    fn waveform_column_stride_caps_wide_viewport_work() {
        assert_eq!(waveform_column_stride(800.0), WAVEFORM_COLUMN_STRIDE_PX);
        let stride = waveform_column_stride(4096.0);
        assert!(stride > WAVEFORM_COLUMN_STRIDE_PX);
        assert!(4096.0 / stride <= MAX_WAVEFORM_COLUMNS_PER_SEGMENT + f32::EPSILON);
    }

    // ── Waveform bucket mapping + channel merge ─────────────────────────────

    #[test]
    fn waveform_bucket_index_maps_clip_relative_x_honoring_trim() {
        let view = TimelineView::default();
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.source_in = Tick::from_seconds(1);
        let level = WaveformLevel {
            samples_per_bucket: 256,
            channels: vec![vec![]],
        };
        // x_local = 0 -> dt = 0 -> source_tick = source_in (1s) -> sample = 48_000.
        let bucket = waveform_bucket_index(&clip, &view, 0.0, &level, 48_000).unwrap();
        assert_eq!(bucket, (48_000i128 / 256) as usize);
    }

    #[test]
    fn waveform_bucket_index_negative_source_position_is_none() {
        let view = TimelineView::default();
        let clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        let level = WaveformLevel {
            samples_per_bucket: 256,
            channels: vec![vec![]],
        };
        assert!(waveform_bucket_index(&clip, &view, -1_000_000.0, &level, 48_000).is_none());
    }

    #[test]
    fn merged_peak_takes_widest_span_and_loudest_rms_across_channels() {
        let level = WaveformLevel {
            samples_per_bucket: 256,
            channels: vec![
                vec![PeakBucket {
                    min: -0.2,
                    max: 0.3,
                    rms: 0.1,
                }],
                vec![PeakBucket {
                    min: -0.5,
                    max: 0.1,
                    rms: 0.4,
                }],
            ],
        };
        let merged = merged_peak(&level, 0).expect("bucket present");
        assert_eq!(merged.min, -0.5);
        assert_eq!(merged.max, 0.3);
        assert_eq!(merged.rms, 0.4);
    }

    #[test]
    fn merged_peak_out_of_range_bucket_is_none() {
        let level = WaveformLevel {
            samples_per_bucket: 256,
            channels: vec![vec![PeakBucket::default()]],
        };
        assert!(merged_peak(&level, 5).is_none());
    }

    // ── Thumbnail budget ─────────────────────────────────────────────────────

    #[test]
    fn thumbnail_budget_denies_after_cap_and_marks_exceeded() {
        let mut budget = ThumbnailBudget::new(2);
        assert!(budget.take());
        assert!(budget.take());
        assert!(!budget.take());
        assert!(budget.exceeded);
    }

    #[test]
    fn thumbnail_budget_zero_cap_is_immediately_exceeded_on_first_take() {
        let mut budget = ThumbnailBudget::new(0);
        assert!(!budget.take());
        assert!(budget.exceeded);
    }

    // ── paint_lane consumes a live cache (spec 15 integration) ───────────────

    /// A `ThumbnailSource` that returns a flat RGBA thumbnail for any request,
    /// so a primed cache reliably hits — lets the test assert `paint_lane`
    /// actually uploads a texture when handed `Some(cache)` (the headline wire).
    struct FlatThumbSource;
    impl photonic_video::media::ThumbnailSource for FlatThumbSource {
        fn asset_hash(&self, _asset: AssetId) -> Option<String> {
            None // memory-only: isolate the test from any on-disk sidecar
        }
        fn decode_thumbnail(
            &self,
            _asset: AssetId,
            _source_tick: Tick,
            target_px: u32,
        ) -> Option<photonic_video::RgbaThumb> {
            let side = target_px.max(1);
            Some(photonic_video::RgbaThumb {
                width: side,
                height: side,
                rgba: vec![180u8; (side * side * 4) as usize],
            })
        }
    }

    fn test_colors() -> LaneColors {
        let c = egui::Color32::WHITE;
        LaneColors {
            clip_fill: c,
            clip_stroke: c,
            selected_fill: c,
            selected_stroke: c,
            label: c,
            transition: c,
            offline: c,
            locked_hatch: c,
            sync_lock_seam: c,
            waveform_fill: c,
            waveform_rms: c,
            fx_badge: c,
        }
    }

    #[test]
    fn paint_lane_uploads_a_texture_when_the_cache_has_the_thumbnail() {
        let asset = AssetId::new();
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(Clip::new(
            ClipSource::Asset { asset },
            Tick::ZERO,
            Tick::from_seconds(2),
        ));
        let view = TimelineView::default();

        let dir = std::env::temp_dir().join(format!(
            "photonic-clips-paintlane-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = ThumbnailCache::new(Arc::new(FlatThumbSource), dir.clone());

        // Prime the cache for exactly the buckets `paint_lane` will request,
        // then wait for the background decodes so the paint below hits (a live
        // draw would hit on a later frame; the test collapses that wait).
        for slice in clip_thumbnail_slices(&track.clips[0], &view, THUMB_SLICE_SPACING_PX) {
            cache.request(asset, slice.source_tick, THUMB_TARGET_HEIGHT_PX);
        }
        cache.block_until_idle();
        assert!(
            !cache.is_empty(),
            "cache should hold at least one thumbnail after priming"
        );

        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 40.0));
        let colors = test_colors();

        // Run inside a real egui frame so fonts (label text) and the texture
        // manager are live; `paint_lane` registers thumbnails into the ctx's
        // persistent `ThumbTextureRegistry`.
        let ctx = egui::Context::default();
        let mut with_cache_textures = 0usize;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("paint_lane_test"));
            let painter = egui::Painter::new(ctx.clone(), layer, rect);
            let mut budget = ThumbnailBudget::new(DEFAULT_THUMBNAIL_BUDGET);
            let painted = paint_lane(
                &painter,
                &view,
                &track,
                rect,
                &[],
                &colors,
                Some(&cache),
                None,
                &mut budget,
            );
            assert_eq!(painted.len(), 1, "the one clip is painted");
            with_cache_textures = with_thumb_registry(ctx, |r| r.entries.len());
        });
        assert!(
            with_cache_textures >= 1,
            "paint_lane must upload a thumbnail texture when the cache has data \
             (got {with_cache_textures})"
        );

        // Control: a fresh ctx painted with `None` registers nothing — proving
        // the texture above came from the `Some(cache)` path, not incidental
        // state (this is exactly the old flat-fill-only behavior).
        let ctx2 = egui::Context::default();
        let mut without_cache_textures = 0usize;
        let _ = ctx2.run(egui::RawInput::default(), |ctx| {
            let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("paint_lane_none"));
            let painter = egui::Painter::new(ctx.clone(), layer, rect);
            let mut budget = ThumbnailBudget::new(DEFAULT_THUMBNAIL_BUDGET);
            paint_lane(
                &painter,
                &view,
                &track,
                rect,
                &[],
                &colors,
                None,
                None,
                &mut budget,
            );
            without_cache_textures = with_thumb_registry(ctx, |r| r.entries.len());
        });
        assert_eq!(
            without_cache_textures, 0,
            "the None path uploads no textures (flat fill only)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Speed badge (14-nle-parity G-11) ────────────────────────────────────

    #[test]
    fn speed_badge_none_for_default_forward_1x() {
        let clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        assert_eq!(clip.speed, SpeedMap::Constant(Ratio::ONE));
        assert_eq!(speed_badge_text(&clip), None);
    }

    #[test]
    fn speed_badge_shows_percent_for_non_default_constant_speed() {
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.speed = SpeedMap::Constant(Ratio::new(2, 1));
        assert_eq!(speed_badge_text(&clip), Some("200%".to_string()));
        clip.speed = SpeedMap::Constant(Ratio::new(1, 2));
        assert_eq!(speed_badge_text(&clip), Some("50%".to_string()));
    }

    #[test]
    fn speed_badge_flags_reverse_even_at_100_percent() {
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.speed = SpeedMap::Constant(Ratio::new(-1, 1));
        assert_eq!(speed_badge_text(&clip), Some("R100%".to_string()));
    }

    #[test]
    fn speed_badge_shows_ramp_for_non_empty_keyframed_speed() {
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.speed = SpeedMap::Keyframed {
            keys: vec![SpeedKey::new(Tick::ZERO, Ratio::ONE)],
        };
        assert_eq!(speed_badge_text(&clip), Some("RAMP".to_string()));
    }

    #[test]
    fn speed_badge_shows_freeze_for_zero_rate() {
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.speed = SpeedMap::Constant(Ratio::new(0, 1));
        assert_eq!(speed_badge_text(&clip), Some("FREEZE".to_string()));
    }

    #[test]
    fn speed_badge_none_for_empty_ramp_identity() {
        let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(4));
        clip.speed = SpeedMap::Keyframed { keys: vec![] };
        assert_eq!(speed_badge_text(&clip), None);
    }
}
