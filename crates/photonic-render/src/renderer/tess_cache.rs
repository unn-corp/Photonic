//! Per-node tessellation cache (03-render-color-pipeline.md §2.2).
//!
//! The live renderer's `build_geometry` re-walks the whole document every frame
//! and, before this cache, re-ran every `tessellate_fill`/`tessellate_stroke`
//! call each time — the dominant CPU cost of a frame. This memoizes those three
//! *pure* tessellation functions so an unchanged path is flattened and
//! triangulated at most once, then reused until its geometry actually changes.
//!
//! ## Why content-addressed, not `(NodeId, TessKind)`-keyed
//!
//! The spec sketches a cache keyed by `(NodeId, TessKind)` invalidated by a
//! per-node `tess_inputs_hash`. This renderer resolves **symbol instances** and
//! **live-boolean groups** to a derived path at draw time
//! (`Document::resolve_render_node`), and `Command::affected_nodes` reports the
//! *edited* node's id (the symbol master / boolean child), **not** the
//! instance/group id that actually renders the derived geometry. An id-keyed,
//! `changes_since`-gated invalidation would therefore reuse a stale mesh for the
//! instance after a master edit.
//!
//! Keying each entry by a hash of the *resolved* path's serialized SVG plus the
//! exact tessellation parameters sidesteps that entirely: the key IS the pure
//! function's argument set, so a cache hit is provably the same mesh a fresh
//! call would produce (the byte-identical-output guarantee this refactor must
//! hold), identical geometry across nodes shares one entry, and undo/redo back
//! to a prior path is a natural hit. The 64-bit key is the spec's
//! `tess_inputs_hash`. The revision counter still drives the whole-frame skip
//! in `build_geometry` (§2.2 step 1); this cache is the per-tessellation layer
//! underneath it.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use photonic_core::node::NodeId;
use photonic_core::path::PathData;
use photonic_core::style::{LineCap, LineJoin};

use crate::tessellator::{tessellate_fill, tessellate_stroke, tessellate_stroke_variable, Mesh};

/// Flatness used when the live zoom scale is not threaded into the cache key.
const DEFAULT_TOLERANCE: f32 = 0.25;

/// Content-addressed memo over the three `tessellate_*` functions, plus
/// per-frame instrumentation used by the render-perf statement / tests.
#[derive(Default)]
pub(crate) struct TessCache {
    /// key (`tess_inputs_hash`) → tessellated mesh (path-local space).
    meshes: HashMap<u64, Arc<Mesh>>,
    /// Keys requested this frame. Drives mark-and-sweep eviction so the memo
    /// tracks the live working set instead of growing without bound.
    used: HashSet<u64>,
    /// Distinct nodes that triggered at least one (re-)tessellation this frame.
    retess_nodes: HashSet<NodeId>,
    /// Total `tessellate_*` calls (cache misses) this frame.
    misses: u32,
}

/// Hash of a resolved path's serialized SVG — computed once per node per frame
/// and combined with each tessellation's parameters to form its cache key, so a
/// node's path is hashed once rather than once per fill/stroke/glow layer.
pub(crate) fn hash_svg(svg: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    svg.hash(&mut h);
    h.finish()
}

fn mix(parts: &[u64]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in parts {
        p.hash(&mut h);
    }
    h.finish()
}

fn cap_disc(cap: LineCap) -> u64 {
    match cap {
        LineCap::Butt => 0,
        LineCap::Round => 1,
        LineCap::Square => 2,
    }
}

fn join_disc(join: LineJoin) -> u64 {
    match join {
        LineJoin::Miter => 0,
        LineJoin::Round => 1,
        LineJoin::Bevel => 2,
    }
}

impl TessCache {
    /// Reset the per-frame instrumentation and mark-set. Call once at the start
    /// of a build that actually walks the document (not on a frame-skip).
    pub(crate) fn begin_frame(&mut self) {
        self.used.clear();
        self.retess_nodes.clear();
        self.misses = 0;
    }

    /// Drop stale keys after a build, retaining only meshes touched this frame.
    pub(crate) fn sweep(&mut self) {
        let used = &self.used;
        self.meshes.retain(|k, _| used.contains(k));
    }

    /// `(nodes_re_tessellated, tessellate_calls)` for the last built frame.
    pub(crate) fn stats(&self) -> (u32, u32) {
        (self.retess_nodes.len() as u32, self.misses)
    }

    fn get_or(&mut self, node: NodeId, key: u64, build: impl FnOnce() -> Mesh) -> Arc<Mesh> {
        self.used.insert(key);
        if let Some(mesh) = self.meshes.get(&key) {
            return Arc::clone(mesh);
        }
        let mesh = Arc::new(build());
        self.misses += 1;
        self.retess_nodes.insert(node);
        self.meshes.insert(key, Arc::clone(&mesh));
        mesh
    }

    /// Memoized [`tessellate_fill`]. `svg_hash` is `hash_svg(path.as_svg())`.
    pub(crate) fn fill(
        &mut self,
        node: NodeId,
        path: &PathData,
        svg_hash: u64,
        even_odd: bool,
    ) -> Arc<Mesh> {
        let key = mix(&[svg_hash, 0, even_odd as u64]);
        self.get_or(node, key, || {
            tessellate_fill(path, even_odd, DEFAULT_TOLERANCE)
        })
    }

    /// Memoized [`tessellate_stroke`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn stroke(
        &mut self,
        node: NodeId,
        path: &PathData,
        svg_hash: u64,
        width: f32,
        cap: LineCap,
        join: LineJoin,
        miter: f32,
    ) -> Arc<Mesh> {
        let key = mix(&[
            svg_hash,
            1,
            width.to_bits() as u64,
            cap_disc(cap),
            join_disc(join),
            miter.to_bits() as u64,
        ]);
        self.get_or(node, key, || {
            tessellate_stroke(path, width, cap, join, miter, DEFAULT_TOLERANCE)
        })
    }

    /// Memoized [`tessellate_stroke_variable`].
    pub(crate) fn stroke_variable(
        &mut self,
        node: NodeId,
        path: &PathData,
        svg_hash: u64,
        widths: &[f64],
    ) -> Arc<Mesh> {
        let mut wh = std::collections::hash_map::DefaultHasher::new();
        for w in widths {
            w.to_bits().hash(&mut wh);
        }
        let key = mix(&[svg_hash, 2, wh.finish()]);
        self.get_or(node, key, || {
            tessellate_stroke_variable(path, widths, DEFAULT_TOLERANCE as f64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri_path() -> PathData {
        PathData::from_svg("M0 0 L10 0 L10 10 Z").unwrap()
    }

    #[test]
    fn memoized_fill_equals_direct_tessellation() {
        let path = tri_path();
        let mut cache = TessCache::default();
        cache.begin_frame();
        let node = NodeId::nil();
        let sh = hash_svg(path.as_svg());

        let cached = cache.fill(node, &path, sh, false);
        let direct = tessellate_fill(&path, false, DEFAULT_TOLERANCE);
        // Byte-identical to a fresh tessellation — the correctness backbone of
        // the whole refactor (memo of a pure function is transparent).
        assert_eq!(cached.vertices, direct.vertices);
        assert_eq!(cached.indices, direct.indices);
    }

    #[test]
    fn second_fetch_is_a_hit_no_extra_tessellation() {
        let path = tri_path();
        let mut cache = TessCache::default();
        cache.begin_frame();
        let node = NodeId::nil();
        let sh = hash_svg(path.as_svg());

        let _ = cache.fill(node, &path, sh, false);
        let _ = cache.fill(node, &path, sh, false); // same key
        let _ = cache.stroke(node, &path, sh, 2.0, LineCap::Butt, LineJoin::Miter, 4.0);
        let (nodes, calls) = cache.stats();
        // One fill + one stroke tessellated; the repeated fill was a hit.
        assert_eq!(calls, 2, "repeated identical fill must not re-tessellate");
        assert_eq!(nodes, 1);
    }

    #[test]
    fn distinct_params_miss_independently() {
        let path = tri_path();
        let mut cache = TessCache::default();
        cache.begin_frame();
        let node = NodeId::nil();
        let sh = hash_svg(path.as_svg());

        let _ = cache.stroke(node, &path, sh, 2.0, LineCap::Butt, LineJoin::Miter, 4.0);
        let _ = cache.stroke(node, &path, sh, 4.0, LineCap::Butt, LineJoin::Miter, 4.0);
        assert_eq!(cache.stats().1, 2, "different widths are different meshes");
    }

    #[test]
    fn sweep_evicts_keys_not_used_this_frame() {
        let path = tri_path();
        let mut cache = TessCache::default();
        let node = NodeId::nil();
        let sh = hash_svg(path.as_svg());

        // Frame 1: tessellate a fill.
        cache.begin_frame();
        let _ = cache.fill(node, &path, sh, false);
        cache.sweep();
        assert_eq!(cache.meshes.len(), 1);

        // Frame 2: touch nothing — the fill's key is now stale and evicted.
        cache.begin_frame();
        cache.sweep();
        assert_eq!(cache.meshes.len(), 0);
    }

    #[test]
    fn unchanged_geometry_across_frames_hits() {
        let path = tri_path();
        let mut cache = TessCache::default();
        let node = NodeId::nil();
        let sh = hash_svg(path.as_svg());

        cache.begin_frame();
        let _ = cache.fill(node, &path, sh, false);
        cache.sweep();

        // Next frame, same geometry: zero tessellation (the color-edit / pan case).
        cache.begin_frame();
        let _ = cache.fill(node, &path, sh, false);
        assert_eq!(cache.stats().1, 0, "unchanged path must be a cache hit");
        cache.sweep();
    }

    /// The perf-statement scenarios (03 §2.2 DoD) at the layer where
    /// "tessellation work" is actually counted: three nodes each with a fill and
    /// a stroke. Frame 2 = a single-node path edit; frame 3 = idle/pan/color.
    #[test]
    fn path_edit_re_tessellates_exactly_one_node() {
        // Distinct node ids and distinct paths so each has its own cache entries.
        let ids: Vec<NodeId> = (0..3).map(|_| NodeId::new_v4()).collect();
        let mut paths = vec![
            PathData::from_svg("M0 0 L10 0 L10 10 Z").unwrap(),
            PathData::from_svg("M0 0 L20 0 L20 20 Z").unwrap(),
            PathData::from_svg("M0 0 L30 0 L30 30 Z").unwrap(),
        ];
        let mut cache = TessCache::default();

        let build = |cache: &mut TessCache, ids: &[NodeId], paths: &[PathData]| {
            cache.begin_frame();
            for (n, p) in ids.iter().zip(paths) {
                let sh = hash_svg(p.as_svg());
                let _ = cache.fill(*n, p, sh, false);
                let _ = cache.stroke(*n, p, sh, 2.0, LineCap::Butt, LineJoin::Miter, 4.0);
            }
        };

        // Frame 1 (cold): all three nodes tessellate — 3 nodes, 6 calls.
        build(&mut cache, &ids, &paths);
        assert_eq!(cache.stats(), (3, 6));
        cache.sweep();

        // Frame 2 (single-node path edit): only node 1's path changes.
        paths[1] = PathData::from_svg("M0 0 L25 0 L25 25 Z").unwrap();
        build(&mut cache, &ids, &paths);
        assert_eq!(
            cache.stats(),
            (1, 2),
            "a path edit re-tessellates only the edited node (its fill + stroke)"
        );
        cache.sweep();

        // Frame 3 (idle / pan / color edit — geometry unchanged): zero work.
        build(&mut cache, &ids, &paths);
        assert_eq!(
            cache.stats(),
            (0, 0),
            "no geometry change ⇒ zero tessellation"
        );
        cache.sweep();
    }
}
