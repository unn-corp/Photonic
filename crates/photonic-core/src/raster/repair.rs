//! Retouching tools — the Photoshop "repair" family as pure CPU operations.
//!
//! Everything here is deterministic and unit-tested, with no GPU or windowing
//! dependency, matching the rest of the [`crate::raster`] subsystem. Each
//! neighborhood op reads from an immutable snapshot ([`RasterImage::clone`]) of
//! the source pixels so that writes never contaminate later reads.
//!
//! Implemented:
//! - [`healing_brush`] — frequency-separation patch from a source offset that
//!   inherits the destination's low-frequency color/lighting.
//! - [`spot_healing`] — auto-heal a blemish by exemplar-synthesizing real
//!   texture from the surrounding ring, then matching the destination's
//!   low-frequency color (frequency separation) for a seamless join.
//! - [`content_aware_fill`] — exemplar-based (PatchMatch-lite) inpaint that
//!   copies the best-matching known patch into each hole pixel, boundary-inward,
//!   so texture is *transplanted* rather than averaged into a blur.
//! - [`red_eye`] — desaturate strongly-red pixels toward dark gray.
//! - [`dust_and_scratches`] — selective median despeckle that only replaces
//!   pixels diverging from the local median by more than a threshold.
//!
//! All ops clamp to `0..=255` and never panic for any input: every public op
//! sanitizes non-finite (`NaN`/`inf`) `cx`/`cy`/`radius` (treating them as a
//! no-op) and clamps `radius` to a sane maximum before any work — including the
//! Gaussian low-pass, whose kernel size would otherwise explode. `radius == 0`
//! / empty selections are no-ops.

use crate::raster::{
    blend_result,
    image::{luma, RasterImage},
    mask::Mask,
};
use std::collections::BinaryHeap;

// ── Shared geometry helpers ──────────────────────────────────────────────────────

/// Circular coverage for a pixel at `(x, y)` relative to center `(cx, cy)`.
///
/// Returns `1.0` well inside the disc, tapering linearly to `0.0` at the radius
/// edge (a soft, anti-aliased falloff), and `0.0` outside. `radius <= 0` yields
/// `0.0` everywhere.
#[inline]
fn disc_falloff(x: u32, y: u32, cx: f32, cy: f32, radius: f32) -> f32 {
    if radius <= 0.0 {
        return 0.0;
    }
    let dx = x as f32 + 0.5 - cx;
    let dy = y as f32 + 0.5 - cy;
    let d = (dx * dx + dy * dy).sqrt();
    if d >= radius {
        return 0.0;
    }
    let feather = (radius * 0.3).max(1.0);
    if d <= radius - feather {
        1.0
    } else {
        ((radius - d) / feather).clamp(0.0, 1.0)
    }
}

/// Inclusive integer bounding box of the disc, clamped to the image.
fn disc_bounds(img: &RasterImage, cx: f32, cy: f32, radius: f32) -> (u32, u32, u32, u32) {
    let x0 = ((cx - radius).floor() as i64).clamp(0, img.width as i64 - 1) as u32;
    let y0 = ((cy - radius).floor() as i64).clamp(0, img.height as i64 - 1) as u32;
    let x1 = ((cx + radius).ceil() as i64).clamp(0, img.width as i64 - 1) as u32;
    let y1 = ((cy + radius).ceil() as i64).clamp(0, img.height as i64 - 1) as u32;
    (x0, y0, x1, y1)
}

/// Blend `healed` over `original` by `t` (0..1), preserving nothing implicitly.
#[inline]
fn mix(original: [u8; 4], healed: [u8; 4], t: f32) -> [u8; 4] {
    let t = t.clamp(0.0, 1.0);
    let mut out = [0u8; 4];
    for c in 0..4 {
        out[c] = (original[c] as f32 * (1.0 - t) + healed[c] as f32 * t)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    out
}

// ── Panic-proofing ───────────────────────────────────────────────────────────────

/// Largest brush radius any op will honor. A "sane max" that bounds all derived
/// work (loop extents, and especially the Gaussian kernel size, which grows with
/// the radius). Anything larger is clamped to this so a huge / runaway radius can
/// never trigger a pathological allocation or hang.
const MAX_RADIUS: f32 = 2048.0;

/// Sanitize a circular brush spec shared by [`healing_brush`], [`spot_healing`]
/// and [`red_eye`].
///
/// Returns `None` — meaning the caller must no-op — when the center or radius is
/// non-finite (`NaN`/`±inf`) or the radius is non-positive. Otherwise returns the
/// center plus a radius clamped to [`MAX_RADIUS`], so no downstream code (loop
/// bounds, the Gaussian low-pass kernel, …) can ever receive a value that would
/// panic, over-allocate, or hang.
#[inline]
fn sanitize_disc(cx: f32, cy: f32, radius: f32) -> Option<(f32, f32, f32)> {
    if !cx.is_finite() || !cy.is_finite() || !radius.is_finite() || radius <= 0.0 {
        return None;
    }
    Some((cx, cy, radius.min(MAX_RADIUS)))
}

// ── Exemplar-based inpaint (shared core) ─────────────────────────────────────────

/// Patch half-extent for exemplar matching. The comparison/copy patch is
/// `(2·PATCH_R+1)²` = 7×7. Copying the whole patch (not just its center) is what
/// lets continuous linear structure propagate across a hole.
const PATCH_R: i64 = 3;
/// Soft floor added to the structure (data) term so flat regions — where the
/// gradient is zero — still make progress via the confidence term alone.
const DATA_EPS: f32 = 1.0e-3;
/// Half-extent of the **local** exemplar search window, in pixels. Each target
/// patch is matched only against source centers within a `(2·SEARCH_R+1)²`
/// neighborhood around it, so the per-patch search cost is a bounded constant
/// that does NOT scale with total image area (a 4000px photo costs the same per
/// patch as a 64px thumbnail). This is generous enough to span typical hole
/// surroundings, and because the fill advances boundary-inward the just-filled
/// ring is always a valid, nearby source — so good matches stay in range even
/// for the center of a large hole. No fixed whole-image stride that skips most
/// of a big image.
const SEARCH_R: i64 = 24;
/// Quantization scale used to turn the floating priority into an integer heap
/// key (priorities are heuristic, so the fixed-point key is exact-enough and
/// gives a stable, deterministic ordering).
const PRIORITY_SCALE: f64 = 1.0e6;

#[inline]
fn idx(w: i64, x: i64, y: i64) -> usize {
    (y * w + x) as usize
}

/// Average of a pixel's known 8-neighbors (fallback when no exemplar exists).
/// Returns the pixel's current value if it has no known neighbor at all.
fn neighbor_average(img: &RasterImage, known: &[bool], w: i64, h: i64, x: i64, y: i64) -> [u8; 4] {
    let mut acc = [0f32; 4];
    let mut n = 0u32;
    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x + dx;
            let ny = y + dy;
            if nx < 0 || ny < 0 || nx >= w || ny >= h {
                continue;
            }
            if known[idx(w, nx, ny)] {
                let p = img.pixel(nx as u32, ny as u32);
                for c in 0..4 {
                    acc[c] += p[c] as f32;
                }
                n += 1;
            }
        }
    }
    if n == 0 {
        return img.pixel(x as u32, y as u32);
    }
    let inv = 1.0 / n as f32;
    [
        (acc[0] * inv).round().clamp(0.0, 255.0) as u8,
        (acc[1] * inv).round().clamp(0.0, 255.0) as u8,
        (acc[2] * inv).round().clamp(0.0, 255.0) as u8,
        (acc[3] * inv).round().clamp(0.0, 255.0) as u8,
    ]
}

#[inline]
fn in_bounds(w: i64, h: i64, x: i64, y: i64) -> bool {
    x >= 0 && y >= 0 && x < w && y < h
}

/// Luma of a pixel in the 0..=255 scale (alpha ignored).
#[inline]
fn pixel_luma(p: [u8; 4]) -> f32 {
    luma([p[0] as f32, p[1] as f32, p[2] as f32])
}

/// True if `(x, y)` has at least one KNOWN 8-neighbor (i.e. it sits on the fill
/// front).
fn has_known_neighbor(known: &[bool], w: i64, h: i64, x: i64, y: i64) -> bool {
    for dy in -1..=1 {
        for dx in -1..=1 {
            if dx == 0 && dy == 0 {
                continue;
            }
            let nx = x + dx;
            let ny = y + dy;
            if in_bounds(w, h, nx, ny) && known[idx(w, nx, ny)] {
                return true;
            }
        }
    }
    false
}

/// Criminisi confidence term C(p): mean confidence over the in-bounds pixels of
/// the patch centered at `(x, y)`. High when the patch is mostly surrounded by
/// already-known (high-confidence) pixels.
fn patch_confidence(confidence: &[f32], w: i64, h: i64, x: i64, y: i64) -> f32 {
    let mut sum = 0f32;
    let mut count = 0f32;
    for dy in -PATCH_R..=PATCH_R {
        for dx in -PATCH_R..=PATCH_R {
            let nx = x + dx;
            let ny = y + dy;
            if !in_bounds(w, h, nx, ny) {
                continue;
            }
            sum += confidence[idx(w, nx, ny)];
            count += 1.0;
        }
    }
    if count == 0.0 {
        0.0
    } else {
        sum / count
    }
}

/// Structure (data) term proxy: the luma contrast (max − min) across the KNOWN
/// 8-neighbors of `(x, y)`. Large at edges / isophotes (e.g. the boundary of a
/// bar), zero in flat regions — so structured patches are filled first and linear
/// structure propagates across the hole.
fn structure_strength(img: &RasterImage, known: &[bool], w: i64, h: i64, x: i64, y: i64) -> f32 {
    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    for dy in -1..=1 {
        for dx in -1..=1 {
            let nx = x + dx;
            let ny = y + dy;
            if !in_bounds(w, h, nx, ny) || !known[idx(w, nx, ny)] {
                continue;
            }
            let l = pixel_luma(img.pixel(nx as u32, ny as u32));
            mn = mn.min(l);
            mx = mx.max(l);
        }
    }
    if mx < mn {
        0.0
    } else {
        mx - mn
    }
}

/// Find the source patch center `(sx, sy)` whose patch best matches the target
/// patch at `(tx, ty)` — lowest mean SSD over the positions where BOTH the target
/// and source patches are known. The whole matched patch (not just its center) is
/// later copied into the target's unknown pixels, which is what reconnects linear
/// structure across a hole.
///
/// The search is **local and bounded**: only source centers within a
/// `(2·SEARCH_R+1)²` window around `(tx, ty)` are examined (see [`SEARCH_R`]), so
/// the per-patch cost is a constant that does NOT grow with total image area and
/// can never hang — yet it still finds genuinely good matches because the
/// boundary-inward fill keeps freshly-filled, real texture within range (no fixed
/// whole-image stride that skips most of a large photo).
///
/// Deterministic: source centers are scanned in fixed row-major order and the
/// first center achieving a strictly lower score wins (no RNG).
fn best_patch_match(
    img: &RasterImage,
    known: &[bool],
    w: i64,
    h: i64,
    tx: i64,
    ty: i64,
) -> Option<(i64, i64)> {
    let mut best_score = f64::INFINITY;
    let mut best: Option<(i64, i64)> = None;

    let sy0 = (ty - SEARCH_R).max(0);
    let sy1 = (ty + SEARCH_R).min(h - 1);
    let sx0 = (tx - SEARCH_R).max(0);
    let sx1 = (tx + SEARCH_R).min(w - 1);

    for sy in sy0..=sy1 {
        for sx in sx0..=sx1 {
            // Candidate center must itself be a real source pixel, and not the
            // target itself.
            if known[idx(w, sx, sy)] && !(sx == tx && sy == ty) {
                let mut ssd = 0f64;
                let mut overlap = 0u32;
                for dy in -PATCH_R..=PATCH_R {
                    for dx in -PATCH_R..=PATCH_R {
                        let txx = tx + dx;
                        let tyy = ty + dy;
                        let sxx = sx + dx;
                        let syy = sy + dy;
                        if !in_bounds(w, h, txx, tyy) || !in_bounds(w, h, sxx, syy) {
                            continue;
                        }
                        // Compare only where BOTH patches are known.
                        if !known[idx(w, txx, tyy)] || !known[idx(w, sxx, syy)] {
                            continue;
                        }
                        let a = img.pixel(txx as u32, tyy as u32);
                        let b = img.pixel(sxx as u32, syy as u32);
                        for c in 0..3 {
                            let d = a[c] as f64 - b[c] as f64;
                            ssd += d * d;
                        }
                        overlap += 1;
                    }
                }
                if overlap > 0 {
                    // Mean SSD over the overlap, with a tiny bias that prefers
                    // larger overlap (more real structure) on near-ties.
                    let score = ssd / overlap as f64 - overlap as f64 * 1.0e-6;
                    if score < best_score {
                        best_score = score;
                        best = Some((sx, sy));
                    }
                }
            }
        }
    }

    best
}

/// A pending fill-front pixel in the priority heap. The integer `key` is the
/// quantized Criminisi priority (`confidence · (structure + ε)`); entries are
/// ordered so the highest priority pops first, ties broken deterministically
/// toward the smallest `(y, x)` (matching the old row-major first-winner rule).
///
/// Entries are allowed to be **stale**: the priority of a front pixel changes as
/// nearby pixels are filled, so on pop the current priority is recomputed and a
/// mismatched entry is simply re-pushed with its fresh key. This is what lets the
/// front be maintained incrementally — only pixels near a just-filled patch are
/// re-evaluated — instead of rescanning the whole image every iteration.
#[derive(Clone, Copy, PartialEq, Eq)]
struct FrontEntry {
    key: i64,
    x: i64,
    y: i64,
}

impl Ord for FrontEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Max-heap on key; on ties prefer the smaller y then smaller x (so the
        // top-left-most front pixel wins, as the old row-major scan did).
        self.key
            .cmp(&other.key)
            .then_with(|| other.y.cmp(&self.y))
            .then_with(|| other.x.cmp(&self.x))
    }
}

impl PartialOrd for FrontEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Quantized Criminisi priority of the patch at `(x, y)` plus its confidence term
/// (`C(p)`, reused as the confidence assigned to the pixels this patch fills).
#[inline]
fn front_priority(
    img: &RasterImage,
    known: &[bool],
    confidence: &[f32],
    w: i64,
    h: i64,
    x: i64,
    y: i64,
) -> (i64, f32) {
    let conf = patch_confidence(confidence, w, h, x, y);
    let data = structure_strength(img, known, w, h, x, y);
    let prio = conf * (data + DATA_EPS);
    ((prio as f64 * PRIORITY_SCALE) as i64, conf)
}

/// Criminisi-style exemplar inpaint of every `false` cell in `known`.
///
/// The fill front (unknown pixels with ≥1 known 8-neighbor) is kept in a max
/// **priority heap** keyed on `confidence · (structure + ε)` — favoring patches
/// that are both well surrounded by known pixels (confidence) and sit on strong
/// gradients / isophotes (structure), so linear structure fills first. The front
/// is built once, then maintained **incrementally**: each iteration only
///
/// 1. pops the highest-priority front pixel (lazily re-pushing stale entries, see
///    [`FrontEntry`]) — no O(w·h) rescan of the whole image,
/// 2. finds its best-matching source patch with a bounded local search
///    ([`best_patch_match`]),
/// 3. copies the WHOLE matched patch's known pixels into the target patch's
///    still-unknown positions (propagating continuous structure, not a single
///    center pixel), raising their confidence, and
/// 4. re-evaluates the priority of only the front pixels NEAR what was just
///    filled (a `±2·PATCH_R` box), pushing fresh entries for them.
///
/// Because neither the front maintenance nor the source search scans the whole
/// image, total cost scales with the hole, not the image area — so it stays fast
/// on multi-megapixel photographs.
///
/// Deterministic (fixed heap ordering / tie-break, no RNG) and always
/// terminating: every accepted target fills ≥1 pixel, an exhausted front (e.g. a
/// fully-masked image with no source) empties the heap, and hard guards bound
/// total work so it can never hang.
fn exemplar_inpaint(work: &mut RasterImage, known: &mut [bool]) {
    let w = work.width as i64;
    let h = work.height as i64;
    if w == 0 || h == 0 {
        return;
    }
    let total = (w * h) as usize;

    // Confidence: 1.0 for original known pixels, 0.0 for the hole.
    let mut confidence = vec![0f32; total];
    for (i, &k) in known.iter().enumerate() {
        if k {
            confidence[i] = 1.0;
        }
    }

    // Build the initial fill front once (the only full-image pass).
    let mut heap: BinaryHeap<FrontEntry> = BinaryHeap::new();
    for y in 0..h {
        for x in 0..w {
            if !known[idx(w, x, y)] && has_known_neighbor(known, w, h, x, y) {
                let (key, _) = front_priority(work, known, &confidence, w, h, x, y);
                heap.push(FrontEntry { key, x, y });
            }
        }
    }

    // Guards: every accepted target fills ≥1 pixel, so the hole (≤ total pixels)
    // cannot need more accepted iterations than there are pixels. A separate pop
    // budget backstops any pathological churn of stale re-pushes.
    let max_iters = total as i64 + 8;
    let mut iters = 0i64;
    let max_pops = max_iters.saturating_mul(64).saturating_add(64);
    let mut pops = 0i64;

    while let Some(top) = heap.pop() {
        pops += 1;
        if pops > max_pops {
            break;
        }
        let tx = top.x;
        let ty = top.y;

        // Already filled by an earlier whole-patch copy → drop the stale entry.
        if known[idx(w, tx, ty)] {
            continue;
        }
        // No longer on the front (shouldn't normally happen) → drop it.
        if !has_known_neighbor(known, w, h, tx, ty) {
            continue;
        }
        // Lazily validate the priority: if it has drifted since this entry was
        // queued, re-push with the fresh key and let the real maximum surface.
        let (cur_key, cterm) = front_priority(work, known, &confidence, w, h, tx, ty);
        if cur_key != top.key {
            heap.push(FrontEntry {
                key: cur_key,
                x: tx,
                y: ty,
            });
            continue;
        }

        match best_patch_match(work, known, w, h, tx, ty) {
            Some((sx, sy)) => {
                // Copy the whole matched patch into the target's unknown pixels.
                for dy in -PATCH_R..=PATCH_R {
                    for dx in -PATCH_R..=PATCH_R {
                        let txx = tx + dx;
                        let tyy = ty + dy;
                        let sxx = sx + dx;
                        let syy = sy + dy;
                        if !in_bounds(w, h, txx, tyy) || known[idx(w, txx, tyy)] {
                            continue;
                        }
                        if !in_bounds(w, h, sxx, syy) || !known[idx(w, sxx, syy)] {
                            continue;
                        }
                        let v = work.pixel(sxx as u32, syy as u32);
                        work.set_pixel(txx as u32, tyy as u32, v);
                        known[idx(w, txx, tyy)] = true;
                        confidence[idx(w, txx, tyy)] = cterm;
                    }
                }
            }
            None => {
                // No usable source patch: fall back to the known-neighbor average
                // so the front still advances.
                let v = neighbor_average(work, known, w, h, tx, ty);
                work.set_pixel(tx as u32, ty as u32, v);
                known[idx(w, tx, ty)] = true;
                confidence[idx(w, tx, ty)] = cterm;
            }
        }

        // Re-evaluate ONLY the front near what was just filled. Every pixel just
        // filled lies within ±PATCH_R of (tx, ty); a pixel's priority depends on
        // pixels within ±PATCH_R of it, so any front pixel whose priority changed
        // lies within ±2·PATCH_R of (tx, ty). New front pixels (unknown neighbors
        // of the freshly-filled region) are inside that box too and get queued
        // here. Stale duplicates are harmless (validated lazily on pop).
        let rr = 2 * PATCH_R;
        for dy in -rr..=rr {
            for dx in -rr..=rr {
                let nx = tx + dx;
                let ny = ty + dy;
                if !in_bounds(w, h, nx, ny) || known[idx(w, nx, ny)] {
                    continue;
                }
                if !has_known_neighbor(known, w, h, nx, ny) {
                    continue;
                }
                let (key, _) = front_priority(work, known, &confidence, w, h, nx, ny);
                heap.push(FrontEntry { key, x: nx, y: ny });
            }
        }

        iters += 1;
        if iters > max_iters {
            break;
        }
    }

    // Any pixels left unknown (e.g. an unreachable, fully-enclosed region with no
    // source) are filled by neighbor diffusion so the buffer is fully defined.
    diffuse_fill(work, known);
}

/// Smooth onion-peel diffusion inpaint: fill each hole pixel from the average of
/// its known neighbors, boundary-inward. Produces a *low-frequency* color field
/// that continues smoothly across the hole's edge (no exemplar texture). Used by
/// [`spot_healing`] to reconstruct a blemish-free destination color for the
/// frequency-separation seam blend. Deterministic and always terminating.
fn diffuse_fill(work: &mut RasterImage, known: &mut [bool]) {
    let w = work.width as i64;
    let h = work.height as i64;
    if w == 0 || h == 0 {
        return;
    }
    loop {
        let mut to_fill: Vec<(i64, i64, [u8; 4])> = Vec::new();
        for y in 0..h {
            for x in 0..w {
                if known[idx(w, x, y)] {
                    continue;
                }
                let avg = neighbor_average(work, known, w, h, x, y);
                // Only fill pixels that actually have a known neighbor.
                let mut has = false;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x + dx;
                        let ny = y + dy;
                        if nx >= 0 && ny >= 0 && nx < w && ny < h && known[idx(w, nx, ny)] {
                            has = true;
                        }
                    }
                }
                if has {
                    to_fill.push((x, y, avg));
                }
            }
        }
        if to_fill.is_empty() {
            break;
        }
        for &(x, y, v) in &to_fill {
            work.set_pixel(x as u32, y as u32, v);
            known[idx(w, x, y)] = true;
        }
    }
}

// ── Healing brush ───────────────────────────────────────────────────────────────

/// Healing brush — copy texture from a source offset `(src_dx, src_dy)` into a
/// circular region at `(cx, cy, radius)`, but match the *destination's*
/// low-frequency color/lighting so the patch blends seamlessly.
///
/// This is the classic frequency-separation / gradient-domain trick:
///
/// ```text
/// result = source_texture + dest_lowfreq
///        = (source - source_lowfreq) + dest_lowfreq
/// ```
///
/// where `lowfreq` is a Gaussian blur of the image. The high-frequency *texture*
/// comes from the source, while the smooth color and lighting come from the
/// destination, so the seam disappears. A circular falloff at the radius edge
/// feathers the patch into its surroundings. Alpha at the destination is kept.
pub fn healing_brush(
    img: &mut RasterImage,
    cx: f32,
    cy: f32,
    radius: f32,
    src_dx: i64,
    src_dy: i64,
) {
    let (cx, cy, radius) = match sanitize_disc(cx, cy, radius) {
        Some(v) => v,
        None => return,
    };
    // Immutable source snapshot + its low-frequency (blurred) version.
    let snapshot = img.clone();
    let mut low = snapshot.clone();
    let sigma = (radius * 0.5).max(1.0);
    crate::raster::filter::gaussian_blur(&mut low, sigma, None);

    let (x0, y0, x1, y1) = disc_bounds(img, cx, cy, radius);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let cov = disc_falloff(x, y, cx, cy, radius);
            if cov <= 0.0 {
                continue;
            }
            let sx = x as i64 + src_dx;
            let sy = y as i64 + src_dy;
            let src = snapshot.sample_clamped(sx, sy);
            let src_low = low.sample_clamped(sx, sy);
            let dst_low = low.pixel(x, y);
            let original = snapshot.pixel(x, y);

            let mut healed = original;
            for c in 0..3 {
                let v = src[c] as f32 - src_low[c] as f32 + dst_low[c] as f32;
                healed[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            // Keep the destination's own alpha.
            healed[3] = original[3];

            img.set_pixel(x, y, mix(original, healed, cov));
        }
    }
}

// ── Spot healing ────────────────────────────────────────────────────────────────

/// Spot healing — auto-heal a circular blemish at `(cx, cy, radius)` without an
/// explicit source.
///
/// The disc is treated as a hole and re-synthesized in two complementary ways
/// from the *surrounding* pixels, then recombined by frequency separation:
///
/// 1. **Texture** — an exemplar-based fill ([`exemplar_inpaint`]) transplants
///    real high-frequency texture from the surrounding ring into the disc, so
///    structure (grain, pattern, pores) is reproduced rather than smeared.
/// 2. **Color** — a smooth onion-peel diffusion ([`diffuse_fill`]) reconstructs
///    a blemish-free low-frequency color/lighting field that continues smoothly
///    across the disc edge.
///
/// The result inside the disc is `texture_high + color_low`
/// `= (texture − blur(texture)) + blur(color)`, feathered into the original by a
/// circular falloff so the seam joins smoothly. The destination's own alpha is
/// preserved. On a flat region both fills reproduce the flat color, so the
/// blemish is restored exactly.
pub fn spot_healing(img: &mut RasterImage, cx: f32, cy: f32, radius: f32) {
    let (cx, cy, radius) = match sanitize_disc(cx, cy, radius) {
        Some(v) => v,
        None => return,
    };
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let snapshot = img.clone();

    // Mark the hard disc (d < radius) as the hole to re-synthesize.
    let (x0, y0, x1, y1) = disc_bounds(&snapshot, cx, cy, radius);
    let mut known = vec![true; w * h];
    let mut any_hole = false;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            if (dx * dx + dy * dy).sqrt() < radius {
                known[y as usize * w + x as usize] = false;
                any_hole = true;
            }
        }
    }
    if !any_hole {
        return;
    }

    // Texture (exemplar) and smooth color (diffusion), each filling the disc.
    let mut tex = snapshot.clone();
    let mut known_tex = known.clone();
    exemplar_inpaint(&mut tex, &mut known_tex);

    let mut color = snapshot.clone();
    let mut known_col = known.clone();
    diffuse_fill(&mut color, &mut known_col);

    // Frequency separation: keep the transplanted texture's high frequencies,
    // ride them on the diffusion field's smooth low frequencies for a seamless
    // (blemish-free) color match.
    let sigma = (radius * 0.5).max(1.0);
    let mut tex_low = tex.clone();
    crate::raster::filter::gaussian_blur(&mut tex_low, sigma, None);
    let mut color_low = color.clone();
    crate::raster::filter::gaussian_blur(&mut color_low, sigma, None);

    // Write-back coverage: FULL heal inside the disc, with only a thin (~1.5px)
    // feather at the very edge. (A center→edge falloff would leave most of the
    // blemish in the outer ring — the exact thing the user clicked to remove.)
    let feather = 1.5_f32.min(radius.max(0.5));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let d = (dx * dx + dy * dy).sqrt();
            if d >= radius {
                continue;
            }
            let cov = ((radius - d) / feather).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let original = snapshot.pixel(x, y);
            let t = tex.pixel(x, y);
            let tl = tex_low.pixel(x, y);
            let cl = color_low.pixel(x, y);
            let mut healed = original;
            for c in 0..3 {
                let v = t[c] as f32 - tl[c] as f32 + cl[c] as f32;
                healed[c] = v.round().clamp(0.0, 255.0) as u8;
            }
            healed[3] = original[3];
            img.set_pixel(x, y, mix(original, healed, cov));
        }
    }
}

// ── Content-aware fill ───────────────────────────────────────────────────────────

/// Content-aware fill — exemplar-based (PatchMatch-lite) inpaint of the selected
/// (`mask > 0`) region.
///
/// Rather than averaging surrounding colors (which blurs texture into a blob),
/// this runs a Criminisi-style exemplar inpaint ([`exemplar_inpaint`]): hole
/// patches are filled in priority order (strong structure/gradient + high
/// confidence first) by copying the WHOLE best-matching known patch into each
/// target patch's unknown pixels. Real texture is transplanted *and* continuous
/// linear structure (e.g. a bar) reconnects across the hole.
///
/// The search is deterministic (fixed heap order, no RNG) and scales with the
/// hole rather than the image: the fill front is maintained incrementally in a
/// priority heap (no per-iteration whole-image rescan) and each target patch is
/// matched only within a bounded local window ([`SEARCH_R`]) — so it stays fast
/// on multi-megapixel photographs and cannot hang even on large holes. An empty
/// selection (or one that selects nothing) is a no-op.
pub fn content_aware_fill(img: &mut RasterImage, mask: &Mask) {
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 || h == 0 {
        return;
    }

    // known[i] == true  ⇒ pixel is a usable source (outside the selection).
    let mut known = vec![true; w * h];
    let mut any_hole = false;
    for y in 0..img.height {
        for x in 0..img.width {
            if mask.get(x, y) > 0 {
                known[y as usize * w + x as usize] = false;
                any_hole = true;
            }
        }
    }
    if !any_hole {
        return;
    }

    // Work on a private buffer; only masked pixels are ever written back.
    let mut work = img.clone();
    exemplar_inpaint(&mut work, &mut known);

    // Commit only the originally-masked pixels.
    for y in 0..img.height {
        for x in 0..img.width {
            if mask.get(x, y) > 0 {
                img.set_pixel(x, y, work.pixel(x, y));
            }
        }
    }
}

// ── Red-eye removal ─────────────────────────────────────────────────────────────

/// Red-eye removal — within a circular region at `(cx, cy, radius)`, detect
/// strongly-red pixels (red dominant over green and blue) and desaturate them
/// toward a dark gray derived from the green/blue channels.
///
/// A pixel is treated as red-eye when `R / (G + B + 1) > 1.5` *and* red clearly
/// exceeds both other channels. Neutral / non-red pixels are left untouched.
pub fn red_eye(img: &mut RasterImage, cx: f32, cy: f32, radius: f32) {
    let (cx, cy, radius) = match sanitize_disc(cx, cy, radius) {
        Some(v) => v,
        None => return,
    };
    let snapshot = img.clone();
    let (x0, y0, x1, y1) = disc_bounds(&snapshot, cx, cy, radius);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let cov = disc_falloff(x, y, cx, cy, radius);
            if cov <= 0.0 {
                continue;
            }
            let p = snapshot.pixel(x, y);
            let r = p[0] as f32;
            let g = p[1] as f32;
            let b = p[2] as f32;
            let mx = g.max(b);
            let redness = r / (g + b + 1.0);
            if redness > 1.5 && r > mx {
                // Replace red with a darkened gray derived from the *non-red*
                // (green/blue) luminance, so the pupil reads as natural dark.
                let gray = (luma([0.0, g, b]) * 0.8).round().clamp(0.0, 255.0) as u8;
                let fixed = [gray, gray, gray, p[3]];
                img.set_pixel(x, y, mix(p, fixed, cov));
            }
        }
    }
}

// ── Dust & Scratches ─────────────────────────────────────────────────────────────

/// Dust & Scratches — a selective median despeckle. Within a `(2·radius+1)²`
/// window, a channel is only replaced by its local median when it diverges from
/// that median by more than `threshold` (0..255). Smooth gradients (small
/// deviations) are preserved exactly; isolated specks and scratches are removed.
///
/// An optional selection `sel` confines the effect. `radius == 0` is a no-op.
pub fn dust_and_scratches(img: &mut RasterImage, radius: u32, threshold: u8, sel: Option<&Mask>) {
    if radius == 0 {
        return;
    }
    let snapshot = img.clone();
    // Bound the EFFECTIVE radius hard. The per-pixel median window is O((2r+1)²),
    // so an unbounded radius (e.g. 200+) turns a tiny image into minutes of work.
    // Clamp to a sane working maximum AND to the image extent: a window can never
    // usefully reach past the (clamped-sampled) edges, so this keeps total cost
    // bounded regardless of input while leaving small radii (≤ MAX_DS_RADIUS and
    // within the image) behaving identically.
    const MAX_DS_RADIUS: u32 = 16;
    let extent = snapshot.width.max(snapshot.height).max(1);
    let r = radius.min(MAX_DS_RADIUS).min(extent) as i64;
    let thr = threshold as i32;
    let mut result = snapshot.clone();

    // Per-channel u8 histograms give the exact same median as a full sort
    // (`window[len/2]` ⇔ first value whose cumulative count exceeds `len/2`), but
    // in O(window + 256) instead of O(window·log window) — so even a fully clamped
    // window stays fast. One sampling pass fills all three channel histograms.
    let count = ((2 * r + 1) * (2 * r + 1)) as u32;
    let mid = count / 2;
    let mut hist = [[0u32; 256]; 3];

    for y in 0..snapshot.height as i64 {
        for x in 0..snapshot.width as i64 {
            for c in 0..3 {
                hist[c] = [0u32; 256];
            }
            for wy in -r..=r {
                for wx in -r..=r {
                    let p = snapshot.sample_clamped(x + wx, y + wy);
                    hist[0][p[0] as usize] += 1;
                    hist[1][p[1] as usize] += 1;
                    hist[2][p[2] as usize] += 1;
                }
            }

            let orig = snapshot.pixel(x as u32, y as u32);
            let mut out = orig;
            for c in 0..3 {
                let mut acc = 0u32;
                let mut med = 0u8;
                for v in 0..256 {
                    acc += hist[c][v];
                    if acc > mid {
                        med = v as u8;
                        break;
                    }
                }
                if (orig[c] as i32 - med as i32).abs() > thr {
                    out[c] = med;
                }
            }
            // Alpha is left as-is (out[3] == orig[3]).
            result.set_pixel(x as u32, y as u32, out);
        }
    }

    blend_result(img, &result, sel);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> RasterImage {
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = ((x + y) * 5 % 200 + 20) as u8;
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        img
    }

    // ── content_aware_fill ───────────────────────────────────────────────────────

    #[test]
    fn content_aware_fill_flat_hole_recovers_color() {
        let color = [60, 120, 180, 255];
        let mut img = RasterImage::filled(9, 9, color);
        // Punch a 3x3 hole in the middle (mark with a sentinel value).
        let mut mask = Mask::empty(9, 9);
        for y in 3..6 {
            for x in 3..6 {
                img.set_pixel(x, y, [0, 0, 0, 0]);
                mask.set(x, y, 255);
            }
        }
        content_aware_fill(&mut img, &mask);
        for y in 3..6 {
            for x in 3..6 {
                assert_eq!(img.pixel(x, y), color, "hole at {x},{y} not filled flat");
            }
        }
    }

    #[test]
    fn content_aware_fill_leaves_known_pixels_untouched() {
        let mut img = gradient(8, 8);
        let before = img.clone();
        let mut mask = Mask::empty(8, 8);
        mask.set(4, 4, 255);
        img.set_pixel(4, 4, [255, 0, 255, 255]);
        content_aware_fill(&mut img, &mask);
        // Every non-masked pixel is identical to before.
        for y in 0..8 {
            for x in 0..8 {
                if !(x == 4 && y == 4) {
                    assert_eq!(img.pixel(x, y), before.pixel(x, y));
                }
            }
        }
    }

    #[test]
    fn content_aware_fill_empty_mask_noop() {
        let mut img = gradient(5, 5);
        let before = img.clone();
        let mask = Mask::empty(5, 5);
        content_aware_fill(&mut img, &mask);
        assert_eq!(img, before);
    }

    #[test]
    fn content_aware_fill_fully_masked_does_not_hang() {
        let mut img = RasterImage::filled(4, 4, [10, 20, 30, 255]);
        let mask = Mask::full(4, 4);
        content_aware_fill(&mut img, &mask);
        // No known source anywhere → original pixels remain (no panic / hang).
        assert_eq!(img.pixel(0, 0), [10, 20, 30, 255]);
    }

    /// Build a checkerboard of `cell`-sized squares alternating `light`/`dark`.
    fn checkerboard(w: u32, h: u32, cell: u32, light: [u8; 4], dark: [u8; 4]) -> RasterImage {
        let mut img = RasterImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let on = ((x / cell) + (y / cell)).is_multiple_of(2);
                img.set_pixel(x, y, if on { light } else { dark });
            }
        }
        img
    }

    /// Mean / variance / min / max of the red channel over a rectangle.
    fn region_stats(img: &RasterImage, x0: u32, y0: u32, x1: u32, y1: u32) -> (f64, f64, f64, f64) {
        let mut v = Vec::new();
        for y in y0..y1 {
            for x in x0..x1 {
                v.push(img.pixel(x, y)[0] as f64);
            }
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64;
        let mn = v.iter().cloned().fold(f64::INFINITY, f64::min);
        let mx = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (mean, var, mn, mx)
    }

    #[test]
    fn content_aware_fill_preserves_texture_pattern() {
        // A checkerboard with a hole punched in the middle. An exemplar fill must
        // reproduce the PATTERN (both light and dark pixels), NOT a flat average.
        let light = [220, 220, 220, 255];
        let dark = [40, 40, 40, 255];
        let mut img = checkerboard(16, 16, 2, light, dark);
        let mut mask = Mask::empty(16, 16);
        for y in 5..11 {
            for x in 5..11 {
                mask.set(x, y, 255);
                img.set_pixel(x, y, [0, 0, 0, 255]); // wipe the hole
            }
        }
        content_aware_fill(&mut img, &mask);

        let (mean, var, mn, mx) = region_stats(&img, 5, 5, 11, 11);
        // A flat/averaged blob would have variance ≈ 0 and mean ≈ 130.
        assert!(
            var > 1000.0,
            "filled region must keep texture variance, got var={var} mean={mean}"
        );
        assert!(
            mn < 100.0 && mx > 160.0,
            "filled region must contain BOTH dark and light pixels, mn={mn} mx={mx}"
        );
    }

    #[test]
    fn content_aware_fill_reconnects_bar_across_hole() {
        // A solid horizontal BAR (rows 14..18) on a white field, with a square
        // hole punched over the middle of the bar. A proper exemplar inpainter
        // must reconnect the bar continuously across the hole — copying whole
        // matched patches, not a single center pixel.
        let white = [240, 240, 240, 255];
        let dark = [20, 20, 20, 255];
        let mut img = RasterImage::filled(32, 32, white);
        for y in 14..18 {
            for x in 0..32 {
                img.set_pixel(x, y, dark);
            }
        }

        // Punch a hole over the middle of the bar.
        let mut mask = Mask::empty(32, 32);
        for y in 10..22 {
            for x in 12..20 {
                mask.set(x, y, 255);
                img.set_pixel(x, y, [0, 0, 0, 0]); // wipe
            }
        }

        content_aware_fill(&mut img, &mask);

        let is_dark = |p: [u8; 4]| p[0] < 110 && p[1] < 110 && p[2] < 110;
        let is_light = |p: [u8; 4]| p[0] > 150 && p[1] > 150 && p[2] > 150;

        // For each bar row, count the hole columns recovered as dark.
        let mut rows_recovered = 0;
        for y in 14..18 {
            let mut cols_ok = 0;
            for x in 12..20 {
                if is_dark(img.pixel(x, y)) {
                    cols_ok += 1;
                }
            }
            // The bar must span (almost) the full width of the hole on this row.
            if cols_ok >= 7 {
                rows_recovered += 1;
            }
        }
        assert!(
            rows_recovered >= 3,
            "bar must reconnect across the hole: {rows_recovered}/4 rows recovered"
        );

        // Rows just outside the bar, inside the hole, must stay light (no smear).
        for x in 12..20 {
            assert!(
                is_light(img.pixel(x, 11)),
                "row 11 col {x} should be light, got {:?}",
                img.pixel(x, 11)
            );
            assert!(
                is_light(img.pixel(x, 20)),
                "row 20 col {x} should be light, got {:?}",
                img.pixel(x, 20)
            );
        }
    }

    #[test]
    fn content_aware_fill_scales_to_large_image() {
        // The whole point: cost scales with the HOLE, not the image area. The old
        // implementation rescanned the entire image every iteration (quadratic) —
        // this would crawl. A modest hole in a large image must complete promptly.
        // Sized for debug test runs; in release this is sub-millisecond.
        let mut img = checkerboard(384, 384, 3, [210, 210, 210, 255], [45, 45, 45, 255]);
        let mut mask = Mask::empty(384, 384);
        for y in 180..204 {
            for x in 180..204 {
                mask.set(x, y, 255);
                img.set_pixel(x, y, [0, 0, 0, 255]); // wipe a 24×24 hole
            }
        }
        let start = std::time::Instant::now();
        content_aware_fill(&mut img, &mask);
        let elapsed = start.elapsed();

        // Generous bound for a debug build — the assertion is that it is NOT
        // quadratic, not a tight benchmark. (Release completes in microseconds.)
        assert!(
            elapsed.as_secs_f64() < 5.0,
            "large-image content_aware_fill must scale with the hole, took {elapsed:?}"
        );
        // Dimensions intact and the hole was actually filled (not left wiped).
        assert_eq!(img.width, 384);
        assert_eq!(img.height, 384);
        let (_, var, _, _) = region_stats(&img, 180, 180, 204, 204);
        assert!(
            var > 1000.0,
            "filled hole must carry real texture, var={var}"
        );
    }

    #[test]
    fn content_aware_fill_large_image_picks_real_texture_not_blur() {
        // On a large textured field the bounded LOCAL search must still pick real
        // nearby texture (variance preserved). The old fixed 256px stride skipped
        // most candidates above 256px → a flat/blurred patch; the local window
        // does not.
        let light = [225, 225, 225, 255];
        let dark = [35, 35, 35, 255];
        let mut img = checkerboard(320, 320, 2, light, dark);
        // Reference variance of an untouched textured region.
        let (_, ref_var, _, _) = region_stats(&img, 40, 40, 70, 70);

        let mut mask = Mask::empty(320, 320);
        for y in 150..174 {
            for x in 150..174 {
                mask.set(x, y, 255);
                img.set_pixel(x, y, [0, 0, 0, 255]); // wipe a 24×24 hole
            }
        }
        content_aware_fill(&mut img, &mask);

        let (mean, var, mn, mx) = region_stats(&img, 150, 150, 174, 174);
        // Real texture transplanted: variance comparable to the source field, and
        // BOTH light and dark pixels present (a blur would collapse to ≈ mid-gray).
        assert!(
            var > ref_var * 0.5,
            "filled region must keep real texture variance (got {var}, ref {ref_var}, mean {mean})"
        );
        assert!(
            mn < 100.0 && mx > 160.0,
            "filled region must contain BOTH dark and light pixels, mn={mn} mx={mx}"
        );
    }

    // ── red_eye ──────────────────────────────────────────────────────────────────

    #[test]
    fn red_eye_reduces_red_channel() {
        let mut img = RasterImage::filled(5, 5, [255, 10, 10, 255]);
        red_eye(&mut img, 2.5, 2.5, 3.0);
        let p = img.pixel(2, 2);
        assert!(p[0] < 255, "red should drop, got {:?}", p);
    }

    #[test]
    fn red_eye_leaves_gray_unchanged() {
        let mut img = RasterImage::filled(5, 5, [128, 128, 128, 255]);
        let before = img.clone();
        red_eye(&mut img, 2.5, 2.5, 3.0);
        assert_eq!(img, before);
    }

    #[test]
    fn red_eye_radius_zero_noop() {
        let mut img = RasterImage::filled(4, 4, [255, 0, 0, 255]);
        let before = img.clone();
        red_eye(&mut img, 2.0, 2.0, 0.0);
        assert_eq!(img, before);
    }

    // ── dust_and_scratches ───────────────────────────────────────────────────────

    #[test]
    fn dust_removes_single_outlier() {
        let mut img = RasterImage::filled(5, 5, [40, 40, 40, 255]);
        img.set_pixel(2, 2, [240, 240, 240, 255]);
        dust_and_scratches(&mut img, 1, 30, None);
        assert_eq!(img.pixel(2, 2), [40, 40, 40, 255]);
    }

    #[test]
    fn dust_preserves_smooth_gradient() {
        // A horizontal ramp: each pixel differs only slightly from its median.
        let mut img = RasterImage::new(7, 1);
        for x in 0..7 {
            let v = (x * 10) as u8;
            img.set_pixel(x, 0, [v, v, v, 255]);
        }
        let before = img.clone();
        dust_and_scratches(&mut img, 1, 30, None);
        assert_eq!(img, before, "gentle gradient must be preserved");
    }

    #[test]
    fn dust_radius_zero_noop() {
        let mut img = gradient(5, 5);
        let before = img.clone();
        dust_and_scratches(&mut img, 0, 10, None);
        assert_eq!(img, before);
    }

    #[test]
    fn dust_respects_selection() {
        let mut img = RasterImage::filled(5, 5, [40, 40, 40, 255]);
        img.set_pixel(2, 2, [240, 240, 240, 255]);
        // Select only a region that excludes the outlier → it survives.
        let mut sel = Mask::empty(5, 5);
        sel.set(0, 0, 255);
        dust_and_scratches(&mut img, 1, 30, Some(&sel));
        assert_eq!(img.pixel(2, 2), [240, 240, 240, 255]);
    }

    #[test]
    fn dust_and_scratches_large_radius_is_fast_and_bounded() {
        // A huge radius must NOT explode into an O((2r+1)²) per-pixel median over a
        // 511×511 window. The effective radius is clamped, so this completes near-
        // instantly and never panics.
        let mut img = gradient(128, 128);
        img.set_pixel(64, 64, [250, 250, 250, 255]); // a speck to despeckle
        let start = std::time::Instant::now();
        dust_and_scratches(&mut img, 255, 30, None);
        let elapsed = start.elapsed();
        // Generous bound — the point is it finishes quickly, not in minutes.
        assert!(
            elapsed.as_secs_f64() < 5.0,
            "large-radius dust_and_scratches must be bounded, took {elapsed:?}"
        );
        // Dimensions intact, no panic.
        assert_eq!(img.width, 128);
        assert_eq!(img.height, 128);
    }

    // ── spot_healing ─────────────────────────────────────────────────────────────

    #[test]
    fn spot_healing_restores_flat_color() {
        let color = [90, 140, 200, 255];
        let mut img = RasterImage::filled(11, 11, color);
        img.set_pixel(5, 5, [255, 0, 0, 255]); // off-color blemish
        spot_healing(&mut img, 5.5, 5.5, 2.0);
        assert_eq!(img.pixel(5, 5), color);
    }

    #[test]
    fn spot_healing_removes_blemish_across_whole_disc_not_just_center() {
        // A large flat-blue field with a solid yellow blemish disc. After a spot
        // heal covering it, NO pixel inside the disc (including the outer ring,
        // ~0.8·radius from center) may retain the blemish color.
        let bg = [80, 130, 210, 255];
        let mut img = RasterImage::filled(48, 48, bg);
        for y in 0..48 {
            for x in 0..48 {
                let dx = x as f32 - 24.0;
                let dy = y as f32 - 24.0;
                if (dx * dx + dy * dy).sqrt() < 10.0 {
                    img.set_pixel(x, y, [255, 255, 0, 255]); // yellow blemish
                }
            }
        }
        spot_healing(&mut img, 24.0, 24.0, 11.0);
        // Ring pixels ~8px from center (well inside the old blemish/feather ring).
        for (px, py) in [(24, 16), (32, 24), (24, 32), (16, 24)] {
            let p = img.pixel(px, py);
            assert!(
                !(p[0] > 200 && p[1] > 200 && p[2] < 80),
                "residual yellow blemish in heal ring at ({px},{py}): {p:?}"
            );
        }
    }

    #[test]
    fn spot_healing_radius_zero_noop() {
        let mut img = gradient(6, 6);
        let before = img.clone();
        spot_healing(&mut img, 3.0, 3.0, 0.0);
        assert_eq!(img, before);
    }

    #[test]
    fn spot_healing_tiny_image_no_panic() {
        let mut img = RasterImage::filled(1, 1, [10, 20, 30, 255]);
        spot_healing(&mut img, 0.5, 0.5, 2.0);
        assert_eq!(img.width, 1);
        assert_eq!(img.height, 1);
    }

    #[test]
    fn spot_healing_preserves_texture_variance() {
        // Checkerboard background with a flat off-color blemish stamped on top.
        // Healing must transplant texture back (variance restored), not leave a
        // flat patch or the red blemish.
        let light = [210, 210, 210, 255];
        let dark = [50, 50, 50, 255];
        let mut img = checkerboard(24, 24, 2, light, dark);
        for y in 10..14 {
            for x in 10..14 {
                img.set_pixel(x, y, [255, 0, 0, 255]); // flat red blemish
            }
        }
        spot_healing(&mut img, 12.0, 12.0, 3.0);

        // No pure-red blemish pixels survive.
        for y in 10..14 {
            for x in 10..14 {
                let p = img.pixel(x, y);
                assert!(
                    !(p[0] > 200 && p[1] < 70 && p[2] < 70),
                    "blemish remained at {x},{y}: {p:?}"
                );
            }
        }
        // The healed region retains texture variance rather than a flat fill.
        let (mean, var, _, _) = region_stats(&img, 10, 10, 14, 14);
        assert!(
            var > 500.0,
            "healed region must restore texture variance, got var={var} mean={mean}"
        );
    }

    // ── healing_brush ────────────────────────────────────────────────────────────

    #[test]
    fn healing_brush_flat_stays_flat() {
        let color = [70, 110, 150, 255];
        let mut img = RasterImage::filled(12, 12, color);
        healing_brush(&mut img, 6.0, 6.0, 3.0, 2, 2);
        // On a flat image source & dest low-freq match → patch is identical.
        assert_eq!(img.pixel(6, 6), color);
    }

    #[test]
    fn healing_brush_blends_texture_without_panic() {
        // Left half dark with texture, right half bright; heal the right using
        // the left as source. Result should move toward the left's texture but
        // keep the right's brightness.
        let mut img = RasterImage::new(20, 8);
        for y in 0..8 {
            for x in 0..20 {
                let base = if x < 10 { 60 } else { 200 };
                let tex = if (x + y) % 2 == 0 { 15 } else { 0 };
                let v = (base + tex) as u8;
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let before = img.clone();
        // Heal a disc on the right side, sourcing texture from the left. An odd
        // offset flips the checkerboard parity so real high-freq is transplanted.
        healing_brush(&mut img, 15.0, 4.0, 3.0, -9, 0);
        // Center pixel changed (texture introduced) but is still in the bright range.
        let p = img.pixel(15, 4);
        assert_ne!(p, before.pixel(15, 4));
        assert!(
            p[0] > 150,
            "should retain destination brightness, got {:?}",
            p
        );
        assert_eq!(img.width, 20);
        assert_eq!(img.height, 8);
    }

    #[test]
    fn healing_brush_radius_zero_noop() {
        let mut img = gradient(6, 6);
        let before = img.clone();
        healing_brush(&mut img, 3.0, 3.0, 0.0, 1, 1);
        assert_eq!(img, before);
    }

    #[test]
    fn healing_brush_tiny_image_no_panic() {
        let mut img = RasterImage::filled(1, 1, [5, 5, 5, 255]);
        healing_brush(&mut img, 0.5, 0.5, 2.0, 3, 3);
        assert_eq!(img.len(), 1);
    }

    // ── Panic-proofing (non-finite / huge inputs) ────────────────────────────────

    #[test]
    fn repair_ops_no_panic_on_nonfinite() {
        let mut img = RasterImage::filled(8, 8, [100, 110, 120, 255]);
        let before = img.clone();
        let nan = f32::NAN;
        let inf = f32::INFINITY;
        let ninf = f32::NEG_INFINITY;

        // Non-finite center or radius, and non-positive radius, must all no-op.
        spot_healing(&mut img, nan, 2.0, 3.0);
        spot_healing(&mut img, 2.0, inf, 3.0);
        spot_healing(&mut img, 2.0, 2.0, nan);
        spot_healing(&mut img, 2.0, 2.0, inf);
        spot_healing(&mut img, 2.0, 2.0, 0.0);
        spot_healing(&mut img, 2.0, 2.0, -5.0);

        healing_brush(&mut img, nan, 2.0, 3.0, 1, 1);
        healing_brush(&mut img, 2.0, 2.0, inf, 1, 1);
        healing_brush(&mut img, 2.0, 2.0, nan, 1, 1);
        healing_brush(&mut img, 2.0, 2.0, ninf, 1, 1);

        red_eye(&mut img, nan, inf, 3.0);
        red_eye(&mut img, 2.0, 2.0, inf);
        red_eye(&mut img, 2.0, 2.0, nan);

        assert_eq!(
            img, before,
            "non-finite / non-positive inputs must be a no-op"
        );
    }

    #[test]
    fn repair_ops_clamp_huge_radius_without_panic() {
        // A runaway radius must be clamped (so the Gaussian kernel etc. stays
        // bounded) and run without panic or hang.
        let mut img = gradient(8, 8);
        spot_healing(&mut img, 4.0, 4.0, 1.0e30);
        healing_brush(&mut img, 4.0, 4.0, 1.0e30, 1, 1);
        red_eye(&mut img, 4.0, 4.0, 1.0e30);
        assert_eq!(img.width, 8);
        assert_eq!(img.height, 8);
    }

    // Keep `luma` import meaningfully exercised so the module's public surface
    // is consistent with siblings even though repair ops are RGB-domain.
    #[test]
    fn luma_helper_is_available() {
        let l = luma([1.0, 1.0, 1.0]);
        assert!((l - 1.0).abs() < 1e-6);
    }
}
