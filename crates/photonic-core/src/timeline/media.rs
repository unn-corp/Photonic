//! Media pool (01 §3): assets referenced, never embedded.
//!
//! Contrast with `RasterImage`'s base64-PNG embedding — video files are orders
//! of magnitude too large to inline. Offline/missing media is a first-class
//! state: an asset with no reachable file renders a placeholder; relink matches
//! `content_hash` first, then filename (§9). This module is also the canonical
//! home for `VectorRef`/`VectorStateKey` (relocated from `photonic-video`'s
//! `contract.rs`, which now re-exports them).

use super::clip::ClipEffect;
use super::grade::Grade;
use super::ids::{AssetId, BinId, TagId};
use super::time::{FrameRate, Tick};
use crate::node::NodeId;
use crate::Color;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// One entry in the project media-tag registry (26 K-C2).
///
/// Mirrors [`super::sequence::MarkerCategory`]: addressed by stable
/// [`TagId`], never by index. Name is the display label; colour is optional
/// pool UI chrome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaTag {
    pub id: TagId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
}

impl MediaTag {
    pub fn new(name: impl Into<String>) -> Self {
        MediaTag {
            id: TagId::new(),
            name: name.into(),
            color: None,
        }
    }

    pub fn with_color(name: impl Into<String>, color: Color) -> Self {
        MediaTag {
            id: TagId::new(),
            name: name.into(),
            color: Some(color),
        }
    }
}

/// The asset pool plus its folder (bin) hierarchy.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct MediaPool {
    pub assets: HashMap<AssetId, MediaAsset>,
    /// Folders; a flat list with parent refs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bins: Vec<MediaBin>,
}

impl MediaPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, asset: MediaAsset) -> AssetId {
        let id = asset.id;
        self.assets.insert(id, asset);
        id
    }
}

/// A media-pool asset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: AssetId,
    pub kind: AssetKind,
    pub source: AssetSource,
    /// Filled by the engine after ffprobe; cached in-file (small, needed for
    /// offline layout).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<MediaProbe>,
    /// Engine-managed proxy (path + status).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyRef>,
    /// xxh3 of file head+tail+len — the relink identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Containing bin (folder), or `None` for the pool root/unfiled (01 §3).
    /// Additive field: v3 files written before bins load with this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bin: Option<BinId>,
    /// Asset-level effect stack (35 §2): applied in the asset's source colour
    /// space beneath every clip that references it. Empty = neutral (§2.6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<ClipEffect>,
    /// Asset-level grade (35 §2): applied after `effects`, still in source space.
    /// `None` = neutral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<Grade>,
    /// Star rating 1–5 (K-C2). `None` = unrated. Additive; omitted in older files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<u8>,
    /// Free-form tag *names* (K-C2 legacy display). Prefer [`Self::tag_ids`] +
    /// the project [`MediaTag`](crate::timeline::MediaTag) registry; names are
    /// kept so older documents and simple UIs still round-trip.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Stable tag ids into `TimelineProject::media_tags` (26 K-C2 registry).
    /// Additive; older files load with this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag_ids: Vec<TagId>,
    /// K-A8: when set, this pool entry is a **subclip** — a zone-bounded view of
    /// `parent`. Proxies, waveforms, and `content_hash` are shared with the
    /// parent (copied at create time; not re-probed). Additive; older files
    /// load with both fields absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<AssetId>,
    /// K-A8: half-open source range `[in, out)` on the parent media, in source
    /// ticks. Only meaningful when `parent` is `Some`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subclip_range: Option<(Tick, Tick)>,
}

impl MediaAsset {
    pub fn new(kind: AssetKind, source: AssetSource) -> Self {
        MediaAsset {
            id: AssetId::new(),
            kind,
            source,
            probe: None,
            proxy: None,
            content_hash: None,
            bin: None,
            effects: Vec::new(),
            grade: None,
            rating: None,
            tags: Vec::new(),
            tag_ids: Vec::new(),
            parent: None,
            subclip_range: None,
        }
    }

    /// True when this asset is a K-A8 subclip view of another pool entry.
    #[inline]
    pub fn is_subclip(&self) -> bool {
        self.parent.is_some() && self.subclip_range.is_some()
    }

    /// A file-backed asset from an absolute path.
    pub fn from_file(kind: AssetKind, path: impl Into<PathBuf>) -> Self {
        MediaAsset::new(
            kind,
            AssetSource::File {
                path: path.into(),
                rel_path: None,
            },
        )
    }
}

/// Asset media kind. `VectorDoc` is this document or an external `.photon`/`.svg`;
/// `Lut3d` is a `.cube` file, referenced not embedded (07 §1) — gets the same
/// offline/relink handling as media.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Video,
    Audio,
    Image,
    VectorDoc,
    Lut3d,
}

/// Where an asset's pixels/samples come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum AssetSource {
    /// Absolute path plus an optional project-relative fallback. The loader
    /// tries `rel_path` first (project moves survive), then `path`, then
    /// relink-by-hash (§9).
    File {
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rel_path: Option<PathBuf>,
    },
    /// Lives inside this `Document` (artboard or node subtree).
    EmbeddedVector { root: VectorRef },
}

/// Reference to vector content inside (or associated with) the document (01 §3).
/// Canonical home; `photonic-video::contract` re-exports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "vref", rename_all = "snake_case")]
pub enum VectorRef {
    Artboard(usize),
    Node(NodeId),
    WholeDocument,
}

/// Cache key for a rasterized vector frame: a hash combining the referenced
/// nodes' state, evaluated animated props, and output size (02 §3, 03 §2.5).
/// Relocated from `contract.rs`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VectorStateKey(pub u128);

/// The subset of ffprobe output we persist in-file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaProbe {
    pub duration: Tick,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoStreamInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioStreamInfo>,
    pub container: String,
    pub codec: String,
    /// K-C7: container avg vs base rate disagree → VFR. Photonic plays VFR via
    /// pts-true decode; triage reports it so the user knows. Additive default false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_vfr: bool,
    /// K-C7: source pixel format string from probe (e.g. `yuv420p`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
    /// K-C7: whether the source carries alpha.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_alpha: bool,
}

impl MediaProbe {
    /// Fixture helper: duration + container/codec only; triage flags clear.
    pub fn basic(duration: Tick, container: impl Into<String>, codec: impl Into<String>) -> Self {
        MediaProbe {
            duration,
            video: None,
            audio: None,
            container: container.into(),
            codec: codec.into(),
            is_vfr: false,
            pixel_format: None,
            has_alpha: false,
        }
    }
}

/// Field / scan type from probe (K-G6 / 32 §6). Default [`ScanType::Progressive`]
/// so pre-K-G6 documents load unchanged.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScanType {
    #[default]
    Progressive,
    InterlacedTopFirst,
    InterlacedBottomFirst,
    Unknown,
}

impl ScanType {
    pub fn is_interlaced(self) -> bool {
        matches!(
            self,
            ScanType::InterlacedTopFirst | ScanType::InterlacedBottomFirst
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    #[serde(default = "default_pixel_aspect")]
    pub pixel_aspect: f32,
    #[serde(default)]
    pub color: ProbedColor,
    /// Whether a keyframe (GOP) index has been built and cached in the sidecar.
    #[serde(default)]
    pub keyframe_index_cached: bool,
    /// Progressive / interlaced / unknown (K-G6). Omitted in older projects.
    #[serde(default, skip_serializing_if = "is_default_scan")]
    pub scan: ScanType,
}

fn is_default_scan(s: &ScanType) -> bool {
    matches!(s, ScanType::Progressive)
}

fn default_pixel_aspect() -> f32 {
    1.0
}

// ── K-C7 import-time media triage ────────────────────────────────────────────

/// Severity of one triage finding.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageSeverity {
    /// Informational — Photonic handles this correctly; user should still know.
    Info,
    /// May need attention (interlaced, odd pixel format).
    Warn,
    /// Likely needs a user remedy (non-seekable class signals if we had them).
    Action,
}

/// One human-readable finding about an imported asset (26 K-C7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageFinding {
    pub code: String,
    pub severity: TriageSeverity,
    pub summary: String,
    pub consequence: String,
    /// Optional remedy text — only when a real fix is needed (not Shotcut's
    /// blanket "Convert to Edit-friendly").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remedy: Option<String>,
}

/// Build triage findings from a persisted probe. Pure — no I/O.
///
/// Photonic already handles VFR correctly (pts-true); VFR is **Info**, not a
/// convert demand. Interlaced is **Warn** (deinterlace available). Odd pixel
/// formats and multi-channel audio mismatches are Info/Warn as appropriate.
pub fn triage_probe(probe: &MediaProbe) -> Vec<TriageFinding> {
    let mut out = Vec::new();
    if probe.is_vfr {
        out.push(TriageFinding {
            code: "vfr".into(),
            severity: TriageSeverity::Info,
            summary: "Variable frame rate".into(),
            consequence: "Container average and base rates disagree. Photonic \
                plays VFR via pts-true decode (no forced convert)."
                .into(),
            remedy: None,
        });
    }
    if let Some(v) = &probe.video {
        if v.scan.is_interlaced() {
            out.push(TriageFinding {
                code: "interlaced".into(),
                severity: TriageSeverity::Warn,
                summary: "Interlaced video".into(),
                consequence: "Fields will comb on motion until a deinterlace \
                    node is applied (K-G6 auto-inserts for interlaced sources)."
                    .into(),
                remedy: Some(
                    "Keep auto-deinterlace on, or convert offline if you need progressive masters."
                        .into(),
                ),
            });
        }
        if (v.pixel_aspect - 1.0).abs() > 0.01 {
            out.push(TriageFinding {
                code: "anamorphic".into(),
                severity: TriageSeverity::Info,
                summary: format!("Non-square pixels (PAR {:.3})", v.pixel_aspect),
                consequence: "Display aspect differs from storage dimensions; \
                    the compositor applies pixel aspect on layout."
                    .into(),
                remedy: None,
            });
        }
    }
    if probe.has_alpha {
        out.push(TriageFinding {
            code: "alpha".into(),
            severity: TriageSeverity::Info,
            summary: "Source carries alpha".into(),
            consequence: "Alpha is preserved through the graph; use alpha view \
                on the program monitor to inspect it."
                .into(),
            remedy: None,
        });
    }
    if let Some(fmt) = probe.pixel_format.as_deref() {
        let f = fmt.to_ascii_lowercase();
        // Flag uncommon formats that often surprise editors.
        if f.contains("10le")
            || f.contains("12le")
            || f.contains("p010")
            || f.contains("yuv422")
            || f.contains("yuv444")
            || f.contains("gbr")
        {
            out.push(TriageFinding {
                code: "pixel_format".into(),
                severity: TriageSeverity::Info,
                summary: format!("Pixel format {fmt}"),
                consequence: "Unusual packing; decode still works, but proxies \
                    and some hardware encoders may down-convert."
                    .into(),
                remedy: None,
            });
        }
    }
    if let Some(a) = &probe.audio {
        if a.sample_rate != 0 && a.sample_rate != 48000 && a.sample_rate != 44100 {
            out.push(TriageFinding {
                code: "sample_rate".into(),
                severity: TriageSeverity::Info,
                summary: format!("{} Hz audio", a.sample_rate),
                consequence: "Mixer resamples to the session rate; slight CPU cost.".into(),
                remedy: None,
            });
        }
        if a.channels > 2 {
            out.push(TriageFinding {
                code: "multichannel".into(),
                severity: TriageSeverity::Info,
                summary: format!("{}-channel audio", a.channels),
                consequence: "Use clip channel map / stream selection (K-D3) to pick routes."
                    .into(),
                remedy: None,
            });
        }
    }
    out
}

/// Highest severity present, if any findings.
pub fn triage_max_severity(findings: &[TriageFinding]) -> Option<TriageSeverity> {
    findings.iter().map(|f| f.severity).max_by_key(|s| match s {
        TriageSeverity::Info => 0,
        TriageSeverity::Warn => 1,
        TriageSeverity::Action => 2,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub codec: String,
}

/// Probed color characteristics (primaries/transfer/range), as reported by
/// ffprobe. Strings kept verbatim; the render pipeline (03) interprets them.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct ProbedColor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primaries: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matrix: Option<String>,
    /// `true` = full/PC range, `false` = limited/TV range, `None` = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_range: Option<bool>,
}

/// How a proxy file entered the pool (G-15A).
///
/// `Generated` is the serde default so projects written before origin existed
/// load as engine-managed cache files. `Attached` marks user-owned files that
/// must never be deleted on detach.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProxyOrigin {
    #[default]
    Generated,
    Attached,
}

/// Engine-managed proxy media reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProxyRef {
    pub path: PathBuf,
    pub status: ProxyStatus,
    /// Provenance; defaults to [`ProxyOrigin::Generated`] for old project JSON.
    #[serde(default)]
    pub origin: ProxyOrigin,
}

impl ProxyRef {
    /// Ready proxy produced by the engine (cache-owned path).
    pub fn ready_generated(path: impl Into<PathBuf>) -> Self {
        ProxyRef {
            path: path.into(),
            status: ProxyStatus::Ready,
            origin: ProxyOrigin::Generated,
        }
    }

    /// Ready proxy linked from a user-supplied file (G-15A attach).
    pub fn ready_attached(path: impl Into<PathBuf>) -> Self {
        ProxyRef {
            path: path.into(),
            status: ProxyStatus::Ready,
            origin: ProxyOrigin::Attached,
        }
    }

    /// Pending/Failed intermediate for engine generation jobs.
    pub fn with_status(path: impl Into<PathBuf>, status: ProxyStatus) -> Self {
        ProxyRef {
            path: path.into(),
            status,
            origin: ProxyOrigin::Generated,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyStatus {
    Pending,
    Ready,
    Failed,
}

/// A media bin (folder). Flat list with parent refs (01 §3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaBin {
    pub id: BinId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<BinId>,
}

impl MediaBin {
    pub fn new(name: impl Into<String>, parent: Option<BinId>) -> Self {
        MediaBin {
            id: BinId::new(),
            name: name.into(),
            parent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::time::FrameRate;
    use super::*;

    #[test]
    fn media_pool_insert_and_roundtrip() {
        let mut pool = MediaPool::new();
        let id = pool.insert(MediaAsset::from_file(AssetKind::Video, "/tmp/clip.mp4"));
        assert!(pool.assets.contains_key(&id));
        let j = serde_json::to_string(&pool).unwrap();
        let back: MediaPool = serde_json::from_str(&j).unwrap();
        assert_eq!(pool, back);
    }

    #[test]
    fn asset_effects_absent_from_json_when_empty() {
        // Additive discipline (35 §2 / §2.6): an asset with no effect/grade scope
        // omits both keys, so pre-scope media loads shape-identical.
        let a = MediaAsset::from_file(AssetKind::Video, "/tmp/clip.mp4");
        assert!(a.effects.is_empty());
        assert!(a.grade.is_none());
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("effects"));
        assert!(!json.contains("grade"));
        // A pre-scope asset JSON (no keys) still loads with neutral defaults.
        let back: MediaAsset = serde_json::from_str(&json).unwrap();
        assert!(back.effects.is_empty());
        assert!(back.grade.is_none());
    }

    #[test]
    fn lut3d_is_a_media_asset_kind() {
        let a = MediaAsset::from_file(AssetKind::Lut3d, "/luts/kodak.cube");
        assert_eq!(a.kind, AssetKind::Lut3d);
    }

    #[test]
    fn triage_probe_flags_vfr_and_interlace() {
        let mut probe = MediaProbe::basic(Tick(30_000), "mp4", "h264");
        probe.is_vfr = true;
        probe.video = Some(VideoStreamInfo {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate { num: 30, den: 1 },
            pixel_aspect: 1.0,
            color: ProbedColor::default(),
            keyframe_index_cached: false,
            scan: ScanType::InterlacedTopFirst,
        });
        let findings = triage_probe(&probe);
        assert!(findings.iter().any(|f| f.code == "vfr"));
        assert!(findings.iter().any(|f| f.code == "interlaced"));
        assert_eq!(triage_max_severity(&findings), Some(TriageSeverity::Warn));
        // VFR is Info, not a convert demand.
        let vfr = findings.iter().find(|f| f.code == "vfr").unwrap();
        assert_eq!(vfr.severity, TriageSeverity::Info);
        assert!(vfr.remedy.is_none());
    }

    #[test]
    fn media_probe_triage_fields_serde_default() {
        // Older documents without is_vfr/pixel_format/has_alpha still load.
        let json = r#"{"duration":0,"container":"mp4","codec":"h264"}"#;
        let p: MediaProbe = serde_json::from_str(json).unwrap();
        assert!(!p.is_vfr);
        assert!(p.pixel_format.is_none());
        assert!(!p.has_alpha);
        assert!(triage_probe(&p).is_empty());
    }

    #[test]
    fn asset_source_serde_tags() {
        let f = AssetSource::File {
            path: "/a/b.mp4".into(),
            rel_path: Some("b.mp4".into()),
        };
        let j = serde_json::to_string(&f).unwrap();
        assert!(j.contains("\"source\":\"file\""));
        let back: AssetSource = serde_json::from_str(&j).unwrap();
        assert_eq!(f, back);
    }

    /// G-15A: projects written before `origin` existed deserialize as Generated.
    #[test]
    fn proxy_ref_serde_defaults_origin_to_generated() {
        let old = r#"{"path":"/cache/abc.proxy.mp4","status":"ready"}"#;
        let pref: ProxyRef = serde_json::from_str(old).unwrap();
        assert_eq!(pref.status, ProxyStatus::Ready);
        assert_eq!(pref.origin, ProxyOrigin::Generated);
        assert_eq!(pref.path, PathBuf::from("/cache/abc.proxy.mp4"));
    }

    #[test]
    fn proxy_ref_attached_roundtrips() {
        let pref = ProxyRef::ready_attached("/user/cam_proxy.mp4");
        let j = serde_json::to_string(&pref).unwrap();
        let back: ProxyRef = serde_json::from_str(&j).unwrap();
        assert_eq!(pref, back);
        assert_eq!(back.origin, ProxyOrigin::Attached);
    }
}
