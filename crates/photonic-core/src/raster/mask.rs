//! `Mask` — an 8-bit coverage buffer used both as a transient **selection**
//! (marquee, lasso, magic wand) and as a persisted **layer mask**.
//!
//! `0` = fully masked / deselected, `255` = fully selected. Intermediate values
//! give partial coverage (feathered edges, anti-aliasing), exactly like a
//! Photoshop selection or layer mask.

use super::image::{checked_raster_pixel_count, luma, RasterImage, MAX_PERSISTED_RASTER_PIXELS};
use serde::{
    de::{self, SeqAccess, Visitor},
    Deserialize, Deserializer, Serialize,
};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    /// Coverage values, row-major. `len == width * height`.
    pub data: Vec<u8>,
}

#[derive(Deserialize)]
struct MaskRepr {
    width: u32,
    height: u32,
    #[serde(deserialize_with = "deserialize_bounded_data")]
    data: Vec<u8>,
}

fn deserialize_bounded_data<'de, D>(d: D) -> Result<Vec<u8>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoundedMaskData;

    impl<'de> Visitor<'de> for BoundedMaskData {
        type Value = Vec<u8>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a mask coverage byte array")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let size_hint = seq.size_hint().unwrap_or(0);
            if size_hint > MAX_PERSISTED_RASTER_PIXELS {
                return Err(de::Error::custom(format!(
                    "mask data exceeds maximum of {} pixels",
                    MAX_PERSISTED_RASTER_PIXELS
                )));
            }
            let mut data = Vec::with_capacity(size_hint);
            while let Some(value) = seq.next_element::<u8>()? {
                if data.len() >= MAX_PERSISTED_RASTER_PIXELS {
                    return Err(de::Error::custom(format!(
                        "mask data exceeds maximum of {} pixels",
                        MAX_PERSISTED_RASTER_PIXELS
                    )));
                }
                data.push(value);
            }
            Ok(data)
        }
    }

    d.deserialize_seq(BoundedMaskData)
}

impl<'de> Deserialize<'de> for Mask {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let repr = MaskRepr::deserialize(d)?;
        if repr.width == 0 || repr.height == 0 {
            return Err(serde::de::Error::custom("mask dimensions must be non-zero"));
        }
        let expected = checked_raster_pixel_count(repr.width, repr.height)
            .map_err(serde::de::Error::custom)?;
        if repr.data.len() != expected {
            return Err(serde::de::Error::custom(format!(
                "mask data length {} does not match {}x{} = {}",
                repr.data.len(),
                repr.width,
                repr.height,
                expected
            )));
        }
        Ok(Self {
            width: repr.width,
            height: repr.height,
            data: repr.data,
        })
    }
}

impl Mask {
    /// A fully *deselected* (all-zero) mask.
    pub fn empty(width: u32, height: u32) -> Self {
        let w = width.max(1);
        let h = height.max(1);
        Self {
            width: w,
            height: h,
            data: vec![0u8; (w as usize) * (h as usize)],
        }
    }

    /// A fully *selected* (all-255) mask.
    pub fn full(width: u32, height: u32) -> Self {
        let w = width.max(1);
        let h = height.max(1);
        Self {
            width: w,
            height: h,
            data: vec![255u8; (w as usize) * (h as usize)],
        }
    }

    #[inline]
    pub fn index(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.width as usize) + (x as usize)
    }

    /// Coverage at a pixel, 0..=255. Out-of-bounds reads as 0.
    #[inline]
    pub fn get(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.data[self.index(x, y)]
    }

    /// Coverage as 0..1.
    #[inline]
    pub fn coverage(&self, x: u32, y: u32) -> f32 {
        self.get(x, y) as f32 / 255.0
    }

    #[inline]
    pub fn set(&mut self, x: u32, y: u32, v: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = self.index(x, y);
        self.data[i] = v;
    }

    // ── Shape builders ──────────────────────────────────────────────────────────

    /// Rectangular marquee. Coordinates are clamped to the canvas.
    ///
    /// Intentionally **not** anti-aliased: edges are crisp and pixel-aligned to
    /// match Photoshop's rectangular marquee, which snaps to integer pixel
    /// boundaries rather than feathering. (Elliptical/lasso selections below are
    /// anti-aliased; the rectangular marquee deliberately is not.)
    pub fn rect(width: u32, height: u32, x: i64, y: i64, w: i64, h: i64) -> Self {
        let mut m = Mask::empty(width, height);
        let x0 = x.max(0);
        let y0 = y.max(0);
        // `saturating_add` so a huge x/w (or y/h) can't overflow i64 and wrap.
        let x1 = x.saturating_add(w).min(width as i64);
        let y1 = y.saturating_add(h).min(height as i64);
        for yy in y0..y1 {
            for xx in x0..x1 {
                m.set(xx as u32, yy as u32, 255);
            }
        }
        m
    }

    /// Elliptical marquee inscribed in the given rect, with anti-aliased edge.
    ///
    /// Coverage is computed by **4×4 (16-sample) supersampling**: each pixel is
    /// split into a 4×4 grid of sub-samples and we count how many fall inside the
    /// analytic ellipse `((sx-cx)/rx)² + ((sy-cy)/ry)² <= 1`. Interior pixels get
    /// all 16 samples → 255 (fully saturated); boundary pixels get smooth partial
    /// coverage. This is geometrically exact even for tiny radii, where the old
    /// `(1-d)*min(rx,ry)` distance approximation under-filled the interior (a 2×2
    /// ellipse peaked at ~29%/74 and never saturated).
    pub fn ellipse(width: u32, height: u32, x: f64, y: f64, w: f64, h: f64) -> Self {
        let mut m = Mask::empty(width, height);
        // Reject non-finite geometry up front; `NaN <= 0.0` is false, so without
        // this an NaN would slip past the size check and poison every pixel.
        if !(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite()) {
            return m;
        }
        if w <= 0.0 || h <= 0.0 {
            return m;
        }
        let cx = x + w / 2.0;
        let cy = y + h / 2.0;
        let rx = w / 2.0;
        let ry = h / 2.0;
        for yy in 0..height {
            for xx in 0..width {
                let mut inside = 0u32;
                for sj in 0..4 {
                    for si in 0..4 {
                        // Sub-sample centre within the 4×4 grid: offsets 0.125,
                        // 0.375, 0.625, 0.875 across the pixel in each axis.
                        let sx = xx as f64 + (si as f64 + 0.5) / 4.0;
                        let sy = yy as f64 + (sj as f64 + 0.5) / 4.0;
                        let nx = (sx - cx) / rx;
                        let ny = (sy - cy) / ry;
                        if nx * nx + ny * ny <= 1.0 {
                            inside += 1;
                        }
                    }
                }
                // inside ∈ 0..=16 → coverage 0..=255 (16 → exactly 255).
                m.set(xx, yy, (inside * 255 / 16) as u8);
            }
        }
        m
    }

    /// Polygon (lasso) selection, anti-aliased via 4×4 (16-sample) supersampling.
    ///
    /// Each pixel is split into a 4×4 grid of sub-samples; each sub-sample is
    /// tested for containment using the even-odd (ray-casting) rule, and coverage
    /// = `inside / 16 * 255`. Pixels fully inside the polygon get all 16 → 255;
    /// the boundary gets smooth partial coverage instead of a hard binary edge.
    ///
    /// Bounded and deterministic: we only iterate the polygon's clamped bounding
    /// box, so cost is proportional to the selection area, not the whole canvas.
    ///
    /// Vertices with non-finite (`NaN`/`±inf`) coordinates are dropped first so a
    /// stray coordinate can neither poison the crossing math nor panic.
    pub fn polygon(width: u32, height: u32, points: &[(f64, f64)]) -> Self {
        let mut m = Mask::empty(width, height);
        // Sanitize: keep only finite vertices.
        let points: Vec<(f64, f64)> = points
            .iter()
            .copied()
            .filter(|(x, y)| x.is_finite() && y.is_finite())
            .collect();
        if points.len() < 3 {
            return m;
        }
        // Clamped bounding box → bounded fast path (pixels outside stay 0).
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for &(px, py) in &points {
            min_x = min_x.min(px);
            min_y = min_y.min(py);
            max_x = max_x.max(px);
            max_y = max_y.max(py);
        }
        // Saturating `as i64` casts keep huge coordinates from panicking; an
        // empty range simply produces an empty selection.
        let x0 = min_x.floor().max(0.0) as i64;
        let x1 = max_x.ceil().min(width as f64) as i64;
        let y0 = min_y.floor().max(0.0) as i64;
        let y1 = max_y.ceil().min(height as f64) as i64;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let mut inside = 0u32;
                for sj in 0..4 {
                    for si in 0..4 {
                        let sx = xx as f64 + (si as f64 + 0.5) / 4.0;
                        let sy = yy as f64 + (sj as f64 + 0.5) / 4.0;
                        if point_in_polygon(sx, sy, &points) {
                            inside += 1;
                        }
                    }
                }
                if inside > 0 {
                    // inside ∈ 1..=16 → coverage; 16 → exactly 255.
                    m.set(xx as u32, yy as u32, (inside * 255 / 16) as u8);
                }
            }
        }
        m
    }

    /// Magic-wand: flood fill from a seed, selecting pixels whose color is within
    /// `tolerance` (0..1) of the seed color. 4-connected.
    pub fn magic_wand(img: &RasterImage, seed_x: u32, seed_y: u32, tolerance: f32) -> Self {
        let mut m = Mask::empty(img.width, img.height);
        if seed_x >= img.width || seed_y >= img.height {
            return m;
        }
        let seed = img.pixel(seed_x, seed_y);
        // Non-finite tolerance → treat as 0 (exact match) rather than letting NaN
        // propagate into the `diff > tol` comparison.
        let tolerance = if tolerance.is_finite() {
            tolerance
        } else {
            0.0
        };
        let tol = (tolerance.clamp(0.0, 1.0) * 255.0) * 3.0; // sum over RGB
        let mut stack = vec![(seed_x, seed_y)];
        while let Some((x, y)) = stack.pop() {
            if m.get(x, y) != 0 {
                continue;
            }
            let p = img.pixel(x, y);
            let diff = (p[0] as f32 - seed[0] as f32).abs()
                + (p[1] as f32 - seed[1] as f32).abs()
                + (p[2] as f32 - seed[2] as f32).abs();
            if diff > tol {
                continue;
            }
            m.set(x, y, 255);
            if x > 0 {
                stack.push((x - 1, y));
            }
            if x + 1 < img.width {
                stack.push((x + 1, y));
            }
            if y > 0 {
                stack.push((x, y - 1));
            }
            if y + 1 < img.height {
                stack.push((x, y + 1));
            }
        }
        // Anti-alias the 1px edge while keeping the interior fully saturated, so
        // the wand (and the paint bucket that composites this mask by coverage)
        // no longer produces hard binary edges.
        m.data = antialias_edge(&m.data, m.width, m.height);
        m
    }

    /// Select-by-color-range: every pixel within `tolerance` of `target` (global,
    /// not contiguous).
    pub fn color_range(img: &RasterImage, target: [u8; 4], tolerance: f32) -> Self {
        let mut m = Mask::empty(img.width, img.height);
        let tolerance = if tolerance.is_finite() {
            tolerance
        } else {
            0.0
        };
        let tol = (tolerance.clamp(0.0, 1.0) * 255.0) * 3.0;
        for y in 0..img.height {
            for x in 0..img.width {
                let p = img.pixel(x, y);
                let diff = (p[0] as f32 - target[0] as f32).abs()
                    + (p[1] as f32 - target[1] as f32).abs()
                    + (p[2] as f32 - target[2] as f32).abs();
                if diff <= tol {
                    m.set(x, y, 255);
                }
            }
        }
        // Same edge anti-aliasing as the magic wand: soft 1px fringe, saturated
        // interior.
        m.data = antialias_edge(&m.data, m.width, m.height);
        m
    }

    /// Build a mask from an image's luminance (for "load luminance as selection").
    pub fn from_luminance(img: &RasterImage) -> Self {
        let mut m = Mask::empty(img.width, img.height);
        for y in 0..img.height {
            for x in 0..img.width {
                let p = img.pixel(x, y);
                let l = luma([
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]);
                m.set(x, y, (l * 255.0).round() as u8);
            }
        }
        m
    }

    // ── Editing ──────────────────────────────────────────────────────────────────

    /// Invert the selection (255 - v).
    pub fn invert(&mut self) {
        for v in self.data.iter_mut() {
            *v = 255 - *v;
        }
    }

    /// Boolean add (union, max).
    pub fn union(&mut self, other: &Mask) {
        self.combine(other, |a, b| a.max(b));
    }

    /// Boolean subtract (a AND NOT b).
    pub fn subtract(&mut self, other: &Mask) {
        self.combine(other, |a, b| a.saturating_sub(b));
    }

    /// Boolean intersect (min).
    pub fn intersect(&mut self, other: &Mask) {
        self.combine(other, |a, b| a.min(b));
    }

    fn combine(&mut self, other: &Mask, f: impl Fn(u8, u8) -> u8) {
        if other.width != self.width || other.height != self.height {
            return;
        }
        for (a, b) in self.data.iter_mut().zip(other.data.iter()) {
            *a = f(*a, *b);
        }
    }

    /// Largest feather/blur radius we will honour. Anything beyond this is both
    /// visually pointless and a runtime/memory hazard, so it is clamped.
    const MAX_FEATHER_RADIUS: f32 = 1024.0;

    /// Feather (soften) the selection edge with a gaussian blur of `radius` px.
    ///
    /// Non-finite (`NaN`/`±inf`) or non-positive radii are a no-op. Huge radii
    /// are clamped to [`Mask::MAX_FEATHER_RADIUS`] so the blur cannot panic,
    /// hang, or exhaust memory.
    pub fn feather(&mut self, radius: f32) {
        // Reject NaN / ±inf up front — these would propagate into the gaussian
        // kernel sizing and panic or hang.
        if !radius.is_finite() || radius <= 0.0 {
            return;
        }
        let radius = radius.min(Self::MAX_FEATHER_RADIUS);
        let blurred =
            super::filter::gaussian_blur_gray(&self.data, self.width, self.height, radius);
        self.data = blurred;
    }

    /// Grow (expand) the selection by `px` pixels (max filter).
    pub fn grow(&mut self, px: u32) {
        self.morph(px, true);
    }

    /// Contract (shrink) the selection by `px` pixels (min filter).
    pub fn contract(&mut self, px: u32) {
        self.morph(px, false);
    }

    fn morph(&mut self, px: u32, dilate: bool) {
        // Growing/shrinking by more than the longest canvas dimension is a no-op
        // beyond saturation, so cap iterations to avoid a pathological hang on a
        // huge `px` (e.g. `u32::MAX`).
        let max_useful = self.width.max(self.height);
        let px = px.min(max_useful);
        for _ in 0..px {
            let src = self.data.clone();
            for y in 0..self.height {
                for x in 0..self.width {
                    let mut acc = src[self.index(x, y)];
                    let consider = |xx: i64, yy: i64, acc: &mut u8| {
                        if xx >= 0
                            && yy >= 0
                            && (xx as u32) < self.width
                            && (yy as u32) < self.height
                        {
                            let v = src[(yy as usize) * (self.width as usize) + xx as usize];
                            if dilate {
                                *acc = (*acc).max(v);
                            } else {
                                *acc = (*acc).min(v);
                            }
                        } else if !dilate {
                            *acc = 0;
                        }
                    };
                    consider(x as i64 - 1, y as i64, &mut acc);
                    consider(x as i64 + 1, y as i64, &mut acc);
                    consider(x as i64, y as i64 - 1, &mut acc);
                    consider(x as i64, y as i64 + 1, &mut acc);
                    let i = self.index(x, y);
                    self.data[i] = acc;
                }
            }
        }
    }

    /// True if nothing is selected.
    pub fn is_empty_selection(&self) -> bool {
        self.data.iter().all(|&v| v == 0)
    }

    /// Crop to the pixel rect `(x, y, w, h)`, clamped to the mask bounds —
    /// the mask counterpart of `geometry::crop`, so a layer mask can be cut
    /// down in lockstep with its image. Out-of-range regions clamp to at
    /// least 1×1 (matching `geometry::crop`).
    pub fn crop(&self, x: i64, y: i64, w: u32, h: u32) -> Mask {
        let x0 = x.clamp(0, self.width as i64);
        let y0 = y.clamp(0, self.height as i64);
        let x1 = (x + w as i64).clamp(0, self.width as i64);
        let y1 = (y + h as i64).clamp(0, self.height as i64);
        let cw = (x1 - x0).max(1) as u32;
        let ch = (y1 - y0).max(1) as u32;
        let mut out = Mask::empty(cw, ch);
        for yy in 0..ch {
            for xx in 0..cw {
                out.set(xx, yy, self.get(x0 as u32 + xx, y0 as u32 + yy));
            }
        }
        out
    }
}

/// Even-odd (ray-casting) point-in-polygon test. Callers must pass only finite
/// vertices; under that contract the denominator `(yj - yi)` is non-zero whenever
/// it is used (the `(yi > py) != (yj > py)` guard implies `yi != yj`), so this
/// neither divides by zero nor produces a non-finite result.
fn point_in_polygon(px: f64, py: f64, points: &[(f64, f64)]) -> bool {
    let n = points.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Anti-alias the **edge** of a binary (0/255) selection while keeping the
/// interior fully saturated.
///
/// Every already-selected pixel is preserved at 255 (so a fully-surrounded pixel
/// never erodes, and existing "interior == 255" assertions hold); each *unselected*
/// pixel instead takes the 3×3 box average of the binary mask (edge-replicated at
/// the canvas border). The result is a soft 1px outer fringe — pixels touching the
/// selection edge get intermediate coverage, pixels far outside stay 0 — which is
/// equivalent to `max(box_average, binary)`.
fn antialias_edge(binary: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as i64;
    let h = height as i64;
    if w <= 0 || h <= 0 {
        return binary.to_vec();
    }
    let mut out = vec![0u8; binary.len()];
    for y in 0..h {
        for x in 0..w {
            let idx = (y * w + x) as usize;
            // Selected pixels stay saturated — never erode the interior.
            if binary[idx] != 0 {
                out[idx] = 255;
                continue;
            }
            let mut sum = 0u32;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let nx = (x + dx).clamp(0, w - 1);
                    let ny = (y + dy).clamp(0, h - 1);
                    sum += binary[(ny * w + nx) as usize] as u32;
                }
            }
            // 9 taps; all-selected → 2295/9 == 255 exactly.
            out[idx] = (sum / 9) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_rejects_invalid_dimensions_and_data_lengths() {
        for value in [
            serde_json::json!({"width": 0, "height": 1, "data": []}),
            serde_json::json!({
                "width": u32::MAX,
                "height": u32::MAX,
                "data": []
            }),
            serde_json::json!({"width": 2, "height": 2, "data": [0, 0, 0]}),
        ] {
            assert!(
                serde_json::from_value::<Mask>(value).is_err(),
                "malformed mask should be rejected"
            );
        }
    }

    #[test]
    fn serde_rejects_oversized_pixel_count() {
        let value = serde_json::json!({
            "width": (MAX_PERSISTED_RASTER_PIXELS + 1) as u64,
            "height": 1,
            "data": []
        });

        let err = serde_json::from_value::<Mask>(value).unwrap_err();
        assert!(err.to_string().contains("exceed maximum"));
    }

    #[test]
    fn serde_roundtrip_preserves_valid_mask() {
        let mut mask = Mask::empty(2, 2);
        mask.set(1, 0, 255);
        let json = serde_json::to_string(&mask).unwrap();
        let back: Mask = serde_json::from_str(&json).unwrap();
        assert_eq!(mask, back);
    }

    #[test]
    fn rect_selects_region() {
        let m = Mask::rect(10, 10, 2, 2, 4, 4);
        assert_eq!(m.get(0, 0), 0);
        assert_eq!(m.get(3, 3), 255);
        assert_eq!(m.get(5, 5), 255);
        assert_eq!(m.get(6, 6), 0);
    }

    #[test]
    fn invert_flips() {
        let mut m = Mask::rect(4, 4, 0, 0, 2, 4);
        m.invert();
        assert_eq!(m.get(0, 0), 0);
        assert_eq!(m.get(3, 0), 255);
    }

    #[test]
    fn boolean_ops() {
        let mut a = Mask::rect(8, 8, 0, 0, 4, 8);
        let b = Mask::rect(8, 8, 2, 0, 4, 8);
        let mut inter = a.clone();
        inter.intersect(&b);
        assert_eq!(inter.get(3, 0), 255);
        assert_eq!(inter.get(0, 0), 0);
        a.subtract(&b);
        assert_eq!(a.get(0, 0), 255);
        assert_eq!(a.get(3, 0), 0);
    }

    #[test]
    fn magic_wand_contiguous() {
        let mut img = RasterImage::filled(5, 5, [255, 255, 255, 255]);
        // a red square in the corner
        for y in 0..2 {
            for x in 0..2 {
                img.set_pixel(x, y, [255, 0, 0, 255]);
            }
        }
        let m = Mask::magic_wand(&img, 0, 0, 0.1);
        assert_eq!(m.get(0, 0), 255);
        assert_eq!(m.get(1, 1), 255);
        assert_eq!(m.get(4, 4), 0);
    }

    #[test]
    fn grow_expands() {
        let mut m = Mask::empty(7, 7);
        m.set(3, 3, 255);
        m.grow(1);
        assert_eq!(m.get(3, 3), 255);
        assert_eq!(m.get(2, 3), 255);
        assert_eq!(m.get(4, 3), 255);
        assert_eq!(m.get(2, 2), 0); // diagonal not touched by 4-neighborhood
    }

    // ── Hardening: no public op may panic for any input ───────────────────────

    #[test]
    fn feather_nonfinite_radius_is_noop() {
        let mut base = Mask::rect(8, 8, 1, 1, 4, 4);
        let before = base.clone();
        base.feather(f32::NAN);
        assert_eq!(base, before, "NaN radius must be a no-op");
        base.feather(f32::INFINITY);
        assert_eq!(base, before, "+inf radius must be a no-op");
        base.feather(f32::NEG_INFINITY);
        assert_eq!(base, before, "-inf radius must be a no-op");
        base.feather(0.0);
        assert_eq!(base, before, "zero radius must be a no-op");
    }

    #[test]
    fn feather_huge_radius_clamped_no_panic() {
        let mut m = Mask::rect(16, 16, 2, 2, 8, 8);
        // Would panic/hang if the radius were honoured verbatim.
        m.feather(1.0e30);
        // Just needs to complete without panicking; mask stays valid length.
        assert_eq!(m.data.len(), 16 * 16);
    }

    #[test]
    fn polygon_with_nan_vertex_does_not_panic() {
        // A clean triangle plus one poisoned vertex.
        let pts = [
            (1.0, 1.0),
            (8.0, 1.0),
            (f64::NAN, 5.0),
            (4.0, 8.0),
            (1.0, 8.0),
        ];
        let m = Mask::polygon(10, 10, &pts);
        // Produces a sane mask: something selected, nothing out of bounds.
        assert_eq!(m.data.len(), 100);
        assert!(m.data.contains(&255), "expected some coverage");
    }

    #[test]
    fn polygon_all_infinite_vertices_empty_no_panic() {
        let pts = [
            (f64::INFINITY, 1.0),
            (2.0, f64::NEG_INFINITY),
            (f64::NAN, f64::NAN),
        ];
        let m = Mask::polygon(8, 8, &pts);
        assert!(m.is_empty_selection());
    }

    #[test]
    fn ellipse_nonfinite_coords_no_panic() {
        let m = Mask::ellipse(8, 8, f64::NAN, 0.0, 4.0, 4.0);
        assert!(m.is_empty_selection());
        let m = Mask::ellipse(8, 8, 0.0, 0.0, f64::INFINITY, 4.0);
        assert!(m.is_empty_selection());
        // Sane ellipse still works.
        let m = Mask::ellipse(8, 8, 0.0, 0.0, 8.0, 8.0);
        assert!(!m.is_empty_selection());
    }

    #[test]
    fn ellipse_huge_coords_no_panic() {
        let m = Mask::ellipse(8, 8, -1.0e18, -1.0e18, 1.0e30, 1.0e30);
        assert_eq!(m.data.len(), 64);
    }

    #[test]
    fn rect_huge_coords_no_panic() {
        // x + w would overflow i64 without saturating arithmetic.
        let m = Mask::rect(8, 8, i64::MAX - 1, 0, i64::MAX, 4);
        assert_eq!(m.data.len(), 64);
        // Negative-extreme origin clamps to an empty (but valid) selection.
        let m = Mask::rect(8, 8, i64::MIN, i64::MIN, i64::MAX, i64::MAX);
        assert_eq!(m.data.len(), 64);
        // Huge width/height from a valid origin saturates to "select all".
        let m = Mask::rect(8, 8, 0, 0, i64::MAX, i64::MAX);
        assert_eq!(m.data.len(), 64);
        assert!(!m.is_empty_selection());
    }

    #[test]
    fn magic_wand_out_of_range_seed_empty_no_panic() {
        let img = RasterImage::filled(5, 5, [10, 20, 30, 255]);
        let m = Mask::magic_wand(&img, 99, 99, 0.5);
        assert!(m.is_empty_selection());
        // Non-finite tolerance must not panic either.
        let m = Mask::magic_wand(&img, 0, 0, f32::NAN);
        assert_eq!(m.data.len(), 25);
    }

    #[test]
    fn color_range_nonfinite_tolerance_no_panic() {
        let img = RasterImage::filled(4, 4, [255, 255, 255, 255]);
        let m = Mask::color_range(&img, [255, 255, 255, 255], f32::INFINITY);
        assert_eq!(m.data.len(), 16);
    }

    // ── Anti-aliasing / coverage correctness ─────────────────────────────────

    #[test]
    fn ellipse_small_radius_interior_saturates() {
        // 2×2 ellipse centred on a pixel centre (cx=cy=2.5): a unit pixel fits
        // inside a radius-1 circle, so the central pixel is fully covered → 255.
        // The old `(1-d)*min(rx,ry)` formula peaked at ~74/255 here.
        let m = Mask::ellipse(5, 5, 1.5, 1.5, 2.0, 2.0);
        assert_eq!(
            m.get(2, 2),
            255,
            "2×2 ellipse interior pixel must saturate (was ~74/255)"
        );

        // 4×4 ellipse (cx=cy=4): the pixels straddling the centre are fully
        // enclosed and must saturate too.
        let m = Mask::ellipse(8, 8, 2.0, 2.0, 4.0, 4.0);
        assert_eq!(m.get(3, 3), 255, "4×4 ellipse interior must saturate");
        assert_eq!(m.get(4, 4), 255, "4×4 ellipse interior must saturate");
    }

    #[test]
    fn ellipse_edge_is_antialiased() {
        // A larger ellipse must have boundary pixels with partial coverage rather
        // than a hard 0/255 edge.
        let m = Mask::ellipse(16, 16, 2.0, 2.0, 12.0, 12.0);
        assert!(!m.is_empty_selection(), "ellipse should select something");
        assert!(
            m.data.contains(&255),
            "interior should be fully saturated somewhere"
        );
        assert!(
            m.data.iter().any(|&v| v > 0 && v < 255),
            "boundary should have anti-aliased (partial) coverage"
        );
    }

    #[test]
    fn polygon_edge_is_antialiased() {
        // A triangle with a diagonal edge: the slanted edge must yield
        // intermediate coverage values, not only 0/255.
        let pts = [(1.0, 1.0), (14.0, 2.0), (2.0, 14.0)];
        let m = Mask::polygon(16, 16, &pts);
        assert!(
            m.data.contains(&255),
            "interior of triangle should saturate"
        );
        assert!(
            m.data.iter().any(|&v| v > 0 && v < 255),
            "diagonal edge should be anti-aliased (partial coverage)"
        );
    }

    #[test]
    fn magic_wand_edge_antialiased_interior_saturated() {
        // Solid white square on a black field, with a 1px gap to the canvas edge
        // so interior pixels are genuinely fully surrounded.
        let mut img = RasterImage::filled(9, 9, [0, 0, 0, 255]);
        for y in 2..7 {
            for x in 2..7 {
                img.set_pixel(x, y, [255, 255, 255, 255]);
            }
        }
        let m = Mask::magic_wand(&img, 4, 4, 0.1);

        // A fully-surrounded interior pixel must stay saturated.
        assert_eq!(m.get(4, 4), 255, "interior pixel must remain 255");
        // The selected boundary pixels also stay saturated (no interior erosion).
        assert_eq!(m.get(2, 2), 255, "selected edge pixel must not erode");
        // …and the anti-aliased fringe just outside the square gets partial
        // coverage between 0 and 255.
        assert!(
            m.data.iter().any(|&v| v > 0 && v < 255),
            "wand edge should be anti-aliased (partial coverage)"
        );
        // Far-away background stays fully deselected.
        assert_eq!(m.get(0, 0), 0, "far background must stay deselected");
    }

    #[test]
    fn grow_huge_px_no_hang() {
        let mut m = Mask::empty(6, 6);
        m.set(3, 3, 255);
        // Without the cap this loops u32::MAX times.
        m.grow(u32::MAX);
        assert!(!m.is_empty_selection());
        let mut m = Mask::full(6, 6);
        m.contract(u32::MAX);
        assert!(m.is_empty_selection());
    }
}
