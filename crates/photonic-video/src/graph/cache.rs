//! Node-result cache (02 §5 "Node results" row): content-hash → GPU working
//! texture, over the pooled allocator ([`crate::pool::TexturePool`]).
//!
//! The pool owns budget / LRU / pin / bucket accounting (03 §3.4); this layer
//! adds the one thing the pool can't know on its own — whether a resident
//! texture has actually been **rendered** (a fresh allocation is uninitialized).
//! The evaluator asks [`NodeCache::lookup_or_alloc`]: a cache hit returns the
//! existing texture with no work, a miss returns a texture the caller must
//! render into and then [`mark_rendered`](NodeCache::mark_rendered).
//!
//! Textures are handed out as `Arc<wgpu::Texture>` (the allocator's `Texture`
//! type) so one node result can feed several consumers within a frame and be
//! published straight through as `EngineFrame.texture` (02 §1) — the pool's
//! recycle path is only ever driven between frames (invalidation), so a clone
//! held during eval can never alias a reused texture.

use std::sync::Arc;

use crate::graph::ir::{ContentHash, TextureDesc};
use crate::pool::{TextureAllocator, TexturePool};

/// wgpu allocator whose texture handle is a shareable `Arc<wgpu::Texture>`.
pub struct ArcWgpuAllocator {
    device: Arc<wgpu::Device>,
}

impl ArcWgpuAllocator {
    pub fn new(device: Arc<wgpu::Device>) -> Self {
        ArcWgpuAllocator { device }
    }
}

impl TextureAllocator for ArcWgpuAllocator {
    type Texture = Arc<wgpu::Texture>;

    fn can_recycle(&self, texture: &Self::Texture) -> bool {
        Arc::strong_count(texture) == 1 && Arc::weak_count(texture) == 0
    }

    fn allocate(&mut self, bucket: (u32, u32)) -> Arc<wgpu::Texture> {
        Arc::new(self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("node_result_texture"),
            size: wgpu::Extent3d {
                width: bucket.0,
                height: bucket.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        }))
    }
}

/// A snapshot of cache occupancy for `EngineStatus` (02 §1 cache stats).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub resident_entries: usize,
    pub resident_bytes: u64,
}

/// Bound on the "rendered" tracking map. A stale entry (its texture LRU-evicted)
/// is only ever a small `u128` leak — gated correctness comes from
/// [`TexturePool::contains`] — so an occasional clear on overflow is harmless.
const RENDERED_CAP: usize = 16_384;

/// Content-hash → rendered working-texture cache (02 §5).
pub struct NodeCache {
    pool: TexturePool<ArcWgpuAllocator>,
    /// Exact logical descriptors whose pooled texture holds rendered content.
    rendered: std::collections::HashMap<ContentHash, TextureDesc>,
    hits: u64,
    misses: u64,
}

impl NodeCache {
    pub fn set_budget_bytes(&mut self, budget_bytes: u64) {
        self.pool.set_budget_bytes(budget_bytes);
    }
    pub fn new(device: Arc<wgpu::Device>, budget_bytes: u64) -> Self {
        NodeCache {
            pool: TexturePool::new(ArcWgpuAllocator::new(device), budget_bytes),
            rendered: std::collections::HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    /// Look up `hash`, or allocate (recycling) a texture of `desc` on a miss.
    /// Returns `(texture, valid)`: when `valid` is `true` the texture already
    /// holds rendered content; when `false` the caller must render into it and
    /// then call [`mark_rendered`](Self::mark_rendered).
    pub fn lookup_or_alloc(
        &mut self,
        hash: ContentHash,
        desc: TextureDesc,
    ) -> (Arc<wgpu::Texture>, bool) {
        let valid = self.rendered.get(&hash) == Some(&desc) && self.pool.contains_desc(hash, desc);
        let tex = self.pool.get_or_alloc(hash, desc).clone();
        if valid {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        (tex, valid)
    }

    /// Mark `hash`'s texture as holding valid rendered content.
    pub fn mark_rendered(&mut self, hash: ContentHash) {
        if self.rendered.len() >= RENDERED_CAP {
            self.rendered.clear();
        }
        if let Some(desc) = self.pool.desc(hash) {
            self.rendered.insert(hash, desc);
        }
    }

    /// Pin the currently-displayed output tick so scrub/step never re-evaluates
    /// a frame still on screen (03 §3.4 exception 1).
    pub fn pin(&mut self, hash: ContentHash) {
        self.pool.pin(hash);
    }

    pub fn unpin(&mut self, hash: ContentHash) {
        self.pool.unpin(hash);
    }

    /// Evict every entry whose hash matches `pred` (asset relink / proxy swap,
    /// 03 §3.4 exception 2). Called between frames only.
    pub fn invalidate_matching(&mut self, pred: impl Fn(ContentHash) -> bool) {
        self.pool.evict_matching(&pred);
        self.rendered.retain(|h, _| !pred(*h));
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            resident_entries: self.pool.len(),
            resident_bytes: self.pool.used_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u128) -> ContentHash {
        ContentHash(n)
    }
    fn desc(w: u32, hh: u32) -> TextureDesc {
        TextureDesc {
            width: w,
            height: hh,
        }
    }

    fn try_device() -> Option<Arc<wgpu::Device>> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, _queue) =
            pollster::block_on(adapter.request_device(&Default::default(), None)).ok()?;
        Some(Arc::new(device))
    }

    #[test]
    fn miss_then_hit() {
        let Some(device) = try_device() else {
            eprintln!("no GPU adapter — skipping NodeCache miss_then_hit");
            return;
        };
        let mut cache = NodeCache::new(device, 64 * 1024 * 1024);
        let (_t1, valid1) = cache.lookup_or_alloc(h(1), desc(64, 64));
        assert!(!valid1, "first touch is a miss");
        cache.mark_rendered(h(1));
        let (_t2, valid2) = cache.lookup_or_alloc(h(1), desc(64, 64));
        assert!(valid2, "second touch hits");
        let s = cache.stats();
        assert_eq!(s.hits, 1);
        assert_eq!(s.misses, 1);
        assert_eq!(s.resident_entries, 1);
    }

    #[test]
    fn invalidate_drops_rendered_flag() {
        let Some(device) = try_device() else {
            eprintln!("no GPU adapter — skipping NodeCache invalidate test");
            return;
        };
        let mut cache = NodeCache::new(device, 64 * 1024 * 1024);
        cache.lookup_or_alloc(h(0x10), desc(64, 64));
        cache.mark_rendered(h(0x10));
        cache.invalidate_matching(|c| c.0 == 0x10);
        let (_t, valid) = cache.lookup_or_alloc(h(0x10), desc(64, 64));
        assert!(!valid, "invalidated entry re-renders");
    }

    #[test]
    fn same_hash_different_same_bucket_desc_is_not_valid() {
        let Some(device) = try_device() else {
            eprintln!("no GPU adapter; skipping NodeCache descriptor test");
            return;
        };
        let mut cache = NodeCache::new(device, 64 * 1024 * 1024);
        cache.lookup_or_alloc(h(0x20), desc(11, 7));
        cache.mark_rendered(h(0x20));
        let (_texture, valid) = cache.lookup_or_alloc(h(0x20), desc(12, 8));
        assert!(!valid, "logical descriptor changes must force a re-render");
    }
}
