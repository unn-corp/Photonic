//! Color-grading data model (07 §1, normative).
//!
//! `Clip.grade: Option<Grade>` holds an ordered corrector stack — the Resolve
//! node-page mental model, not a flat filter list. Every op's params animate
//! through the same [`AnimProps`](super::anim::AnimProps) machinery as clip
//! transforms and effects. Grade math (linear working space) lives in the engine
//! (`photonic-video`); this module is pure data plus the ASC CDL XML interchange
//! functions (07 §6.2), which are pure string in/out and satisfy core's no-I/O
//! rule.

use super::anim::{AnimProps, PropSet};
use super::ids::{AssetId, GradeOpId, GraphId, GraphNodeId};
use super::prop_registry::PropTargetKind;
use super::unknown::UnknownTag;
use serde::{Deserialize, Serialize};

/// An ordered grade stack applied to a clip (or embedded in a `GraphOp::Grade`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Grade {
    /// Ordered; user-reorderable. Default seed order is defined in 07 §4.4.
    pub ops: Vec<GradeOp>,
    /// Global grade bypass (color-page toggle); `false` = active.
    #[serde(default)]
    pub bypass: bool,
}

impl Grade {
    pub fn new() -> Self {
        Grade {
            ops: Vec::new(),
            bypass: false,
        }
    }
}

impl Default for Grade {
    fn default() -> Self {
        Grade::new()
    }
}

/// One corrector in the stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GradeOp {
    pub id: GradeOpId,
    /// Per-op bypass.
    #[serde(default = "crate::timeline::grade::default_true")]
    pub enabled: bool,
    /// Discriminant, immutable after creation.
    pub kind: GradeOpKind,
    /// `base + PropertyTrack` keyframes (01 §6 convention).
    pub params: AnimProps<GradeOpParams>,
    /// Spatial/qualifier mask; `None` = full frame (§4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<GradeMask>,
}

pub(crate) fn default_true() -> bool {
    true
}

impl GradeOp {
    /// Construct an op with default params for its kind.
    pub fn new(kind: GradeOpKind, params: GradeOpParams) -> Self {
        GradeOp {
            id: GradeOpId::new(),
            enabled: true,
            kind,
            params: AnimProps::new(params),
            mask: None,
        }
    }
}

/// Grade-op discriminant. Additive-only; never remove or renumber a variant
/// (07 §1). Serde uses snake_case tags matching `GradeOpParams`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GradeOpKind {
    Exposure,
    Contrast,
    WhiteBalance,
    Cdl,
    Wheels,
    Curves,
    HslQualifier,
    Lut3d,
    /// Forward-compat (39 §2.2): a variant this build does not know. The
    /// original serialized tag is preserved verbatim and re-emitted on save.
    /// Declared last so serde tries the known snake_case tags first.
    #[serde(untagged)]
    Unknown(UnknownTag),
}

impl GradeOpKind {
    pub fn target_kind(self) -> PropTargetKind {
        PropTargetKind::GradeOp(self)
    }

    /// The preserved tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(self) -> Option<UnknownTag> {
        match self {
            GradeOpKind::Unknown(t) => Some(t),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(self) -> bool {
        matches!(self, GradeOpKind::Unknown(_))
    }
}

/// 3D-LUT interpolation quality (07 §3.8, §6.5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LutInterp {
    /// Shipped baseline.
    Trilinear,
    /// Behind a quality toggle.
    Tetrahedral,
}

/// ASC CDL slope/offset/power/sat, per-channel. Used both as the `Cdl` op's
/// params and inline as `HslQualifier.correction` (07 §1 — deliberately NOT a
/// boxed recursive `GradeOpParams`).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CdlParams {
    pub slope: [f32; 3],
    pub offset: [f32; 3],
    pub power: [f32; 3],
    pub sat: f32,
}

impl CdlParams {
    /// The identity correction (no-op).
    pub fn identity() -> Self {
        CdlParams {
            slope: [1.0; 3],
            offset: [0.0; 3],
            power: [1.0; 3],
            sat: 1.0,
        }
    }
}

impl Default for CdlParams {
    fn default() -> Self {
        CdlParams::identity()
    }
}

/// Per-kind grade parameters. Every field is a plain scalar/array — animation is
/// supplied entirely by the surrounding `AnimProps` (07 §1). Forward-compat is
/// carried by the `#[serde(other)] Unknown` catch-all: a newer binary's op kind
/// loads inert and flagged, never dropped.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GradeOpParams {
    Exposure {
        stops: f32,
    },
    Contrast {
        pivot: f32,
        amount: f32,
    },
    WhiteBalance {
        temp: f32,
        tint: f32,
    },
    Cdl {
        slope: [f32; 3],
        offset: [f32; 3],
        power: [f32; 3],
        sat: f32,
    },
    Wheels {
        lift: [f32; 3],
        gamma: [f32; 3],
        gain: [f32; 3],
        sat: f32,
    },
    Curves {
        master: Vec<(f32, f32)>,
        red: Vec<(f32, f32)>,
        green: Vec<(f32, f32)>,
        blue: Vec<(f32, f32)>,
        hue_vs_hue: Vec<(f32, f32)>,
        hue_vs_sat: Vec<(f32, f32)>,
    },
    HslQualifier {
        hue: [f32; 2],
        sat: [f32; 2],
        lum: [f32; 2],
        softness: f32,
        correction: CdlParams,
    },
    Lut3d {
        asset: AssetId,
        intensity: f32,
        interp: LutInterp,
    },
    /// Forward-compat (39 §2.2): an op kind this build does not understand. The
    /// whole object — `kind` tag and payload — is retained verbatim and
    /// re-emitted unchanged (the old `#[serde(other)]` unit variant destroyed
    /// the payload and rewrote the tag). Loads inert + flagged in UI, never
    /// dropped. Declared last so serde tries the known tags first.
    #[serde(untagged)]
    Unknown(serde_json::Map<String, serde_json::Value>),
}

impl GradeOpParams {
    /// The op kind this param payload corresponds to, or `None` for `Unknown`.
    pub fn kind(&self) -> Option<GradeOpKind> {
        Some(match self {
            GradeOpParams::Exposure { .. } => GradeOpKind::Exposure,
            GradeOpParams::Contrast { .. } => GradeOpKind::Contrast,
            GradeOpParams::WhiteBalance { .. } => GradeOpKind::WhiteBalance,
            GradeOpParams::Cdl { .. } => GradeOpKind::Cdl,
            GradeOpParams::Wheels { .. } => GradeOpKind::Wheels,
            GradeOpParams::Curves { .. } => GradeOpKind::Curves,
            GradeOpParams::HslQualifier { .. } => GradeOpKind::HslQualifier,
            GradeOpParams::Lut3d { .. } => GradeOpKind::Lut3d,
            GradeOpParams::Unknown(_) => return None,
        })
    }

    /// The preserved `kind` tag if this is an unknown (forward-compat) variant.
    pub fn unknown_tag(&self) -> Option<&str> {
        match self {
            GradeOpParams::Unknown(map) => map.get("kind").and_then(|v| v.as_str()),
            _ => None,
        }
    }

    /// True if this is a forward-compat variant this build does not understand.
    pub fn is_unknown(&self) -> bool {
        matches!(self, GradeOpParams::Unknown(_))
    }
}

impl PropSet for GradeOpParams {
    // Like `EffectParams`, grade params span multiple kinds; the concrete kind
    // drives per-path validation via `prop_registry`, so the generic bound uses
    // the lenient `GraphNode` target for its single const.
    const TARGET_KIND: PropTargetKind = PropTargetKind::GraphNode;
}

/// A grade mask restricting an op to part of the frame (§4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape_kind", rename_all = "snake_case")]
pub enum GradeMask {
    PowerWindow {
        shape: WindowShape,
        center: [f32; 2],
        size: [f32; 2],
        rotation: f32,
        softness: f32,
        invert: bool,
    },
    /// Stretch/forward-compat (07 §4.2): not required for CAP-015's v1 exit.
    RotoMatte { source: MaskRef, invert: bool },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowShape {
    Ellipse,
    Rectangle,
}

/// Reference to a mask-producing source for `GradeMask::RotoMatte` (07 §4.2).
///
/// The spec leaves `MaskRef`'s concrete shape open (no roto tool exists yet). We
/// define a minimal forward-compat enum covering the two named v1 candidates: an
/// automatic subject matte (`photonic-matte`) and a mask-typed node in a
/// composition graph. See the deviation note in the P2 report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MaskRef {
    /// Auto subject cutout via `photonic-matte` (07 §4.2 v1 candidate).
    Matte,
    /// A `Mask`-typed output of a node in a composition graph.
    GraphNode { graph: GraphId, node: GraphNodeId },
}

// ── ASC CDL XML interchange (07 §6.2) ───────────────────────────────────────

/// Error parsing ASC CDL XML.
#[derive(Debug, Clone, PartialEq)]
pub enum CdlXmlError {
    /// The document was not well-formed XML.
    Malformed(String),
    /// A `Slope`/`Offset`/`Power` element did not contain three floats, or a
    /// `Saturation` was not a single float.
    BadNumbers(String),
    /// No `<ColorCorrection>` element was found.
    NoColorCorrection,
}

impl std::fmt::Display for CdlXmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdlXmlError::Malformed(s) => write!(f, "malformed CDL XML: {s}"),
            CdlXmlError::BadNumbers(s) => write!(f, "bad CDL numbers: {s}"),
            CdlXmlError::NoColorCorrection => write!(f, "no <ColorCorrection> element"),
        }
    }
}

impl std::error::Error for CdlXmlError {}

fn parse_triple(s: &str) -> Result<[f32; 3], CdlXmlError> {
    let parts: Vec<f32> = s
        .split_whitespace()
        .map(|p| p.parse::<f32>())
        .collect::<Result<_, _>>()
        .map_err(|e| CdlXmlError::BadNumbers(e.to_string()))?;
    if parts.len() != 3 {
        return Err(CdlXmlError::BadNumbers(format!(
            "expected 3 values, got {}",
            parts.len()
        )));
    }
    Ok([parts[0], parts[1], parts[2]])
}

/// Parse ASC CDL XML — either a single `<ColorCorrection>` (`.cdl`) or a
/// `<ColorCorrectionCollection>` of several (`.ccc`) — into `(id, params)` pairs.
/// A missing SOP/Sat child defaults to the identity for that component.
pub fn parse_cdl_xml(s: &str) -> Result<Vec<(String, CdlParams)>, CdlXmlError> {
    let doc = roxmltree::Document::parse(s).map_err(|e| CdlXmlError::Malformed(e.to_string()))?;
    let mut out = Vec::new();
    for cc in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "ColorCorrection")
    {
        let id = cc.attribute("id").unwrap_or("").to_string();
        let mut params = CdlParams::identity();
        if let Some(sop) = cc
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SOPNode")
        {
            for child in sop.children().filter(|n| n.is_element()) {
                let text = child.text().unwrap_or("").trim();
                match child.tag_name().name() {
                    "Slope" => params.slope = parse_triple(text)?,
                    "Offset" => params.offset = parse_triple(text)?,
                    "Power" => params.power = parse_triple(text)?,
                    _ => {}
                }
            }
        }
        if let Some(sat) = cc
            .children()
            .find(|n| n.is_element() && n.tag_name().name() == "SatNode")
        {
            if let Some(satn) = sat
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "Saturation")
            {
                let text = satn.text().unwrap_or("").trim();
                params.sat = text
                    .parse::<f32>()
                    .map_err(|e| CdlXmlError::BadNumbers(e.to_string()))?;
            }
        }
        out.push((id, params));
    }
    if out.is_empty() {
        return Err(CdlXmlError::NoColorCorrection);
    }
    Ok(out)
}

/// Write `(id, params)` pairs as ASC CDL XML. A single entry emits a bare
/// `<ColorCorrection>` (`.cdl`); multiple entries emit a
/// `<ColorCorrectionCollection>` (`.ccc`). Round-trips through [`parse_cdl_xml`].
pub fn write_cdl_xml(entries: &[(String, CdlParams)]) -> String {
    fn triple(v: &[f32; 3]) -> String {
        format!("{} {} {}", v[0], v[1], v[2])
    }
    fn one(id: &str, p: &CdlParams, indent: &str) -> String {
        let id_attr = if id.is_empty() {
            String::new()
        } else {
            format!(" id=\"{id}\"")
        };
        format!(
            "{i}<ColorCorrection{id_attr}>\n\
             {i}  <SOPNode>\n\
             {i}    <Slope>{}</Slope>\n\
             {i}    <Offset>{}</Offset>\n\
             {i}    <Power>{}</Power>\n\
             {i}  </SOPNode>\n\
             {i}  <SatNode>\n\
             {i}    <Saturation>{}</Saturation>\n\
             {i}  </SatNode>\n\
             {i}</ColorCorrection>\n",
            triple(&p.slope),
            triple(&p.offset),
            triple(&p.power),
            p.sat,
            i = indent
        )
    }

    let mut s = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    if entries.len() == 1 {
        s.push_str(&one(&entries[0].0, &entries[0].1, ""));
    } else {
        s.push_str("<ColorCorrectionCollection>\n");
        for (id, p) in entries {
            s.push_str(&one(id, p, "  "));
        }
        s.push_str("</ColorCorrectionCollection>\n");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grade_op_params_forward_compat_unknown() {
        // A payload tagged with an op kind this build doesn't know loads as
        // Unknown, preserving the WHOLE object verbatim (39 §2.2 rule 1) — the
        // old `#[serde(other)]` unit variant destroyed the payload.
        let json = r#"{"kind":"future_op","wild":[1,2,3]}"#;
        let p: GradeOpParams = serde_json::from_str(json).unwrap();
        assert!(p.is_unknown());
        assert_eq!(p.unknown_tag(), Some("future_op"));
        assert_eq!(p.kind(), None);
        // Re-serializes value-equal to the input (payload retained, not dropped).
        let back: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        let orig: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(back, orig);
    }

    #[test]
    fn grade_op_kind_unknown_preserves_tag() {
        let k: GradeOpKind = serde_json::from_str("\"bloom\"").unwrap();
        assert!(k.is_unknown());
        assert_eq!(k.unknown_tag().unwrap().as_str(), "bloom");
        assert_eq!(serde_json::to_string(&k).unwrap(), "\"bloom\"");
        for (k, tag) in [
            (GradeOpKind::Exposure, "\"exposure\""),
            (GradeOpKind::Cdl, "\"cdl\""),
            (GradeOpKind::Lut3d, "\"lut3d\""),
        ] {
            assert_eq!(serde_json::to_string(&k).unwrap(), tag);
            let back: GradeOpKind = serde_json::from_str(tag).unwrap();
            assert_eq!(back, k);
            assert!(!back.is_unknown());
        }
    }

    #[test]
    fn grade_op_params_malformed_known_falls_to_unknown_with_known_tag() {
        // serde's per-variant untagged fallback is greedy: a KNOWN kind with a
        // malformed field degrades to Unknown at the serde layer (this
        // regressed the old `#[serde(other)]` unit variant, which errored). The
        // document-level integrity guard lives in `load::finalize_load`, which
        // rejects a retained Unknown whose tag is a KNOWN catalog tag. Here we
        // pin the serde-layer behaviour that feeds that guard.
        let bad = r#"{"kind":"exposure","stops":"NOPE"}"#;
        let p: GradeOpParams = serde_json::from_str(bad).unwrap();
        assert!(p.is_unknown());
        assert_eq!(p.unknown_tag(), Some("exposure"));
    }

    #[test]
    fn grade_op_params_roundtrip_each_kind() {
        let samples = vec![
            GradeOpParams::Exposure { stops: 1.5 },
            GradeOpParams::Cdl {
                slope: [1.1, 1.0, 0.9],
                offset: [0.0, 0.01, -0.02],
                power: [1.0, 1.0, 1.0],
                sat: 1.2,
            },
            GradeOpParams::Lut3d {
                asset: AssetId::new(),
                intensity: 0.8,
                interp: LutInterp::Tetrahedral,
            },
        ];
        for s in samples {
            let j = serde_json::to_string(&s).unwrap();
            let back: GradeOpParams = serde_json::from_str(&j).unwrap();
            assert_eq!(s, back);
        }
    }

    #[test]
    fn cdl_xml_round_trips() {
        let entries = vec![(
            "shot_010".to_string(),
            CdlParams {
                slope: [1.2, 1.0, 0.8],
                offset: [0.0, 0.05, -0.05],
                power: [0.9, 1.0, 1.1],
                sat: 1.1,
            },
        )];
        let xml = write_cdl_xml(&entries);
        let parsed = parse_cdl_xml(&xml).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "shot_010");
        let p = parsed[0].1;
        let e = entries[0].1;
        for i in 0..3 {
            assert!((p.slope[i] - e.slope[i]).abs() < 1e-5);
            assert!((p.offset[i] - e.offset[i]).abs() < 1e-5);
            assert!((p.power[i] - e.power[i]).abs() < 1e-5);
        }
        assert!((p.sat - e.sat).abs() < 1e-5);
    }

    #[test]
    fn ccc_collection_round_trips() {
        let entries = vec![
            ("a".to_string(), CdlParams::identity()),
            (
                "b".to_string(),
                CdlParams {
                    slope: [2.0, 2.0, 2.0],
                    offset: [0.0; 3],
                    power: [1.0; 3],
                    sat: 1.0,
                },
            ),
        ];
        let xml = write_cdl_xml(&entries);
        assert!(xml.contains("ColorCorrectionCollection"));
        let parsed = parse_cdl_xml(&xml).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].0, "b");
        assert!((parsed[1].1.slope[0] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn parse_rejects_non_cdl() {
        assert!(matches!(
            parse_cdl_xml("<foo/>"),
            Err(CdlXmlError::NoColorCorrection)
        ));
        assert!(matches!(
            parse_cdl_xml("not xml <<"),
            Err(CdlXmlError::Malformed(_))
        ));
    }
}
