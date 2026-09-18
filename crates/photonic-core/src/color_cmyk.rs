//! RGB→CMYK ICC colour conversion for PDF export.
//!
//! Provides [`CmykTransform`] — a thin wrapper around a moxcms f32 transform —
//! and a process-wide cache keyed by ICC profile path so `convert_color` pays
//! the profile-parse cost only once per unique path.

use moxcms::{ColorProfile, Layout, TransformF32Executor, TransformOptions};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

/// Embedded CoatedFOGRA39 ICC profile, bundled at compile-time so export works
/// without any external files on the target machine.
pub const DEFAULT_CMYK_ICC: &[u8] = include_bytes!("../../../assets/icc/CoatedFOGRA39.icc");

// ── transform cache ─────────────────────────────────────────────────────────

/// Key used to distinguish entries in the process-wide cache.
///
/// `None` = embedded FOGRA39 default; `Some(path)` = a user-supplied profile.
type CacheKey = Option<std::path::PathBuf>;

static CACHE: OnceLock<Mutex<HashMap<CacheKey, Arc<CmykTransform>>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<CacheKey, Arc<CmykTransform>>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// ── CmykTransform ────────────────────────────────────────────────────────────

/// Variant held inside [`CmykTransform`].
enum Inner {
    /// A full ICC-based RGB→CMYK transform built from moxcms.
    Icc(Arc<TransformF32Executor>),
    /// Fallback: simple GCR formula when all ICC paths fail.
    Gcr,
}

/// A reusable, `Send + Sync` RGB→CMYK colour transform.
///
/// Build once, call [`rgb_to_cmyk`](CmykTransform::rgb_to_cmyk) as many times
/// as needed.
pub struct CmykTransform {
    inner: Inner,
}

// The Arc<dyn TransformExecutor> is Send+Sync because moxcms implements it that
// way; we assert the same for our wrapper.
unsafe impl Send for CmykTransform {}
unsafe impl Sync for CmykTransform {}

impl CmykTransform {
    // ── constructors ────────────────────────────────────────────────────────

    /// Build a transform from a raw ICC byte slice.
    ///
    /// Source is the moxcms built-in sRGB profile; destination is the CMYK
    /// profile parsed from `bytes`.  Input and output values are f32 in 0..1.
    pub fn from_icc_bytes(bytes: &[u8]) -> Result<Self, String> {
        let dst_profile = ColorProfile::new_from_slice(bytes)
            .map_err(|e| format!("moxcms: failed to parse ICC profile: {e:?}"))?;

        let src_profile = ColorProfile::new_srgb();

        let transform: Arc<TransformF32Executor> = src_profile
            .create_transform_f32(
                Layout::Rgb,
                &dst_profile,
                Layout::Rgba, // Cmyka is 5-channel; Rgba is the 4-channel CMYK layout
                TransformOptions::default(),
            )
            .map_err(|e| format!("moxcms: failed to create RGB→CMYK transform: {e:?}"))?;

        Ok(Self {
            inner: Inner::Icc(transform),
        })
    }

    /// Build a transform by reading an ICC profile from `path`.
    pub fn from_icc_path(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path)
            .map_err(|e| format!("failed to read ICC profile {}: {e}", path.display()))?;
        Self::from_icc_bytes(&bytes)
    }

    /// Build a transform using the embedded CoatedFOGRA39 profile.
    pub fn default_fogra39() -> Result<Self, String> {
        Self::from_icc_bytes(DEFAULT_CMYK_ICC)
    }

    /// A pure-Rust GCR (grey component replacement) fallback that never fails.
    fn gcr_fallback() -> Self {
        Self { inner: Inner::Gcr }
    }

    // ── conversion ──────────────────────────────────────────────────────────

    /// Convert a single RGB colour (components in 0..1) to CMYK (0..1).
    ///
    /// Output is `[C, M, Y, K]`.
    pub fn rgb_to_cmyk(&self, rgb: [f32; 3]) -> [f32; 4] {
        match &self.inner {
            Inner::Icc(transform) => {
                // Input:  3 f32 values (R, G, B) in 0..1
                // Output: 4 f32 values (C, M, Y, K) in 0..1
                // moxcms Layout::Rgba (4-channel) is used for the CMYK destination.
                let src = rgb;
                let mut dst = [0f32; 4];
                if transform.transform(&src, &mut dst).is_err() {
                    return gcr_convert(rgb);
                }
                // Clamp to ensure no negative or >1 values from extended-range
                // float math.
                [
                    dst[0].clamp(0.0, 1.0),
                    dst[1].clamp(0.0, 1.0),
                    dst[2].clamp(0.0, 1.0),
                    dst[3].clamp(0.0, 1.0),
                ]
            }
            Inner::Gcr => gcr_convert(rgb),
        }
    }
}

/// Naive GCR conversion — used only when ICC loading fails completely.
///
/// Formula: K = 1 − max(R,G,B);  C = (1−R−K)/(1−K);  M = (1−G−K)/(1−K);  Y = (1−B−K)/(1−K).
fn gcr_convert(rgb: [f32; 3]) -> [f32; 4] {
    let [r, g, b] = rgb;
    let k = 1.0 - r.max(g).max(b);
    if (1.0 - k).abs() < f32::EPSILON {
        // Pure black — avoid divide-by-zero.
        return [0.0, 0.0, 0.0, 1.0];
    }
    let denom = 1.0 - k;
    let c = (1.0 - r - k) / denom;
    let m = (1.0 - g - k) / denom;
    let y = (1.0 - b - k) / denom;
    [
        c.clamp(0.0, 1.0),
        m.clamp(0.0, 1.0),
        y.clamp(0.0, 1.0),
        k.clamp(0.0, 1.0),
    ]
}

// ── process-wide cached accessor ─────────────────────────────────────────────

/// Return (or build and cache) the [`CmykTransform`] for `icc_profile`.
///
/// * `None` → embedded CoatedFOGRA39 default.
/// * `Some(path)` → try to load the user profile; on failure, log a warning and
///   fall back to FOGRA39; if that also fails, fall back to GCR so export never
///   panics.
///
/// The result is `Arc`-shared so callers pay only an atomic reference-count bump
/// on every colour conversion after the first.
pub fn cached_transform(icc_profile: Option<&Path>) -> Arc<CmykTransform> {
    let key: CacheKey = icc_profile.map(|p| p.to_path_buf());

    {
        let guard = cache().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = guard.get(&key) {
            return Arc::clone(t);
        }
    }

    // Build outside the lock to avoid holding it during (potentially slow)
    // file I/O and profile parsing.
    let transform: Arc<CmykTransform> = match icc_profile {
        None => {
            // Embedded default.
            match CmykTransform::default_fogra39() {
                Ok(t) => Arc::new(t),
                Err(e) => {
                    tracing::warn!(
                        "Failed to build default FOGRA39 CMYK transform: {e}; \
                         falling back to GCR conversion"
                    );
                    Arc::new(CmykTransform::gcr_fallback())
                }
            }
        }
        Some(path) => {
            match CmykTransform::from_icc_path(path) {
                Ok(t) => Arc::new(t),
                Err(e) => {
                    tracing::warn!(
                        "Failed to load ICC profile {}: {e}; \
                         falling back to embedded FOGRA39",
                        path.display()
                    );
                    // Retry with the embedded default.
                    match CmykTransform::default_fogra39() {
                        Ok(t) => Arc::new(t),
                        Err(e2) => {
                            tracing::warn!(
                                "Embedded FOGRA39 fallback also failed: {e2}; \
                                 using GCR conversion"
                            );
                            Arc::new(CmykTransform::gcr_fallback())
                        }
                    }
                }
            }
        }
    };

    let mut guard = cache().lock().unwrap_or_else(|p| p.into_inner());
    // Another thread may have inserted in the meantime; prefer their entry.
    guard.entry(key).or_insert(transform).clone()
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_fogra39_builds() {
        CmykTransform::default_fogra39().expect("embedded FOGRA39 profile must parse");
    }

    /// Pure red should have low C, high M, high Y (and moderate K) per FOGRA39.
    #[test]
    fn test_red_conversion_sanity() {
        let t = CmykTransform::default_fogra39().unwrap();
        let [c, m, y, k] = t.rgb_to_cmyk([1.0, 0.0, 0.0]);
        // All channels in valid range.
        assert!((0.0..=1.0).contains(&c), "C out of range: {c}");
        assert!((0.0..=1.0).contains(&m), "M out of range: {m}");
        assert!((0.0..=1.0).contains(&y), "Y out of range: {y}");
        assert!((0.0..=1.0).contains(&k), "K out of range: {k}");
        // Red in FOGRA39: C low (<0.15), M high (>0.5), Y high (>0.5).
        assert!(c < 0.15, "Red should have low C, got {c}");
        assert!(m > 0.5, "Red should have high M, got {m}");
        assert!(y > 0.5, "Red should have high Y, got {y}");
    }

    /// White should map to all-zero (no ink).
    #[test]
    fn test_white_conversion() {
        let t = CmykTransform::default_fogra39().unwrap();
        let [c, m, y, k] = t.rgb_to_cmyk([1.0, 1.0, 1.0]);
        assert!(c < 0.05, "White: C should be ~0, got {c}");
        assert!(m < 0.05, "White: M should be ~0, got {m}");
        assert!(y < 0.05, "White: Y should be ~0, got {y}");
        assert!(k < 0.05, "White: K should be ~0, got {k}");
    }

    /// Black should map to high K.
    #[test]
    fn test_black_conversion() {
        let t = CmykTransform::default_fogra39().unwrap();
        let [_c, _m, _y, k] = t.rgb_to_cmyk([0.0, 0.0, 0.0]);
        assert!(k > 0.7, "Black should have high K, got {k}");
    }

    /// `cached_transform(None)` must return a valid transform without panicking.
    #[test]
    fn test_cached_transform_default() {
        let t = cached_transform(None);
        let [c, m, y, k] = t.rgb_to_cmyk([1.0, 0.0, 0.0]);
        assert!(
            c < 0.15 && m > 0.5 && y > 0.5,
            "cached red: c={c} m={m} y={y} k={k}"
        );
    }
}
