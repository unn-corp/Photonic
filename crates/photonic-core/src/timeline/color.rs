//! Versioned color intent. Legacy documents retain their existing renderer;
//! managed configurations describe required transforms, never implicit fallbacks.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// SHA-256 identity of the exact configuration or transform bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ColorDigest(String);

impl TryFrom<String> for ColorDigest {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("color resource digest must contain 64 hexadecimal SHA-256 digits".into());
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl From<ColorDigest> for String {
    fn from(value: ColorDigest) -> Self {
        value.0
    }
}

impl ColorDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A `.cube` file's declared signal space. A named OCIO space is meaningful
/// only with the exact configuration whose hash is recorded here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LutColorSpace {
    LegacySrgbEncoded,
    Ocio {
        config_sha256: ColorDigest,
        color_space: String,
    },
    Native {
        transform_revision: u32,
        space: NativeLutSpace,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeLutSpace {
    Acescg,
    Acescct,
    SrgbDisplayEncoded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LutPurpose {
    Creative,
    Technical,
}

/// Versioned interpretation of a LUT asset, separate from each correction's
/// interpolation/intensity. An absent value preserves old Legacy SDR projects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LutColorInterpretation {
    pub version: u32,
    pub purpose: LutPurpose,
    pub input: LutColorSpace,
    pub output: LutColorSpace,
}

impl LutColorInterpretation {
    pub fn legacy_creative() -> Self {
        Self {
            version: 1,
            purpose: LutPurpose::Creative,
            input: LutColorSpace::LegacySrgbEncoded,
            output: LutColorSpace::LegacySrgbEncoded,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported LUT color interpretation version".into());
        }
        for space in [&self.input, &self.output] {
            match space {
                LutColorSpace::Ocio { color_space, .. } => {
                    if color_space.trim().is_empty() || color_space.contains('\0') {
                        return Err(
                            "LUT OCIO color space must be nonempty and contain no NUL".into()
                        );
                    }
                }
                LutColorSpace::Native {
                    transform_revision, ..
                } => {
                    if *transform_revision != 1 {
                        return Err("unsupported native LUT transform revision".into());
                    }
                }
                LutColorSpace::LegacySrgbEncoded => {}
            }
        }
        Ok(())
    }

    /// Creative native LUTs operate entirely in one explicitly pinned grading space.
    pub fn validate_for_native_grade(&self) -> Result<NativeLutSpace, String> {
        self.validate()?;
        if self.purpose != LutPurpose::Creative {
            return Err("technical LUT requires a technical color-transform stage".into());
        }
        if self.input != self.output {
            return Err("native creative LUT input and output spaces must match".into());
        }
        match self.input {
            LutColorSpace::Native {
                transform_revision: 1,
                space: NativeLutSpace::Acescg,
            } => Ok(NativeLutSpace::Acescg),
            LutColorSpace::Native {
                transform_revision: 1,
                space: NativeLutSpace::Acescct,
            } => Ok(NativeLutSpace::Acescct),
            _ => Err("native creative LUT requires declared ACEScg or ACEScct coordinates".into()),
        }
    }

    pub fn validate_for_legacy_grade(&self) -> Result<(), String> {
        self.validate()?;
        if self.purpose != LutPurpose::Creative {
            return Err("technical LUT requires a technical color-transform stage".into());
        }
        if self.input != LutColorSpace::LegacySrgbEncoded
            || self.output != LutColorSpace::LegacySrgbEncoded
        {
            return Err("LUT input/output spaces are unavailable in Legacy SDR grading".into());
        }
        Ok(())
    }
}

/// Omitted in old projects, which always retain Legacy SDR behavior.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", content = "configuration", rename_all = "snake_case")]
pub enum SequenceColorConfig {
    #[default]
    LegacySdr,
    Managed(Box<ManagedColorConfig>),
    /// Photonic-owned transforms, distinct from preserved OCIO-backed drafts.
    NativeManaged(Box<NativeManagedColorConfig>),
}

impl SequenceColorConfig {
    pub fn is_legacy(&self) -> bool {
        matches!(self, Self::LegacySdr)
    }

    /// Stable serialized transform identity. Ordered resource maps make this
    /// independent of insertion order. Reserved for managed render-cache keys;
    /// the currently gated managed renderer does not consume it yet.
    pub fn cache_identity(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// A display/view selection is a technical transform, not a creative LUT.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayView {
    pub display: String,
    pub view: String,
}

/// Export intent is independent of the monitor's display/view selection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExportColorTransform {
    DisplayView { display: String, view: String },
    ColorSpace { name: String },
}

/// A reproducible OCIO configuration. Transform assets use config-relative
/// names and content digests, not machine-specific absolute paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OcioConfigIdentity {
    pub runtime_version: String,
    pub name: String,
    pub sha256: ColorDigest,
    #[serde(default)]
    pub transform_assets: BTreeMap<String, ColorDigest>,
}

/// Version 1 fixes ACEScg compositing and ACEScct logarithmic grading. Changing
/// these semantics requires a new version, rather than reinterpreting a grade.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedColorConfig {
    pub version: u32,
    pub grading_semantics_version: u32,
    pub ocio: OcioConfigIdentity,
    pub display: DisplayView,
    pub export: ExportColorTransform,
    pub unknown_input: UnknownInputPolicy,
}

/// Versioned Photonic-owned managed-color intent. Revision 1 pins the numeric
/// transform definitions independently of the executable's package version.
/// It is an authoring draft until the native renderer and output are qualified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeManagedColorConfig {
    pub version: u32,
    pub grading_semantics_version: u32,
    pub transform_revision: u32,
    pub display: NativeOutputTransform,
    pub export: NativeOutputTransform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeOutputTransform {
    SrgbSdr,
    /// 100-nit SDR rendering followed by BT.709 video-signal transfer.
    /// This is an export target, not a viewer display transform.
    Bt709VideoSdr,
}

/// Explicit source interpretation for the Photonic-owned runtime. This is
/// deliberately separate from an OCIO configuration's named color space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeInputColorInterpretation {
    /// Nominal HLG display peak used only to calibrate its scene reference-white anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hlg_peak_nits: Option<u32>,
    /// Explicit PQ normalization or HLG neutral-white luminance anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_white_nits: Option<u32>,
    pub version: u32,
    pub standard: NativeInputStandard,
    pub range: InputSignalRange,
    pub matrix: InputMatrix,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_location: Option<NativeChromaLocation>,
}

/// Position of a 4:2:0 chroma sample relative to its top-left luma sample.
/// Names follow FFmpeg's AVChromaLocation values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeChromaLocation {
    Left,
    Center,
    #[serde(rename = "topleft", alias = "top_left")]
    TopLeft,
    Top,
    #[serde(rename = "bottomleft", alias = "bottom_left")]
    BottomLeft,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeInputStandard {
    /// Display-referred sRGB still; gamut conversion does not invert an ODT.
    SrgbDisplay,
    Bt709Scene,
    Bt2020Scene,
    /// Inverse HLG OETF scene light, calibrated to an explicitly selected white/peak.
    Bt2100HlgScene,
    /// Absolute display-referred BT.2100 PQ; does not invert camera rendering.
    Bt2100PqDisplay,
}

impl NativeInputColorInterpretation {
    pub fn validate_asset_kind(&self, kind: super::media::AssetKind) -> Result<(), String> {
        self.validate()?;
        let expected = if self.standard == NativeInputStandard::SrgbDisplay {
            super::media::AssetKind::Image
        } else {
            super::media::AssetKind::Video
        };
        if kind != expected {
            return Err("native input interpretation does not match the asset kind".into());
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err("unsupported native input interpretation version".into());
        }
        if self.range == InputSignalRange::FromMetadata {
            return Err("native input requires an explicit full or limited signal range".into());
        }
        match self.standard {
            NativeInputStandard::Bt2100PqDisplay => {
                if !matches!(self.reference_white_nits, Some(1..=10000))
                    || self.hlg_peak_nits.is_some()
                {
                    return Err(
                        "PQ input requires reference white 1..10000 nits without an HLG peak"
                            .into(),
                    );
                }
            }
            NativeInputStandard::Bt2100HlgScene => {
                if !matches!(self.hlg_peak_nits, Some(400..=2000))
                    || !matches!(self.reference_white_nits, Some(1..=2000))
                    || self.reference_white_nits > self.hlg_peak_nits
                {
                    return Err("HLG scene input requires peak 400..2000 nits and reference white between 1 and that peak".into());
                }
            }
            _ => {
                if self.reference_white_nits.is_some() || self.hlg_peak_nits.is_some() {
                    return Err("HDR normalization is only valid for PQ or HLG input".into());
                }
            }
        }
        let expected = match self.standard {
            NativeInputStandard::SrgbDisplay => {
                if self.range != InputSignalRange::Full || self.chroma_location.is_some() {
                    return Err(
                        "native sRGB still requires full-range RGB without chroma siting".into(),
                    );
                }
                InputMatrix::Rgb
            }
            NativeInputStandard::Bt709Scene => InputMatrix::Bt709,
            NativeInputStandard::Bt2020Scene
            | NativeInputStandard::Bt2100PqDisplay
            | NativeInputStandard::Bt2100HlgScene => InputMatrix::Bt2020NonConstant,
        };
        if self.matrix != expected {
            return Err("native input matrix does not match its source standard".into());
        }
        Ok(())
    }
}

impl NativeManagedColorConfig {
    pub fn sdr_draft() -> Self {
        Self {
            version: 1,
            grading_semantics_version: 1,
            transform_revision: 1,
            display: NativeOutputTransform::SrgbSdr,
            export: NativeOutputTransform::Bt709VideoSdr,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.grading_semantics_version != 1 || self.transform_revision != 1
        {
            return Err(
                "unsupported native managed-color transform or grading semantics version".into(),
            );
        }
        if self.display != NativeOutputTransform::SrgbSdr {
            return Err("native managed viewer requires an sRGB SDR display transform".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case")]
pub enum UnknownInputPolicy {
    RequireExplicit,
    /// A deliberate user-selected assumption, never an automatic sRGB fallback.
    AssumeColorSpace {
        name: String,
    },
}

impl ManagedColorConfig {
    pub const WORKING_SPACE: &'static str = "ACEScg";
    pub const GRADING_SPACE: &'static str = "ACEScct";

    /// Resolve clip → asset → explicit sequence assumption. A stale override
    /// from a different configuration is an error, never a reason to fall back.
    pub fn resolve_input<'a>(
        &'a self,
        clip: Option<&'a InputColorInterpretation>,
        asset: Option<&'a InputColorInterpretation>,
    ) -> Result<ResolvedInput<'a>, String> {
        self.validate()?;
        if let Some(input) = effective_input(clip, asset) {
            input.validate()?;
            if input.config_sha256 != self.ocio.sha256 {
                return Err(
                    "input interpretation belongs to a different OCIO configuration".into(),
                );
            }
            return Ok(ResolvedInput::Explicit(input));
        }
        match &self.unknown_input {
            UnknownInputPolicy::RequireExplicit => {
                Err("source color is unknown; specify an input interpretation".into())
            }
            UnknownInputPolicy::AssumeColorSpace { name } => Ok(ResolvedInput::Assumed(name)),
        }
    }

    /// Validate authoring intent independently of runtime availability. Unknown
    /// versions can be preserved in a file but may never execute as version 1.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.grading_semantics_version != 1 {
            return Err("unsupported managed color or grading semantics version".into());
        }
        for value in [
            &self.ocio.runtime_version,
            &self.ocio.name,
            &self.display.display,
            &self.display.view,
        ] {
            if value.trim().is_empty() || value.contains('\0') {
                return Err("color configuration names and runtime version must be nonempty and contain no NUL".into());
            }
        }
        let export_valid = match &self.export {
            ExportColorTransform::DisplayView { display, view } => {
                !display.trim().is_empty()
                    && !view.trim().is_empty()
                    && !display.contains('\0')
                    && !view.contains('\0')
            }
            ExportColorTransform::ColorSpace { name } => {
                !name.trim().is_empty() && !name.contains('\0')
            }
        };
        if !export_valid {
            return Err("export color transform is incomplete".into());
        }
        if let UnknownInputPolicy::AssumeColorSpace { name } = &self.unknown_input {
            if name.trim().is_empty() || name.contains('\0') {
                return Err("assumed input color space is empty or invalid".into());
            }
        }
        for path in self.ocio.transform_assets.keys() {
            if path.is_empty()
                || path.contains(['\\', ':', '\0'])
                || path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err("transform asset names must be normalized relative paths".into());
            }
        }
        Ok(())
    }
}

/// Source encoding is interpreted in the named, pinned OCIO configuration.
/// A clip's value overrides its asset's value; None inherits, not assumes sRGB.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputColorInterpretation {
    pub config_sha256: ColorDigest,
    pub color_space: String,
    pub range: InputSignalRange,
    pub matrix: InputMatrix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSignalRange {
    FromMetadata,
    Full,
    Limited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMatrix {
    FromMetadata,
    Rgb,
    Bt601,
    Bt709,
    Bt2020NonConstant,
}

impl InputColorInterpretation {
    pub fn validate(&self) -> Result<(), String> {
        if self.color_space.trim().is_empty() || self.color_space.contains('\0') {
            return Err("input color space must be nonempty and contain no NUL".into());
        }
        Ok(())
    }
}

/// The assumption remains visible so inspection never presents it as probe data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolvedInput<'a> {
    Explicit(&'a InputColorInterpretation),
    Assumed(&'a str),
}

/// Select an override without silently replacing a missing interpretation.
pub fn effective_input<'a>(
    clip: Option<&'a InputColorInterpretation>,
    asset: Option<&'a InputColorInterpretation>,
) -> Option<&'a InputColorInterpretation> {
    clip.or(asset)
}

/// A managed copy is an explicit conversion draft; the runtime must separately
/// qualify its processors, decoder precision and presentation/export paths.
pub fn conversion_copy(
    sequence: &super::Sequence,
    config: ManagedColorConfig,
) -> Result<super::Sequence, String> {
    config.validate()?;
    let mut copy = sequence.duplicate_with_fresh_ids();
    copy.name = format!("{} — managed color", sequence.name);
    copy.color = SequenceColorConfig::Managed(Box::new(config));
    Ok(copy)
}

/// Create a distinct native-managed draft without modifying the Legacy SDR
/// original or silently converting an OCIO-managed sequence.
pub fn native_conversion_copy(
    sequence: &super::Sequence,
    config: NativeManagedColorConfig,
) -> Result<super::Sequence, String> {
    config.validate()?;
    if !sequence.color.is_legacy() {
        return Err("native conversion requires a Legacy SDR source sequence".into());
    }
    let mut copy = sequence.duplicate_with_fresh_ids();
    copy.name = format!("{} — native managed color", sequence.name);
    copy.color = SequenceColorConfig::NativeManaged(Box::new(config));
    Ok(copy)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::timeline::{FrameRate, Sequence};

    pub(crate) fn config() -> ManagedColorConfig {
        ManagedColorConfig {
            version: 1,
            grading_semantics_version: 1,
            ocio: OcioConfigIdentity {
                runtime_version: "2.5.2".into(),
                name: "owned-test-config".into(),
                sha256: "a".repeat(64).try_into().unwrap(),
                transform_assets: BTreeMap::new(),
            },
            display: DisplayView {
                display: "sRGB".into(),
                view: "SDR".into(),
            },
            export: ExportColorTransform::ColorSpace {
                name: "Rec.709".into(),
            },
            unknown_input: UnknownInputPolicy::RequireExplicit,
        }
    }

    #[test]
    fn old_sequence_roundtrip_retains_legacy_and_omits_new_fields() {
        let sequence = Sequence::new("legacy", FrameRate::FPS_30, 32, 32);
        let json = serde_json::to_value(&sequence).unwrap();
        assert!(json.get("color").is_none());
        let decoded: Sequence = serde_json::from_value(json.clone()).unwrap();
        assert!(decoded.color.is_legacy());
        assert_eq!(serde_json::to_value(decoded).unwrap(), json);
    }

    #[test]
    fn conversion_is_a_distinct_sequence_and_original_is_unchanged() {
        let original = Sequence::new("original", FrameRate::FPS_30, 32, 32);
        let before = serde_json::to_value(&original).unwrap();
        let copy = conversion_copy(&original, config()).unwrap();
        assert_ne!(original.id, copy.id);
        assert!(!copy.color.is_legacy());
        assert_eq!(serde_json::to_value(&original).unwrap(), before);
        let decoded: Sequence =
            serde_json::from_value(serde_json::to_value(&copy).unwrap()).unwrap();
        assert_eq!(decoded, copy);
    }

    #[test]
    fn native_draft_has_distinct_identity_and_preserves_legacy_source() {
        let original = Sequence::new("original", FrameRate::FPS_30, 32, 32);
        let before = serde_json::to_value(&original).unwrap();
        let copy =
            native_conversion_copy(&original, NativeManagedColorConfig::sdr_draft()).unwrap();
        assert_ne!(original.id, copy.id);
        assert_eq!(serde_json::to_value(&original).unwrap(), before);
        let serialized = serde_json::to_value(&copy).unwrap();
        assert_eq!(serialized["color"]["mode"], "native_managed");
        assert_eq!(serialized["color"]["configuration"]["display"], "srgb_sdr");
        assert_eq!(
            serialized["color"]["configuration"]["export"],
            "bt709_video_sdr"
        );
        assert!(serialized["color"]["configuration"].get("ocio").is_none());
        let decoded: Sequence = serde_json::from_value(serialized).unwrap();
        assert_eq!(decoded, copy);
        assert!(native_conversion_copy(&copy, NativeManagedColorConfig::sdr_draft()).is_err());
        let mut wrong_display = NativeManagedColorConfig::sdr_draft();
        wrong_display.display = NativeOutputTransform::Bt709VideoSdr;
        assert!(wrong_display.validate().is_err());
        // Older drafts with an sRGB export selection still round-trip. Export
        // remains gated until the dedicated video transform is qualified.
        let mut older = NativeManagedColorConfig::sdr_draft();
        older.export = NativeOutputTransform::SrgbSdr;
        assert!(older.validate().is_ok());
        assert_ne!(
            copy.color.cache_identity().unwrap(),
            SequenceColorConfig::NativeManaged(Box::new(older))
                .cache_identity()
                .unwrap()
        );
        let mut future = NativeManagedColorConfig::sdr_draft();
        future.transform_revision = 2;
        assert!(native_conversion_copy(&original, future.clone()).is_err());
        assert_ne!(
            copy.color.cache_identity().unwrap(),
            SequenceColorConfig::NativeManaged(Box::new(future))
                .cache_identity()
                .unwrap()
        );
    }

    #[test]
    fn native_srgb_still_interpretation_requires_full_rgb_and_image_kind() {
        let mut input = NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::SrgbDisplay,
            range: InputSignalRange::Full,
            matrix: InputMatrix::Rgb,
            chroma_location: None,
        };
        assert!(input
            .validate_asset_kind(super::super::media::AssetKind::Image)
            .is_ok());
        assert!(input
            .validate_asset_kind(super::super::media::AssetKind::Video)
            .is_err());
        let encoded = serde_json::to_string(&input).unwrap();
        assert_eq!(
            serde_json::from_str::<NativeInputColorInterpretation>(&encoded).unwrap(),
            input
        );
        input.range = InputSignalRange::Limited;
        assert!(input.validate().is_err());
        input.range = InputSignalRange::Full;
        input.chroma_location = Some(NativeChromaLocation::Center);
        assert!(input.validate().is_err());
        input.chroma_location = None;
        input.matrix = InputMatrix::Bt709;
        assert!(input.validate().is_err());
    }

    #[test]
    fn native_input_requires_explicit_matching_source_codes() {
        let mut input = NativeInputColorInterpretation {
            hlg_peak_nits: None,
            reference_white_nits: None,
            version: 1,
            standard: NativeInputStandard::Bt709Scene,
            range: InputSignalRange::Limited,
            matrix: InputMatrix::Bt709,
            chroma_location: None,
        };
        assert!(input.validate().is_ok());
        input.range = InputSignalRange::FromMetadata;
        assert!(input.validate().is_err());
        input.range = InputSignalRange::Full;
        input.matrix = InputMatrix::Bt2020NonConstant;
        assert!(input.validate().is_err());
        input.standard = NativeInputStandard::Bt2020Scene;
        assert!(input.validate().is_ok());
        input.version = 2;
        assert!(input.validate().is_err());
    }

    #[test]
    fn native_chroma_location_is_additive_and_round_trips() {
        let legacy = r#"{"version":1,"standard":"bt709_scene","range":"limited","matrix":"bt709"}"#;
        let mut input: NativeInputColorInterpretation = serde_json::from_str(legacy).unwrap();
        assert_eq!(input.chroma_location, None);
        assert_eq!(serde_json::to_string(&input).unwrap(), legacy);
        input.chroma_location = Some(NativeChromaLocation::BottomLeft);
        assert!(serde_json::to_string(&input)
            .unwrap()
            .contains("\"bottomleft\""));
        let restored: NativeInputColorInterpretation =
            serde_json::from_str(&serde_json::to_string(&input).unwrap()).unwrap();
        assert_eq!(restored, input);
        assert_eq!(
            serde_json::from_str::<NativeChromaLocation>("\"top_left\"").unwrap(),
            NativeChromaLocation::TopLeft
        );
    }

    #[test]
    fn hashes_export_intent_and_versions_participate_in_cache_identity() {
        let initial = SequenceColorConfig::Managed(Box::new(config()));
        let mut changed = config();
        changed.ocio.sha256 = "b".repeat(64).try_into().unwrap();
        assert_ne!(
            initial.cache_identity().unwrap(),
            SequenceColorConfig::Managed(Box::new(changed))
                .cache_identity()
                .unwrap()
        );
        let mut changed = config();
        changed.export = ExportColorTransform::ColorSpace {
            name: "ACEScg".into(),
        };
        assert_ne!(
            initial.cache_identity().unwrap(),
            SequenceColorConfig::Managed(Box::new(changed))
                .cache_identity()
                .unwrap()
        );
        let mut future = config();
        future.version = 2;
        assert!(future.validate().is_err());
        assert_eq!(
            serde_json::from_value::<ManagedColorConfig>(serde_json::to_value(&future).unwrap())
                .unwrap(),
            future
        );
    }

    #[test]
    fn input_precedence_requires_matching_config_and_explicit_assumptions() {
        let mut config = config();
        let asset = InputColorInterpretation {
            config_sha256: config.ocio.sha256.clone(),
            color_space: "ACEScg".into(),
            range: InputSignalRange::Full,
            matrix: InputMatrix::Rgb,
        };
        let mut clip = asset.clone();
        clip.color_space = "ACEScct".into();
        assert_eq!(
            config.resolve_input(Some(&clip), Some(&asset)).unwrap(),
            ResolvedInput::Explicit(&clip)
        );
        assert_eq!(
            config.resolve_input(None, Some(&asset)).unwrap(),
            ResolvedInput::Explicit(&asset)
        );
        assert!(config.resolve_input(None, None).is_err());
        clip.config_sha256 = "b".repeat(64).try_into().unwrap();
        assert!(
            config.resolve_input(Some(&clip), Some(&asset)).is_err(),
            "mismatched override must not silently fall back to the asset"
        );
        config.unknown_input = UnknownInputPolicy::AssumeColorSpace {
            name: "sRGB".into(),
        };
        assert_eq!(
            config.resolve_input(None, None).unwrap(),
            ResolvedInput::Assumed("sRGB")
        );
    }

    #[test]
    fn rejects_unpinned_resources_and_unsafe_config_relative_paths() {
        assert!(ColorDigest::try_from("not a hash".to_owned()).is_err());
        for path in [
            "../look.cube",
            "/look.cube",
            "C:\\look.cube",
            "looks//a.cube",
        ] {
            let mut config = config();
            config
                .ocio
                .transform_assets
                .insert(path.into(), "a".repeat(64).try_into().unwrap());
            assert!(config.validate().is_err(), "{path}");
        }
    }

    #[test]
    fn lut_interpretation_preserves_legacy_and_rejects_technical_or_named_spaces() {
        let legacy = LutColorInterpretation::legacy_creative();
        assert!(legacy.validate_for_legacy_grade().is_ok());
        assert_eq!(
            serde_json::from_value::<LutColorInterpretation>(
                serde_json::to_value(&legacy).unwrap()
            )
            .unwrap(),
            legacy
        );
        let mut technical = legacy.clone();
        technical.purpose = LutPurpose::Technical;
        assert!(technical.validate_for_legacy_grade().is_err());
        let mut named = legacy.clone();
        named.input = LutColorSpace::Ocio {
            config_sha256: "a".repeat(64).try_into().unwrap(),
            color_space: "ACEScct".into(),
        };
        assert!(named.validate().is_ok());
        assert!(named.validate_for_legacy_grade().is_err());
        named.input = LutColorSpace::Native {
            transform_revision: 1,
            space: NativeLutSpace::Acescct,
        };
        assert!(named.validate().is_ok());
        assert!(named.validate_for_legacy_grade().is_err());
        named.input = LutColorSpace::Native {
            transform_revision: 2,
            space: NativeLutSpace::Acescct,
        };
        assert!(named.validate().is_err());
        named.version = 2;
        assert!(named.validate().is_err());
    }
}
