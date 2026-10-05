//! Runtime qualification gate for versioned sequence color intent.
//!
//! Managed authoring metadata can be preserved while Photonic-owned source,
//! working-space, grading, display and export transforms are qualified. It
//! must never select the existing SDR renderer as an implicit fallback.

use photonic_core::timeline::{
    color::{
        InputColorInterpretation, InputMatrix, InputSignalRange, ResolvedInput, SequenceColorConfig,
    },
    AssetKind, Clip, ClipSource, MediaAsset, Sequence, TimelineProject,
};
use serde::Serialize;

/// Photonic-owned scalar color-transform reference. Not yet connected to the
/// managed renderer; unsupported managed sequences still fail closed.
pub mod native;
pub mod native_output;

/// Checked bridge from persisted native source intent to the isolated GPU
/// decoder. No metadata fallback or BT.601 substitution is allowed.
pub fn native_gpu_source(
    input: &photonic_core::timeline::color::NativeInputColorInterpretation,
    subsampled_chroma: bool,
) -> Result<
    (
        photonic_render::video::NativeYuvInput,
        photonic_render::color::Range,
        photonic_render::video::NativeChromaLocation,
    ),
    String,
> {
    use photonic_core::timeline::color::{NativeChromaLocation, NativeInputStandard};
    input.validate()?;
    if subsampled_chroma && input.chroma_location.is_none() {
        return Err("subsampled source requires an explicit chroma sample location".into());
    }
    let standard = match input.standard {
        NativeInputStandard::SrgbDisplay => {
            return Err("sRGB still interpretation cannot decode YUV video".into())
        }
        NativeInputStandard::Bt709Scene => photonic_render::video::NativeYuvInput::Bt709Scene,
        NativeInputStandard::Bt2020Scene => photonic_render::video::NativeYuvInput::Bt2020Scene,
        NativeInputStandard::Bt2100HlgScene => {
            photonic_render::video::NativeYuvInput::Bt2100HlgScene {
                reference_white_nits: input.reference_white_nits.expect("validated HLG white"),
                display_peak_nits: input.hlg_peak_nits.expect("validated HLG peak"),
            }
        }
        NativeInputStandard::Bt2100PqDisplay => {
            photonic_render::video::NativeYuvInput::Bt2100PqDisplay {
                reference_white_nits: input
                    .reference_white_nits
                    .expect("validated PQ normalization"),
            }
        }
    };
    let range = match input.range {
        InputSignalRange::Full => photonic_render::color::Range::Full,
        InputSignalRange::Limited => photonic_render::color::Range::Limited,
        InputSignalRange::FromMetadata => unreachable!("validated native input is explicit"),
    };
    let chroma = match input
        .chroma_location
        .unwrap_or(NativeChromaLocation::Center)
    {
        NativeChromaLocation::Left => photonic_render::video::NativeChromaLocation::Left,
        NativeChromaLocation::Center => photonic_render::video::NativeChromaLocation::Center,
        NativeChromaLocation::TopLeft => photonic_render::video::NativeChromaLocation::TopLeft,
        NativeChromaLocation::Top => photonic_render::video::NativeChromaLocation::Top,
        NativeChromaLocation::BottomLeft => {
            photonic_render::video::NativeChromaLocation::BottomLeft
        }
        NativeChromaLocation::Bottom => photonic_render::video::NativeChromaLocation::Bottom,
    };
    Ok((standard, range, chroma))
}

/// Initial native still decoder qualification: 8/16-bit PNG and 8-bit JPEG.
/// Inspect magic bytes rather than trusting an extension; EXR/HDR must not be
/// silently quantized through the legacy image decoder.
pub fn validate_native_still_file(path: &std::path::Path) -> Result<(), String> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| format!("native still source: {e}"))?
        .with_guessed_format()
        .map_err(|e| format!("native still format: {e}"))?;
    if !matches!(
        reader.format(),
        Some(image::ImageFormat::Png | image::ImageFormat::Jpeg)
    ) {
        return Err("native sRGB still decoder currently qualifies PNG and JPEG; other formats require a precision-preserving input path".into());
    }
    Ok(())
}

/// Convert a decoded frame using the authored native input contract. Plane
/// layout, rather than a caller-provided flag or advisory probe tag, decides
/// whether subsampled chroma siting is mandatory. Managed graph entry remains
/// gated until the display/output path is qualified.
pub fn convert_native_decoded_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    converter: &photonic_render::video::YuvConverter,
    frame: &crate::decode::DecodedFrame,
    input: &photonic_core::timeline::color::NativeInputColorInterpretation,
) -> Result<wgpu::Texture, String> {
    convert_native_decoded_frame_to_size(
        device,
        queue,
        converter,
        frame,
        input,
        frame.planes.dims(),
    )
}

/// Variant for a padded graph-pool bucket. The source's logical dimensions
/// still determine subsampling and UVs; pixels outside them remain transparent.
pub fn convert_native_decoded_frame_to_size(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    converter: &photonic_render::video::YuvConverter,
    frame: &crate::decode::DecodedFrame,
    input: &photonic_core::timeline::color::NativeInputColorInterpretation,
    output_size: (u32, u32),
) -> Result<wgpu::Texture, String> {
    let subsampled = matches!(
        &frame.planes,
        crate::decode::DecodedPlanes::Yuv420 { .. }
            | crate::decode::DecodedPlanes::Yuv422 { .. }
            | crate::decode::DecodedPlanes::Yuv420P16 { .. }
            | crate::decode::DecodedPlanes::Yuv422P16 { .. }
    );
    let (standard, range, chroma_location) = native_gpu_source(input, subsampled)?;
    Ok(converter.convert_native_to_size(
        device,
        queue,
        &frame.planes.as_yuv_planes(),
        standard,
        range,
        chroma_location,
        output_size,
    ))
}

/// Native scalar reference for the ACEScct AP1 transfer. Kept independent of
/// OCIO and of the GPU implementation so both can be checked against it.
/// These functions operate on one channel; callers preserve alpha separately.
pub mod acescct {
    const SLOPE: f64 = 10.540_237_741_654_5;
    const OFFSET: f64 = 0.072_905_534_195_835_5;
    const LINEAR_BREAK: f64 = 0.007_812_5;
    const LOG_BREAK: f64 = 0.155_251_141_552_511;
    const HALF_MAX: f64 = 65_504.0;

    pub fn encode(linear_ap1: f64) -> f64 {
        if linear_ap1 <= LINEAR_BREAK {
            linear_ap1.mul_add(SLOPE, OFFSET)
        } else {
            (linear_ap1.log2() + 9.72) / 17.52
        }
    }

    pub fn decode(acescct: f64) -> f64 {
        if acescct <= LOG_BREAK {
            (acescct - OFFSET) / SLOPE
        } else if acescct >= (HALF_MAX.log2() + 9.72) / 17.52 {
            HALF_MAX
        } else {
            2.0_f64.powf(acescct.mul_add(17.52, -9.72))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn reference_points_and_extended_range() {
            let cases = [
                (-0.1, -0.981_118_239_969_614_5),
                (0.0, 0.072_905_534_195_835_5),
                (LINEAR_BREAK, LOG_BREAK),
                (0.18, 0.413_588_402_492_442_3),
                (1.0, 0.554_794_520_547_945_2),
            ];
            for (linear, expected) in cases {
                let encoded = encode(linear);
                assert!((encoded - expected).abs() < 1e-10, "{linear}: {encoded}");
                assert!((decode(encoded) - linear).abs() < 1e-10);
            }
            assert!(encode(16.0) > 0.7);
            assert_eq!(decode(2.0), HALF_MAX);
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ColorPipelineStatus {
    pub mode: &'static str,
    /// `available` describes full sequence playback and export. This field
    /// reports the narrower interactive preview capability separately.
    pub preview_mode: &'static str,
    /// Qualified delivery subset; `available` remains false until the whole
    /// managed timeline and export surface are supported.
    pub delivery_mode: &'static str,
    pub available: bool,
    pub working_space: &'static str,
    pub grading_space: &'static str,
    pub diagnostic: Option<String>,
}

/// Source interpretation preview for a managed sequence. This is usable before
/// the managed renderer is available and does not mutate project state.
#[derive(Clone, Debug, Serialize)]
pub struct ColorInputFinding {
    pub clip: photonic_core::timeline::ClipId,
    pub asset: photonic_core::timeline::AssetId,
    pub interpretation: &'static str,
    pub color_space: Option<String>,
    pub diagnostic: Option<String>,
}

/// An explicit OCIO space is insufficient when the user also asks to inherit
/// range or YUV matrix from absent source tags. Preserve that unresolved state
/// in preflight so a future managed renderer cannot guess silently.
fn missing_input_metadata(input: &InputColorInterpretation, media: &MediaAsset) -> Option<String> {
    if media.kind != AssetKind::Video {
        return None;
    }
    let color = media
        .probe
        .as_ref()
        .and_then(|probe| probe.video.as_ref())
        .map(|video| &video.color);
    let mut missing = Vec::new();
    if input.range == InputSignalRange::FromMetadata
        && color.and_then(|value| value.full_range).is_none()
    {
        missing.push("signal range");
    }
    if input.matrix == InputMatrix::FromMetadata
        && color.and_then(|value| value.matrix.as_ref()).is_none()
    {
        missing.push("YUV matrix");
    }
    (!missing.is_empty()).then(|| {
        format!(
            "source metadata lacks {}; choose an explicit input value",
            missing.join(" and ")
        )
    })
}

fn missing_assumed_video_metadata(media: &MediaAsset) -> Option<String> {
    if media.kind != AssetKind::Video {
        return None;
    }
    let color = media
        .probe
        .as_ref()
        .and_then(|probe| probe.video.as_ref())
        .map(|video| &video.color);
    let mut missing = Vec::new();
    if color.and_then(|value| value.full_range).is_none() {
        missing.push("signal range");
    }
    if color.and_then(|value| value.matrix.as_ref()).is_none() {
        missing.push("YUV matrix");
    }
    (!missing.is_empty()).then(|| {
        format!(
            "assumed color space still lacks source {}; add probe metadata or an explicit input interpretation",
            missing.join(" and ")
        )
    })
}

pub fn inspect_inputs(project: &TimelineProject, sequence: &Sequence) -> Vec<ColorInputFinding> {
    if sequence.color.is_legacy() {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for track in &sequence.video_tracks {
        if !track.enabled || !track.kind.is_visual() {
            continue;
        }
        for clip in &track.clips {
            if !clip.enabled {
                continue;
            }
            if let Some(finding) = inspect_input(project, sequence, clip) {
                findings.push(finding);
            }
        }
    }
    findings
}

pub fn inspect_input(
    project: &TimelineProject,
    sequence: &Sequence,
    clip: &Clip,
) -> Option<ColorInputFinding> {
    if matches!(&sequence.color, SequenceColorConfig::NativeManaged(_)) {
        let asset = match clip.source {
            ClipSource::Asset { asset } | ClipSource::Vector { asset } => asset,
            _ => return None,
        };
        let media = project.media.assets.get(&asset);
        let input = clip
            .native_input_color
            .as_ref()
            .or_else(|| media.and_then(|asset| asset.native_input_color.as_ref()));
        let (interpretation, color_space, diagnostic) = match (media, input) {
            (None, _) => ("unresolved", None, Some("source asset is missing".into())),
            (Some(media), Some(input)) => match input.validate_asset_kind(media.kind).and_then(|()| {
                if media.kind == AssetKind::Image {
                    let photonic_core::timeline::AssetSource::File { path, .. } = &media.source else { return Err("native still must be file-backed".into()); };
                    return validate_native_still_file(path);
                }
                let pixel_format = media
                    .probe
                    .as_ref()
                    .and_then(|probe| probe.pixel_format.as_deref());
                let Some(format) = pixel_format else {
                    return Err("native broadcast input requires a probed source pixel format to preserve bit depth and chroma sampling".into());
                };
                if !crate::decode::PixFmt::supports_native_source(format) {
                    return Err(format!(
                        "native broadcast input does not support source pixel format {format}; select a supported YUV interpretation or keep this sequence in Legacy SDR"
                    ));
                }
                let subsampled = pixel_format.is_some_and(|format| {
                        let format = format.to_ascii_lowercase();
                        format.contains("420")
                            || format.contains("422")
                            || format.starts_with("nv12")
                            || format.starts_with("nv21")
                            || format.starts_with("nv16")
                            || format.starts_with("nv61")
                            || format.starts_with("p010")
                            || format.starts_with("p012")
                            || format.starts_with("p016")
                            || format.starts_with("p210")
                            || format.starts_with("p212")
                            || format.starts_with("p216")
                    });
                if subsampled && input.chroma_location.is_none() {
                    let reported = media.probe.as_ref()
                        .and_then(|probe| probe.video.as_ref())
                        .and_then(|video| video.color.chroma_location.as_deref());
                    Err(match reported {
                        Some(value) => format!(
                            "subsampled source requires an explicit chroma sample location (probe reports {value})"
                        ),
                        None => "subsampled source requires an explicit chroma sample location".into(),
                    })
                } else {
                    Ok(())
                }
            }) {
                Ok(()) => (
                    "explicit",
                    Some(match input.standard {
                        photonic_core::timeline::color::NativeInputStandard::SrgbDisplay => "sRGB display still".into(),
                        photonic_core::timeline::color::NativeInputStandard::Bt709Scene => {
                            "BT.709 scene".into()
                        }
                        photonic_core::timeline::color::NativeInputStandard::Bt2020Scene => {
                            "BT.2020 scene".into()
                        }
                        photonic_core::timeline::color::NativeInputStandard::Bt2100HlgScene => format!("BT.2100 HLG scene · white {} / peak {} nits", input.reference_white_nits.unwrap_or(0), input.hlg_peak_nits.unwrap_or(0)),
                        photonic_core::timeline::color::NativeInputStandard::Bt2100PqDisplay => format!("BT.2100 PQ display · reference white {} nits", input.reference_white_nits.unwrap_or(0)),
                    }),
                    None,
                ),
                Err(error) => ("unresolved", None, Some(error)),
            },
            _ => (
                "unresolved",
                None,
                Some(
                    "native source interpretation is missing; OCIO input overrides are not reused"
                        .into(),
                ),
            ),
        };
        return Some(ColorInputFinding {
            clip: clip.id,
            asset,
            interpretation,
            color_space,
            diagnostic,
        });
    }
    let SequenceColorConfig::Managed(config) = &sequence.color else {
        return None;
    };
    let asset = match clip.source {
        ClipSource::Asset { asset } | ClipSource::Vector { asset } => asset,
        _ => return None,
    };
    let Some(media) = project.media.assets.get(&asset) else {
        return Some(ColorInputFinding {
            clip: clip.id,
            asset,
            interpretation: "unresolved",
            color_space: None,
            diagnostic: Some("source asset is missing".into()),
        });
    };
    Some(
        match config.resolve_input(clip.input_color.as_ref(), media.input_color.as_ref()) {
            Ok(ResolvedInput::Explicit(input)) => match missing_input_metadata(input, media) {
                Some(diagnostic) => ColorInputFinding {
                    clip: clip.id,
                    asset,
                    interpretation: "unresolved",
                    color_space: Some(input.color_space.clone()),
                    diagnostic: Some(diagnostic),
                },
                None => ColorInputFinding {
                    clip: clip.id,
                    asset,
                    interpretation: "explicit",
                    color_space: Some(input.color_space.clone()),
                    diagnostic: None,
                },
            },
            Ok(ResolvedInput::Assumed(name)) => match missing_assumed_video_metadata(media) {
                Some(diagnostic) => ColorInputFinding {
                    clip: clip.id,
                    asset,
                    interpretation: "unresolved",
                    color_space: Some(name.to_owned()),
                    diagnostic: Some(diagnostic),
                },
                None => ColorInputFinding {
                    clip: clip.id,
                    asset,
                    interpretation: "assumed",
                    color_space: Some(name.to_owned()),
                    diagnostic: Some(
                        "using the sequence's explicit unknown-source assumption".into(),
                    ),
                },
            },
            Err(error) => ColorInputFinding {
                clip: clip.id,
                asset,
                interpretation: "unresolved",
                color_space: None,
                diagnostic: Some(error),
            },
        },
    )
}

pub fn inspect(sequence: &Sequence) -> ColorPipelineStatus {
    match &sequence.color {
        SequenceColorConfig::LegacySdr => ColorPipelineStatus {
            mode: "legacy_sdr", preview_mode: "full", delivery_mode: "full", available: true,
            working_space: "linear_rec709", grading_space: "legacy_operator_defined",
            diagnostic: None,
        },
        SequenceColorConfig::Managed(config) => ColorPipelineStatus {
            mode: "managed", preview_mode: "unavailable", delivery_mode: "unavailable", available: false,
            working_space: "ACEScg", grading_space: "ACEScct",
            diagnostic: Some(match config.validate() {
                Err(error) => format!("Sequence {} has an invalid color configuration: {error}", sequence.id),
                Ok(()) => format!("Sequence {} requires managed color, which is not available in this build. Open the original Legacy SDR sequence if this sequence was created by conversion.", sequence.id),
            }),
        },
        SequenceColorConfig::NativeManaged(config) => ColorPipelineStatus {
            mode: "native_managed", preview_mode: if config.validate().is_ok() { "qualified_video_tracks_sdr" } else { "unavailable" },
            delivery_mode: if config.validate().is_ok() && config.export == photonic_core::timeline::color::NativeOutputTransform::Bt709VideoSdr { "qualified_prores_mov_sdr" } else { "unavailable" }, available: false,
            working_space: "ACEScg", grading_space: "ACEScct",
            diagnostic: Some(match config.validate() {
                Err(error) => format!("Sequence {} has an invalid native color configuration: {error}", sequence.id),
                Ok(()) => format!("Sequence {} supports qualified video-track SDR previews and, with a BT.709 export transform, full-resolution ProRes MOV delivery. Full managed timeline support and other export formats remain unavailable; unsupported stages fail with a diagnostic.", sequence.id),
            }),
        },
    }
}

/// Return a visible diagnostic before any unsupported sequence is rendered.
pub fn ensure_supported(sequence: &Sequence) -> Result<(), String> {
    match inspect(sequence).diagnostic {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
