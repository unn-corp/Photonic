//! Still-image decode sizing and the size-keyed still cache (26 K-C8, 32 §9).
//!
//! `DecodeStill` used to cache one uploaded texture **per asset**, so a 6000 px
//! JPEG was decoded and uploaded at full resolution regardless of the canvas the
//! frame was being evaluated at, and stayed resident that way. Worse, the single
//! entry was served for *every* request, so the same still asked for at two
//! different sizes got whichever size happened to be decoded first.
//!
//! The key is `(AssetId, width, height)` — mirroring the `uploads` key shape and
//! the vector-raster cache's `VectorStateKey` (which already carries size).
//!
//! # Which size is the key? The **logical** one.
//!
//! The width/height in the key is the *logical picture size* — the canvas the
//! evaluator asked for, clamped to the asset's native size — **never** the
//! physical texture size the pool hands back. Source uploads are padded up to
//! the texture pool's 64 px bucket ([`crate::graph::ir::TextureDesc::bucket`]),
//! so a 100×100 still lives in a 128×128 texture. Keying on the bucket would
//! collapse every request in the same bucket (100×100 and 128×128, say) onto one
//! entry and then serve one scale for the other — exactly the
//! texture-size-is-not-picture-size confusion fixed for grade power windows in
//! `photonic-render/src/grade_gpu.rs`. Bucketing is an allocation detail chosen
//! *downstream* of the decode; nobody ever requests a bucket.
//!
//! # What varies the decoded bytes (and therefore belongs in the key)
//!
//! | Input | In the key? | Why |
//! |---|---|---|
//! | Asset identity | yes | different file, different pixels |
//! | Requested logical size | yes | it *is* the resample target |
//! | Preview scale (Draft/Full) | **no, already folded in** | Draft only shrinks the canvas (`preview_canvas`), and the canvas *is* the requested size. Adding it as a separate component would split the cache for a distinction that produces identical bytes |
//! | Colour conversion | **no** | the still upload path is parameterless: sRGB8 straight → linear-light premultiplied, always. Unlike video there is no per-asset `Colorimetry` to vary it |
//!
//! An over-specified key silently destroys the hit rate, which is the whole
//! point of the item — hence the clamp in [`still_target_size`] and the
//! canonicalization in [`StillCache::key_for`]: every request big enough to want
//! the full native image lands on **one** entry, not one per canvas.
//!
//! # No GUI route, no MCP tool — recorded exception
//!
//! This is an internal, per-session GPU cache, not a user verb: nothing about it
//! is addressable, undoable, or persisted, so there is no state for a tool or a
//! panel to act on. The user-facing control already exists and is unchanged —
//! the **Draft/Full preview-quality toggle**, which shrinks the canvas and
//! therefore the size a still is decoded and uploaded at. Adding a tool to poke
//! a cache key would expose an implementation detail the document does not carry
//! and could not round-trip. (ROADMAP §10 DoD 2/3 exception.)

use std::collections::{HashMap, HashSet};

use photonic_core::RasterImage;

use crate::contract::AssetId;
use crate::graph::ops::srgb_to_linear;

/// Cache key: asset plus the **logical** decoded picture size (see module docs).
pub type StillKey = (AssetId, u32, u32);

/// The size a still should actually be decoded/uploaded at, given its native
/// size and the logical size the evaluator requested (the canvas).
///
/// Per-axis `min`: never upscale (a 400 px logo on a 4K canvas is uploaded at
/// 400 px and stretched by the evaluator exactly as before), never keep more
/// resolution than the canvas can show (the 6000 px JPEG case).
///
/// Per *axis*, not aspect-preserving fit, deliberately: the evaluator normalizes
/// every source op to the canvas with an identity matrix
/// (`Evaluator::normalize_source_cached`), so a still's own aspect ratio has
/// never participated in its geometry — only in its resolution. Fitting instead
/// of clamping would throw away detail on the wide axis (6000×4000 → 1620×1080
/// instead of 1920×1080) for a picture that gets stretched to 1920×1080 anyway.
/// Clamping per axis also makes the common case land *exactly* on the canvas
/// size, which lets `normalize_source_cached` return the frame untouched — one
/// less GPU pass and one less pooled texture.
pub fn still_target_size(native: (u32, u32), requested: (u32, u32)) -> (u32, u32) {
    (
        requested.0.clamp(1, native.0.max(1)),
        requested.1.clamp(1, native.1.max(1)),
    )
}

/// Wholesale-clear cap on learned native sizes. Two `u32`s per asset, so this is
/// far larger than the texture cap; it exists only so a session that touches
/// thousands of stills cannot grow the map without bound.
const NATIVE_CAP: usize = 512;

/// Still-image cache keyed on `(asset, logical size)`.
///
/// Generic over the cached value so the keying can be unit-tested with no GPU
/// (the engine instantiates it as `StillCache<GpuFrame>`).
pub struct StillCache<T> {
    entries: HashMap<StillKey, StillEntry<T>>,
    /// Native decoded size per asset, learned on the first decode. Lets a later
    /// request for a *larger* canvas canonicalize onto the entry that already
    /// holds the full-resolution image instead of allocating a second copy.
    native: HashMap<AssetId, (u32, u32)>,
    cap: usize,
    byte_budget: u64,
    clock: u64,
}

struct StillEntry<T> {
    value: T,
    bytes: u64,
    last_used: u64,
}

impl<T> StillCache<T> {
    pub fn new(cap: usize) -> Self {
        StillCache {
            entries: HashMap::new(),
            native: HashMap::new(),
            cap: cap.max(1),
            byte_budget: u64::MAX,
            clock: 0,
        }
    }

    pub fn with_byte_budget(mut self, bytes: u64) -> Self {
        self.byte_budget = bytes;
        self
    }

    pub fn resident_bytes(&self) -> u64 {
        self.entries.values().map(|entry| entry.bytes).sum()
    }

    /// The canonical key for `requested`. Once the asset's native size is known
    /// the request is clamped to it, so every canvas at or above native maps to
    /// the same entry. Before that (first decode) the raw request is used — the
    /// lookup misses, and [`insert`](Self::insert) files the result under the
    /// canonical key so the next request hits.
    pub fn key_for(&self, asset: AssetId, requested: (u32, u32)) -> StillKey {
        let req = (requested.0.max(1), requested.1.max(1));
        match self.native.get(&asset) {
            Some(native) => {
                let (w, h) = still_target_size(*native, req);
                (asset, w, h)
            }
            None => (asset, req.0, req.1),
        }
    }

    pub fn get(&mut self, asset: AssetId, requested: (u32, u32)) -> Option<&T> {
        self.clock = self.clock.wrapping_add(1);
        let key = self.key_for(asset, requested);
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = self.clock;
        Some(&entry.value)
    }

    /// File a freshly decoded still. `native` is the asset's decoded size, which
    /// is what makes the key canonical from here on.
    ///
    /// Evict the coldest entries to satisfy both count and physical GPU bytes.
    /// An individual oversize frame is returned by its caller but not retained.
    pub fn insert(&mut self, asset: AssetId, native: (u32, u32), requested: (u32, u32), value: T) {
        if self.native.len() >= NATIVE_CAP && !self.native.contains_key(&asset) {
            self.native.clear();
        }
        self.native
            .insert(asset, (native.0.max(1), native.1.max(1)));
        let key = self.key_for(asset, requested);
        let (bw, bh) = crate::graph::ir::TextureDesc {
            width: key.1,
            height: key.2,
        }
        .bucket();
        let bytes = u64::from(bw) * u64::from(bh) * 8;
        self.entries.remove(&key);
        if bytes > self.byte_budget {
            return;
        }
        while self.entries.len() >= self.cap
            || self.resident_bytes().saturating_add(bytes) > self.byte_budget
        {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            self.entries.remove(&victim);
        }
        self.clock = self.clock.wrapping_add(1);
        self.entries.insert(
            key,
            StillEntry {
                value,
                bytes,
                last_used: self.clock,
            },
        );
    }

    /// Drop everything (project swap / whole-cache invalidation).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.native.clear();
    }

    /// Drop every entry belonging to `assets` (targeted relink/proxy-swap
    /// eviction). The learned native size goes too — a relink can point the
    /// asset at a file with different dimensions.
    pub fn remove_assets(&mut self, assets: &HashSet<AssetId>) {
        self.entries
            .retain(|(asset, _, _), _| !assets.contains(asset));
        self.native.retain(|asset, _| !assets.contains(asset));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The keys currently held, for assertions and diagnostics.
    pub fn keys(&self) -> impl Iterator<Item = &StillKey> {
        self.entries.keys()
    }
}

/// Convert an sRGB8 straight-alpha [`RasterImage`] to linear-light
/// **premultiplied** RGBA (D-09, the compositor working space), resampling to
/// `tw`×`th` with a box (area) filter on the way, and hand each target pixel to
/// `emit` in row-major order.
///
/// Downscale only: `tw`/`th` are clamped to the source size, matching
/// [`still_target_size`]. At 1:1 every target pixel covers exactly one source
/// pixel, so the output is bit-identical to the straight per-pixel convert this
/// replaced — the no-scale path is untouched.
///
/// Averaging happens **after** premultiplication, in linear light. Averaging
/// straight sRGB values would darken every gradient and bleed opaque colour out
/// of transparent pixels along an alpha edge.
///
/// Streams through a callback rather than returning a `Vec<[f32; 4]>` so the
/// caller can pack straight into its f16 upload buffer; a 4K target would
/// otherwise cost a 128 MB intermediate.
pub fn resample_linear_premult(
    img: &RasterImage,
    tw: u32,
    th: u32,
    mut emit: impl FnMut([f32; 4]),
) {
    let sw = img.width.max(1);
    let sh = img.height.max(1);
    let tw = tw.clamp(1, sw);
    let th = th.clamp(1, sh);
    for ty in 0..th {
        let (y0, y1) = span(ty, th, sh);
        for tx in 0..tw {
            let (x0, x1) = span(tx, tw, sw);
            let mut acc = [0.0f32; 4];
            let mut n = 0.0f32;
            for sy in y0..y1 {
                let row = (sy as usize) * (sw as usize) * 4;
                for sx in x0..x1 {
                    let i = row + (sx as usize) * 4;
                    let px = match img.pixels.get(i..i + 4) {
                        Some(px) => px,
                        None => continue,
                    };
                    let a = px[3] as f32 / 255.0;
                    acc[0] += srgb_to_linear(px[0] as f32 / 255.0) * a;
                    acc[1] += srgb_to_linear(px[1] as f32 / 255.0) * a;
                    acc[2] += srgb_to_linear(px[2] as f32 / 255.0) * a;
                    acc[3] += a;
                    n += 1.0;
                }
            }
            if n > 0.0 {
                for c in acc.iter_mut() {
                    *c /= n;
                }
            }
            emit(acc);
        }
    }
}

/// Half-open source span `[lo, hi)` covered by target index `t` of `n` over a
/// `src`-long axis. Always non-empty (`hi > lo`) and never past `src`.
#[inline]
fn span(t: u32, n: u32, src: u32) -> (u32, u32) {
    let lo = ((t as u64) * (src as u64) / (n as u64)) as u32;
    let hi = (((t as u64) + 1) * (src as u64) / (n as u64)) as u32;
    (lo.min(src - 1), hi.max(lo + 1).min(src))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(n: u128) -> AssetId {
        AssetId(uuid::Uuid::from_u128(n))
    }

    /// Four solid quadrants — survives any correct area downscale by an even
    /// factor with its corner colours exactly intact.
    fn quadrants(size: u32) -> RasterImage {
        let mut img = RasterImage::new(size, size);
        let half = size / 2;
        for y in 0..size {
            for x in 0..size {
                let rgba = match (x < half, y < half) {
                    (true, true) => [255, 0, 0, 255],
                    (false, true) => [0, 255, 0, 255],
                    (true, false) => [0, 0, 255, 255],
                    (false, false) => [255, 255, 255, 255],
                };
                img.set_pixel(x, y, rgba);
            }
        }
        img
    }

    fn resampled(img: &RasterImage, tw: u32, th: u32) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        resample_linear_premult(img, tw, th, |p| out.push(p));
        out
    }

    // ── the key ─────────────────────────────────────────────────────────────

    #[test]
    fn target_size_clamps_to_native_and_never_upscales() {
        // The 6000 px JPEG on a 1080p canvas: decode at the canvas, not native.
        assert_eq!(still_target_size((6000, 4000), (1920, 1080)), (1920, 1080));
        // A small still on a big canvas: never upscale on the CPU.
        assert_eq!(still_target_size((400, 300), (1920, 1080)), (400, 300));
        // Mixed axes clamp independently.
        assert_eq!(still_target_size((6000, 600), (1920, 1080)), (1920, 600));
        // Degenerate requests floor at 1×1 rather than producing a 0-sized
        // texture (wgpu would reject it).
        assert_eq!(still_target_size((100, 100), (0, 0)), (1, 1));
    }

    #[test]
    fn two_sizes_of_one_asset_are_two_entries_and_never_serve_each_other() {
        // The K-C8 defect in miniature: with the old `HashMap<AssetId, _>` key
        // BOTH of these lookups returned the first-decoded entry.
        let mut cache: StillCache<(u32, u32)> = StillCache::new(8);
        let a = asset(1);
        cache.insert(a, (6000, 4000), (1920, 1080), (1920, 1080));
        cache.insert(a, (6000, 4000), (960, 540), (960, 540));

        assert_eq!(cache.len(), 2, "one entry per requested size");
        assert_eq!(cache.get(a, (1920, 1080)), Some(&(1920, 1080)));
        assert_eq!(cache.get(a, (960, 540)), Some(&(960, 540)));
        // Sensitivity: an asset-only key would answer *something* for a size
        // that was never decoded. A size-keyed one must miss.
        assert_eq!(cache.get(a, (1280, 720)), None);
    }

    #[test]
    fn requests_at_or_above_native_share_one_entry() {
        // The over-specification trap: if the key were the raw request, a small
        // still would get a fresh full-resolution copy per canvas size and the
        // hit rate would collapse to zero.
        let mut cache: StillCache<(u32, u32)> = StillCache::new(8);
        let a = asset(2);
        // First request is bigger than the image; it is filed at native size.
        cache.insert(a, (400, 300), (1920, 1080), (400, 300));
        assert_eq!(
            cache.keys().copied().collect::<Vec<_>>(),
            vec![(a, 400, 300)]
        );
        // Every other canvas at or above native canonicalizes onto it.
        assert_eq!(cache.get(a, (1920, 1080)), Some(&(400, 300)));
        assert_eq!(cache.get(a, (1280, 720)), Some(&(400, 300)));
        assert_eq!(cache.get(a, (400, 300)), Some(&(400, 300)));
        assert_eq!(cache.len(), 1);
        // Below native is a genuinely different picture, so it still misses.
        assert_eq!(cache.get(a, (200, 150)), None);
    }

    #[test]
    fn keys_do_not_collide_across_assets() {
        let mut cache: StillCache<u32> = StillCache::new(8);
        cache.insert(asset(1), (100, 100), (50, 50), 1);
        cache.insert(asset(2), (100, 100), (50, 50), 2);
        assert_eq!(cache.get(asset(1), (50, 50)), Some(&1));
        assert_eq!(cache.get(asset(2), (50, 50)), Some(&2));
    }

    #[test]
    fn removing_an_asset_forgets_its_entries_and_its_native_size() {
        let mut cache: StillCache<u32> = StillCache::new(8);
        let a = asset(3);
        let b = asset(4);
        cache.insert(a, (400, 300), (1920, 1080), 1);
        cache.insert(b, (400, 300), (1920, 1080), 2);
        cache.remove_assets(&HashSet::from([a]));
        assert_eq!(cache.get(b, (1920, 1080)), Some(&2));
        assert_eq!(cache.get(a, (1920, 1080)), None);
        // The native size went too: after a relink the file may be a different
        // size, so the next request must not canonicalize on the stale one.
        assert_eq!(cache.key_for(a, (1920, 1080)), (a, 1920, 1080));
        assert_eq!(cache.key_for(b, (1920, 1080)), (b, 400, 300));
    }

    #[test]
    fn overflow_clears_wholesale_and_stays_bounded() {
        let mut cache: StillCache<u32> = StillCache::new(4);
        for i in 0..10u32 {
            cache.insert(asset(i as u128), (100, 100), (50, 50), i);
        }
        assert!(cache.len() <= 4, "cap held, got {}", cache.len());
        assert!(!cache.is_empty());
    }

    // ── the resample ────────────────────────────────────────────────────────

    #[test]
    fn identity_resample_matches_the_plain_per_pixel_convert() {
        // The no-scale path must stay byte-for-byte what it was before K-C8.
        let img = quadrants(4);
        let got = resampled(&img, 4, 4);
        let want: Vec<[f32; 4]> = img
            .pixels
            .chunks_exact(4)
            .map(|px| {
                let a = px[3] as f32 / 255.0;
                [
                    srgb_to_linear(px[0] as f32 / 255.0) * a,
                    srgb_to_linear(px[1] as f32 / 255.0) * a,
                    srgb_to_linear(px[2] as f32 / 255.0) * a,
                    a,
                ]
            })
            .collect();
        assert_eq!(got.len(), want.len());
        for (g, w) in got.iter().zip(want.iter()) {
            assert_eq!(g, w);
        }
    }

    #[test]
    fn area_downscale_preserves_flat_regions_exactly() {
        // 64×64 quadrants → 8×8: every target pixel sits wholly inside one
        // quadrant, so the colours must come through untouched.
        let img = quadrants(64);
        let out = resampled(&img, 8, 8);
        assert_eq!(out.len(), 64);
        let at = |x: usize, y: usize| out[y * 8 + x];
        let red = srgb_to_linear(1.0);
        assert!((at(0, 0)[0] - red).abs() < 1e-5 && at(0, 0)[1] < 1e-5);
        assert!((at(7, 0)[1] - red).abs() < 1e-5 && at(7, 0)[0] < 1e-5);
        assert!((at(0, 7)[2] - red).abs() < 1e-5 && at(0, 7)[0] < 1e-5);
        assert!(at(7, 7).iter().all(|c| (c - 1.0).abs() < 1e-5));
    }

    #[test]
    fn area_downscale_averages_in_linear_light_not_gamma() {
        // A 2×1 black/white pair down to 1×1. The sRGB midpoint (0.5 → ~0.214)
        // is NOT the linear midpoint (0.5): averaging in gamma space would give
        // the wrong, too-dark answer.
        let mut img = RasterImage::new(2, 1);
        img.set_pixel(0, 0, [0, 0, 0, 255]);
        img.set_pixel(1, 0, [255, 255, 255, 255]);
        let out = resampled(&img, 1, 1);
        assert!(
            (out[0][0] - 0.5).abs() < 1e-5,
            "linear average of 0 and 1 is 0.5, got {}",
            out[0][0]
        );
        assert!(
            (out[0][0] - srgb_to_linear(0.5)).abs() > 0.2,
            "must not be the gamma-space average"
        );
    }

    #[test]
    fn area_downscale_premultiplies_before_averaging() {
        // Opaque white beside transparent white. Premultiplied-then-averaged
        // gives (0.5, 0.5) — colour weighted by coverage. Averaging straight
        // values first would give RGB 1.0 at alpha 0.5, i.e. a bright fringe
        // bleeding out of a fully transparent pixel.
        let mut img = RasterImage::new(2, 1);
        img.set_pixel(0, 0, [255, 255, 255, 255]);
        img.set_pixel(1, 0, [255, 255, 255, 0]);
        let out = resampled(&img, 1, 1);
        assert!((out[0][3] - 0.5).abs() < 1e-5, "alpha averages to 0.5");
        assert!(
            (out[0][0] - 0.5).abs() < 1e-5,
            "premultiplied RGB averages to 0.5, got {}",
            out[0][0]
        );
    }

    #[test]
    fn every_source_pixel_is_covered_exactly_once() {
        // The span partition is what makes the filter an average rather than a
        // decimation; a gap or overlap would alias.
        for src in [1u32, 2, 3, 7, 64, 100] {
            for n in 1..=src {
                let mut covered = vec![0u32; src as usize];
                for t in 0..n {
                    let (lo, hi) = span(t, n, src);
                    assert!(hi > lo && hi <= src, "span {t}/{n} of {src} = {lo}..{hi}");
                    for i in lo..hi {
                        covered[i as usize] += 1;
                    }
                }
                assert!(
                    covered.iter().all(|c| *c == 1),
                    "n={n} src={src} coverage {covered:?}"
                );
            }
        }
    }

    #[test]
    fn resample_never_upscales_past_the_source() {
        // Defence in depth against a caller that skips `still_target_size`.
        let img = quadrants(4);
        let out = resampled(&img, 64, 64);
        assert_eq!(out.len(), 16, "clamped to the 4×4 source");
    }

    #[test]
    fn t005_still_budget_uses_padded_bytes_and_preserves_hot_images() {
        let mut cache = StillCache::new(16).with_byte_budget(2 * 64 * 64 * 8);
        cache.insert(asset(1), (50, 50), (50, 50), 1);
        cache.insert(asset(2), (50, 50), (50, 50), 2);
        assert_eq!(cache.get(asset(1), (50, 50)), Some(&1));
        cache.insert(asset(3), (50, 50), (50, 50), 3);
        assert_eq!(cache.resident_bytes(), 2 * 64 * 64 * 8);
        assert!(cache.get(asset(2), (50, 50)).is_none());
        assert_eq!(cache.get(asset(1), (50, 50)), Some(&1));
        cache.insert(asset(4), (128, 128), (128, 128), 4);
        assert!(cache.get(asset(4), (128, 128)).is_none());
        assert_eq!(cache.len(), 2);
    }
}
