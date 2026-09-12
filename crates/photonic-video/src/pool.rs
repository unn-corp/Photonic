//! Pooled working-texture allocator (03-render-color-pipeline.md §3.4).
//!
//! `graph::eval` (P3) requests one `Rgba16Float` texture per node keyed by its
//! [`ContentHash`]; a cache hit returns the existing texture with no allocation,
//! and textures freed by LRU eviction are recycled per size bucket rather than
//! destroyed. The pool is bucketed by [`TextureDesc::bucket`] (dims rounded up to
//! 64px) so near-identical sequence formats share allocations, and bounded by a
//! byte budget (§3.4 states 1–2 GB). Two eviction exceptions: the currently
//! displayed `Output` tick is **pinned** (never LRU-evicted), and asset relink /
//! proxy swaps evict **by predicate** rather than waiting for LRU aging.
//!
//! The allocation backend is abstracted behind [`TextureAllocator`] so the pool
//! accounting (budget / LRU / pin / bucket) is unit-tested without a GPU; the
//! real backend is [`WgpuTextureAllocator`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::graph::ir::{ContentHash, TextureDesc};

/// `Rgba16Float` = 8 bytes per texel.
const BYTES_PER_TEXEL: u64 = 8;

/// Bytes a bucket-sized working texture occupies.
fn bucket_bytes(bucket: (u32, u32)) -> u64 {
    bucket.0 as u64 * bucket.1 as u64 * BYTES_PER_TEXEL
}

/// Creates the pooled textures. Abstracted so pool logic is GPU-free testable.
pub trait TextureAllocator {
    type Texture;
    /// Allocate a fresh texture of exactly `bucket` (width, height) dimensions.
    fn allocate(&mut self, bucket: (u32, u32)) -> Self::Texture;
    /// Shared handles may outlive a cache entry. Only recycle exclusive storage.
    fn can_recycle(&self, _texture: &Self::Texture) -> bool {
        false
    }
}

struct Entry<T> {
    texture: T,
    bucket: (u32, u32),
    desc: TextureDesc,
    last_used: u64,
}

/// LRU, budget-bounded pool of working textures keyed by [`ContentHash`].
pub struct TexturePool<A: TextureAllocator> {
    allocator: A,
    budget_bytes: u64,
    used_bytes: u64,
    clock: u64,
    entries: HashMap<ContentHash, Entry<A::Texture>>,
    pinned: HashSet<ContentHash>,
    /// Recyclable textures freed by eviction, keyed by size bucket.
    free: HashMap<(u32, u32), Vec<A::Texture>>,
}

/// Default working-set budget (03 §3.4 states a 1–2 GB range); 1.5 GiB.
pub const DEFAULT_BUDGET_BYTES: u64 = 1536 * 1024 * 1024;

impl<A: TextureAllocator> TexturePool<A> {
    pub fn new(allocator: A, budget_bytes: u64) -> Self {
        Self {
            allocator,
            budget_bytes,
            used_bytes: 0,
            clock: 0,
            entries: HashMap::new(),
            pinned: HashSet::new(),
            free: HashMap::new(),
        }
    }

    pub fn with_default_budget(allocator: A) -> Self {
        Self::new(allocator, DEFAULT_BUDGET_BYTES)
    }

    /// Total bytes the pool currently holds (resident entries + free-list).
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn set_budget_bytes(&mut self, budget_bytes: u64) {
        self.budget_bytes = budget_bytes;
        self.reclaim_to_fit(0);
    }

    /// Number of resident (cached) entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `hash` is currently resident (a cache hit would not allocate).
    pub fn contains(&self, hash: ContentHash) -> bool {
        self.entries.contains_key(&hash)
    }

    /// Whether `hash` is resident for this exact logical descriptor. Physical
    /// bucket equality is insufficient because downstream sampling uses the
    /// logical width and height.
    pub fn contains_desc(&self, hash: ContentHash, desc: TextureDesc) -> bool {
        self.entries
            .get(&hash)
            .is_some_and(|entry| entry.desc == desc)
    }

    pub(crate) fn desc(&self, hash: ContentHash) -> Option<TextureDesc> {
        self.entries.get(&hash).map(|entry| entry.desc)
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Get the texture for `hash`, allocating (or recycling) on a miss (03 §3.4).
    ///
    /// Cache hit: refresh recency and return the existing texture — no
    /// allocation. Miss: reuse a same-bucket free-list texture if available,
    /// else evict to fit the budget (dropping free-list textures first, then the
    /// least-recently-used **unpinned** entry) and allocate.
    pub fn get_or_alloc(&mut self, hash: ContentHash, desc: TextureDesc) -> &A::Texture {
        let now = self.tick();
        if self.contains_desc(hash, desc) {
            self.entries.get_mut(&hash).unwrap().last_used = now;
            return &self.entries[&hash].texture;
        }
        if let Some(entry) = self.entries.remove(&hash) {
            // A caller may still hold the old texture handle. Do not recycle it
            // into the replacement and overwrite a frame that is on screen.
            self.used_bytes -= bucket_bytes(entry.bucket);
            drop(entry.texture);
        }

        let bucket = desc.bucket();
        let texture = if let Some(t) = self.free.get_mut(&bucket).and_then(Vec::pop) {
            // Recycle a freed texture of the same bucket — no new bytes, no alloc.
            t
        } else {
            self.reclaim_to_fit(bucket_bytes(bucket));
            self.used_bytes += bucket_bytes(bucket);
            self.allocator.allocate(bucket)
        };

        self.entries.insert(
            hash,
            Entry {
                texture,
                bucket,
                desc,
                last_used: now,
            },
        );
        &self.entries[&hash].texture
    }

    /// Free room for `need` more bytes by dropping recyclable textures, then the
    /// least-recently-used unpinned entry, until within budget (or nothing left
    /// to drop — the budget is a soft ceiling, never a hard allocation failure).
    fn reclaim_to_fit(&mut self, need: u64) {
        while self.used_bytes + need > self.budget_bytes {
            // Prefer dropping an already-free (recyclable) texture.
            if let Some((&bucket, list)) = self.free.iter_mut().find(|(_, l)| !l.is_empty()) {
                list.pop();
                self.used_bytes -= bucket_bytes(bucket);
                continue;
            }
            // Else evict the LRU unpinned resident entry.
            let victim = self
                .entries
                .iter()
                .filter(|(h, _)| !self.pinned.contains(h))
                .min_by_key(|(_, e)| e.last_used)
                .map(|(h, _)| *h);
            match victim {
                Some(h) => {
                    let e = self.entries.remove(&h).unwrap();
                    // Drop outright (frees bytes) — do not recycle, we are over budget.
                    self.used_bytes -= bucket_bytes(e.bucket);
                    drop(e.texture);
                }
                None => break, // everything left is pinned
            }
        }
    }

    /// Pin `hash` so it is never LRU-evicted (03 §3.4 exception 1: the currently
    /// displayed `Output` tick). No-op if not resident yet; pin persists and
    /// takes effect once it is inserted.
    pub fn pin(&mut self, hash: ContentHash) {
        self.pinned.insert(hash);
    }

    pub fn unpin(&mut self, hash: ContentHash) {
        self.pinned.remove(&hash);
    }

    /// Evict every resident entry whose hash matches `pred` (03 §3.4 exception 2:
    /// asset relink / proxy swap). Explicit invalidation overrides pinning — a
    /// matching pinned entry is dropped and unpinned. Evicted textures are
    /// recycled onto the free-list for reuse.
    pub fn evict_matching(&mut self, pred: impl Fn(ContentHash) -> bool) {
        let victims: Vec<ContentHash> = self.entries.keys().copied().filter(|h| pred(*h)).collect();
        for h in victims {
            let e = self.entries.remove(&h).unwrap();
            self.pinned.remove(&h);
            if self.allocator.can_recycle(&e.texture) {
                self.free.entry(e.bucket).or_default().push(e.texture);
            } else {
                self.used_bytes -= bucket_bytes(e.bucket);
            }
        }
    }
}

/// Real backend: allocates `Rgba16Float` working textures on a `wgpu::Device`.
pub struct WgpuTextureAllocator {
    device: Arc<wgpu::Device>,
}

impl WgpuTextureAllocator {
    pub fn new(device: Arc<wgpu::Device>) -> Self {
        Self { device }
    }
}

impl TextureAllocator for WgpuTextureAllocator {
    type Texture = wgpu::Texture;

    fn can_recycle(&self, _texture: &Self::Texture) -> bool {
        true
    }

    fn allocate(&mut self, bucket: (u32, u32)) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("pooled_working_texture"),
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u128) -> ContentHash {
        ContentHash(n)
    }
    fn desc(width: u32, height: u32) -> TextureDesc {
        TextureDesc { width, height }
    }

    /// Mock allocator: each texture is a unique id; counts total allocations so
    /// tests can distinguish a fresh allocation from a cache hit / recycle.
    #[derive(Default)]
    struct MockAllocator {
        next_id: u64,
        allocations: u64,
    }
    impl TextureAllocator for MockAllocator {
        type Texture = u64;
        fn can_recycle(&self, _texture: &Self::Texture) -> bool {
            true
        }
        fn allocate(&mut self, _bucket: (u32, u32)) -> u64 {
            self.allocations += 1;
            let id = self.next_id;
            self.next_id += 1;
            id
        }
    }

    #[test]
    fn bucket_accounting() {
        // 100×100 rounds up to 128×128; 128*128*8 = 131072 bytes.
        assert_eq!(desc(100, 100).bucket(), (128, 128));
        let mut pool = TexturePool::new(MockAllocator::default(), 10 * 1024 * 1024);
        pool.get_or_alloc(h(1), desc(100, 100));
        assert_eq!(pool.used_bytes(), 128 * 128 * 8);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn cache_hit_does_not_allocate() {
        let mut pool = TexturePool::new(MockAllocator::default(), 10 * 1024 * 1024);
        let first = *pool.get_or_alloc(h(1), desc(64, 64));
        let again = *pool.get_or_alloc(h(1), desc(64, 64));
        assert_eq!(first, again, "same hash returns the same texture");
        assert_eq!(pool.allocator.allocations, 1, "no second allocation");
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn same_hash_different_bucket_replaces_texture() {
        let mut pool = TexturePool::new(MockAllocator::default(), 10 * 1024 * 1024);
        let first = *pool.get_or_alloc(h(1), desc(64, 64));
        let resized = *pool.get_or_alloc(h(1), desc(65, 65));
        assert_ne!(
            first, resized,
            "a different bucket cannot reuse the resident texture"
        );
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn same_hash_same_bucket_tracks_exact_logical_desc() {
        let mut pool = TexturePool::new(MockAllocator::default(), 10 * 1024 * 1024);
        let first = *pool.get_or_alloc(h(2), desc(11, 7));
        assert!(pool.contains_desc(h(2), desc(11, 7)));
        let resized = *pool.get_or_alloc(h(2), desc(12, 8));
        assert_ne!(
            first, resized,
            "descriptor replacement must not overwrite a texture still held by a caller"
        );
        assert!(!pool.contains_desc(h(2), desc(11, 7)));
        assert!(pool.contains_desc(h(2), desc(12, 8)));
    }

    #[test]
    fn lru_eviction_over_budget() {
        // Budget fits exactly two 64×64 (each 64*64*8 = 32768 bytes).
        let mut pool = TexturePool::new(MockAllocator::default(), 2 * 64 * 64 * 8);
        pool.get_or_alloc(h(1), desc(64, 64));
        pool.get_or_alloc(h(2), desc(64, 64));
        pool.get_or_alloc(h(1), desc(64, 64)); // touch 1 → 2 is now LRU
        pool.get_or_alloc(h(3), desc(64, 64)); // must evict the LRU (2)
        assert!(pool.contains(h(1)), "recently-used entry survives");
        assert!(!pool.contains(h(2)), "LRU entry evicted");
        assert!(pool.contains(h(3)));
        assert_eq!(pool.used_bytes(), 2 * 64 * 64 * 8);
    }

    #[test]
    fn pinned_entry_survives_eviction() {
        let mut pool = TexturePool::new(MockAllocator::default(), 2 * 64 * 64 * 8);
        pool.get_or_alloc(h(1), desc(64, 64));
        pool.pin(h(1)); // pin the oldest
        pool.get_or_alloc(h(2), desc(64, 64));
        pool.get_or_alloc(h(3), desc(64, 64)); // over budget → evicts 2, not pinned 1
        assert!(pool.contains(h(1)), "pinned entry never evicted");
        assert!(!pool.contains(h(2)));
        assert!(pool.contains(h(3)));
    }

    #[test]
    fn evict_matching_removes_only_matches() {
        let mut pool = TexturePool::new(MockAllocator::default(), 10 * 1024 * 1024);
        pool.get_or_alloc(h(0x10), desc(64, 64));
        pool.get_or_alloc(h(0x11), desc(64, 64));
        pool.get_or_alloc(h(0x20), desc(64, 64));
        pool.pin(h(0x10)); // explicit invalidation overrides the pin
        pool.evict_matching(|c| c.0 >> 4 == 0x1); // asset-prefix 0x1_
        assert!(!pool.contains(h(0x10)));
        assert!(!pool.contains(h(0x11)));
        assert!(pool.contains(h(0x20)), "non-matching entry kept");
    }

    #[test]
    fn evicted_texture_is_recycled() {
        let mut pool = TexturePool::new(MockAllocator::default(), 10 * 1024 * 1024);
        pool.get_or_alloc(h(1), desc(64, 64));
        pool.evict_matching(|_| true); // free h(1)'s texture to the recycle list
        assert_eq!(pool.len(), 0);
        let allocs_before = pool.allocator.allocations;
        pool.get_or_alloc(h(2), desc(64, 64)); // same bucket → reuse, no new alloc
        assert_eq!(
            pool.allocator.allocations, allocs_before,
            "same-bucket allocation reuses the recycled texture"
        );
    }

    fn try_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        pollster::block_on(adapter.request_device(&Default::default(), None)).ok()
    }

    #[test]
    fn gpu_smoke_allocates_and_reuses_real_textures() {
        let Some((device, _queue)) = try_device() else {
            eprintln!("no GPU adapter — skipping texture-pool GPU smoke test");
            return;
        };
        let mut pool = TexturePool::new(
            WgpuTextureAllocator::new(Arc::new(device)),
            64 * 1024 * 1024,
        );
        let id_a = pool.get_or_alloc(h(1), desc(1920, 1080)).global_id();
        let id_a_again = pool.get_or_alloc(h(1), desc(1920, 1080)).global_id();
        assert_eq!(id_a, id_a_again, "cache hit returns the same wgpu texture");
        let id_b = pool.get_or_alloc(h(2), desc(1920, 1080)).global_id();
        assert_ne!(id_a, id_b, "distinct hashes get distinct textures");
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn t005_retained_arc_and_weak_handles_prevent_texture_recycling() {
        #[derive(Default)]
        struct SharedAllocator(u64);
        impl TextureAllocator for SharedAllocator {
            type Texture = Arc<std::sync::atomic::AtomicU64>;
            fn allocate(&mut self, _: (u32, u32)) -> Self::Texture {
                self.0 += 1;
                Arc::new(std::sync::atomic::AtomicU64::new(self.0))
            }
            fn can_recycle(&self, texture: &Self::Texture) -> bool {
                Arc::strong_count(texture) == 1 && Arc::weak_count(texture) == 0
            }
        }
        let mut pool = TexturePool::new(SharedAllocator::default(), 1024 * 1024);
        let retained = pool.get_or_alloc(h(1), desc(64, 64)).clone();
        let weak = Arc::downgrade(pool.get_or_alloc(h(2), desc(64, 64)));
        pool.evict_matching(|_| true);
        assert_eq!(
            pool.used_bytes(),
            0,
            "externally held textures are not pooled storage"
        );
        let next = pool.get_or_alloc(h(3), desc(64, 64)).clone();
        next.store(900, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(retained.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            weak.upgrade().is_none(),
            "weak-only allocation was discarded, not recycled"
        );
        pool.set_budget_bytes(0);
        assert_eq!(pool.used_bytes(), 0);
    }
}
