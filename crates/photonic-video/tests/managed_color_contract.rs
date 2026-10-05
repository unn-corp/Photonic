//! Managed intent is preserved but cannot be misrendered by a Legacy SDR build.
use photonic_core::{
    timeline::{
        color::{
            InputColorInterpretation, InputMatrix, InputSignalRange, ManagedColorConfig,
            NativeChromaLocation, NativeInputColorInterpretation, NativeInputStandard,
            NativeManagedColorConfig, SequenceColorConfig,
        },
        *,
    },
    Color,
};
use photonic_video::{
    export::{job::resolve_export_job, presets::built_in_presets},
    graph::{
        compile::{compile, CompileCode, Quality},
        ir::IrOp,
    },
    ExportJob,
};

fn config() -> ManagedColorConfig {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/color/managed-v1.json"
    ))
    .unwrap()
}

fn project() -> (TimelineProject, SequenceId) {
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("legacy", FrameRate::FPS_30, 32, 32);
    let mut track = Track::new(TrackKind::Video, "V1");
    track.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color::rgb(0.18, 0.18, 0.18),
        },
        Tick(0),
        Tick::from_seconds(1),
    ));
    sequence.video_tracks.push(track);
    let id = project.insert_sequence(sequence);
    (project, id)
}

#[test]
fn managed_intent_never_falls_through_to_legacy_rendering_or_export() {
    let (mut project, id) = project();
    let legacy = compile(&project, id, 0, Tick(0), Quality::FULL, None);
    assert!(legacy.diagnostics.is_empty());
    let job = ExportJob {
        sequence: id,
        format_index: 0,
        preset: built_in_presets()
            .into_iter()
            .find(|p| p.name == "Web H.264")
            .unwrap(),
        output: "unused-color-test.mp4".into(),
        range: None,
        options: Default::default(),
    };
    assert!(resolve_export_job(&project, &job).is_ok());
    project.sequences.get_mut(&id).unwrap().color =
        SequenceColorConfig::Managed(Box::new(config()));
    let compiled = compile(&project, id, 0, Tick(0), Quality::FULL, None);
    assert!(compiled
        .diagnostics
        .iter()
        .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
    assert_eq!(
        compiled.graph.nodes.len(),
        2,
        "only placeholder and output may render"
    );
    assert!(matches!(compiled.graph.nodes[0].op, IrOp::SolidColor { color } if color.a == 0.0));
    assert!(resolve_export_job(&project, &job)
        .unwrap_err()
        .to_string()
        .contains("managed color"));
    let status = photonic_video::color::inspect(&project.sequences[&id]);
    assert_eq!(status.mode, "managed");
    assert_eq!(status.preview_mode, "unavailable");
    assert_eq!(status.delivery_mode, "unavailable");
    assert!(!status.available);
}

#[test]
fn native_managed_draft_keeps_full_compiler_and_unqualified_export_gated() {
    let (mut project, id) = project();
    project.sequences.get_mut(&id).unwrap().color =
        SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
    let compiled = compile(&project, id, 0, Tick(0), Quality::FULL, None);
    assert!(compiled
        .diagnostics
        .iter()
        .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
    assert_eq!(compiled.graph.nodes.len(), 2);
    let status = photonic_video::color::inspect(&project.sequences[&id]);
    assert_eq!(status.mode, "native_managed");
    assert_eq!(status.preview_mode, "qualified_video_tracks_sdr");
    assert_eq!(status.delivery_mode, "qualified_prores_mov_sdr");
    assert!(!status.available);
    assert!(status
        .diagnostic
        .unwrap()
        .contains("Full managed timeline support"));
    let job = ExportJob {
        sequence: id,
        format_index: 0,
        preset: built_in_presets()
            .into_iter()
            .find(|p| p.name == "Web H.264")
            .unwrap(),
        output: "unused-native-color-test.mp4".into(),
        range: None,
        options: Default::default(),
    };
    assert!(resolve_export_job(&project, &job)
        .unwrap_err()
        .to_string()
        .contains("full-resolution, original-media ProRes MOV"));
    let mut qualified_preset = job.clone();
    qualified_preset.preset = built_in_presets()
        .into_iter()
        .find(|p| p.name == "ProRes Mezzanine")
        .unwrap();
    qualified_preset.preset.audio = None;
    assert!(resolve_export_job(&project, &qualified_preset)
        .unwrap_err()
        .to_string()
        .contains("native delivery first frame is unavailable"));
    if let SequenceColorConfig::NativeManaged(config) =
        &mut project.sequences.get_mut(&id).unwrap().color
    {
        config.export = photonic_core::timeline::color::NativeOutputTransform::SrgbSdr;
    }
    assert!(resolve_export_job(&project, &qualified_preset)
        .unwrap_err()
        .to_string()
        .contains("BT.709 video-signal output transform"));
    if let SequenceColorConfig::NativeManaged(config) =
        &mut project.sequences.get_mut(&id).unwrap().color
    {
        config.export = photonic_core::timeline::color::NativeOutputTransform::Bt709VideoSdr;
    }
    let (mut outer_project, outer) = self::project();
    let native_copy = photonic_core::timeline::color::native_conversion_copy(
        &outer_project.sequences[&outer],
        NativeManagedColorConfig::sdr_draft(),
    )
    .unwrap();
    let nested = native_copy.id;
    outer_project.insert_sequence(native_copy);
    outer_project
        .sequences
        .get_mut(&outer)
        .unwrap()
        .video_tracks[0]
        .clips[0]
        .source = ClipSource::NestedSequence { sequence: nested };
    let nested_compile = compile(&outer_project, outer, 0, Tick(0), Quality::FULL, None);
    assert!(nested_compile
        .diagnostics
        .iter()
        .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
}

#[test]
fn native_draft_never_reuses_ocio_source_interpretation() {
    let (mut project, id) = project();
    let cfg = config();
    let mut asset = MediaAsset::from_file(AssetKind::Video, "unused-native-input.mov");
    let asset_id = asset.id;
    asset.input_color = Some(InputColorInterpretation {
        config_sha256: cfg.ocio.sha256,
        color_space: "ACEScg".into(),
        range: InputSignalRange::Full,
        matrix: InputMatrix::Rgb,
    });
    project.media.assets.insert(asset_id, asset);
    project.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0].source =
        ClipSource::Asset { asset: asset_id };
    project.sequences.get_mut(&id).unwrap().color =
        SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].interpretation, "unresolved");
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("OCIO input overrides are not reused"));
    project
        .media
        .assets
        .get_mut(&asset_id)
        .unwrap()
        .native_input_color = Some(NativeInputColorInterpretation {
        hlg_peak_nits: None,
        reference_white_nits: None,
        version: 1,
        standard: NativeInputStandard::Bt709Scene,
        range: InputSignalRange::Limited,
        matrix: InputMatrix::Bt709,
        chroma_location: None,
    });
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "unresolved");
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("probed source pixel format"));
    let media = project.media.assets.get_mut(&asset_id).unwrap();
    let mut probe = MediaProbe::basic(Tick::from_seconds(1), "mov", "h264");
    probe.pixel_format = Some("yuv420p".into());
    probe.video = Some(VideoStreamInfo {
        width: 32,
        height: 32,
        frame_rate: FrameRate::FPS_30,
        pixel_aspect: 1.0,
        color: ProbedColor {
            chroma_location: Some("left".into()),
            ..Default::default()
        },
        keyframe_index_cached: false,
        scan: Default::default(),
    });
    media.probe = Some(probe);
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "unresolved");
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("probe reports left"));
    for format in ["p012le", "P016LE"] {
        project
            .media
            .assets
            .get_mut(&asset_id)
            .unwrap()
            .probe
            .as_mut()
            .unwrap()
            .pixel_format = Some(format.into());
        assert_eq!(
            photonic_video::color::inspect_inputs(&project, &project.sequences[&id])[0]
                .interpretation,
            "unresolved",
            "{format} also needs explicit chroma siting"
        );
    }
    project
        .media
        .assets
        .get_mut(&asset_id)
        .unwrap()
        .native_input_color
        .as_mut()
        .unwrap()
        .chroma_location = Some(NativeChromaLocation::Left);
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "explicit");
    project
        .media
        .assets
        .get_mut(&asset_id)
        .unwrap()
        .probe
        .as_mut()
        .unwrap()
        .pixel_format = Some("gbrp10le".into());
    let unsupported = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(unsupported[0].interpretation, "unresolved");
    assert!(unsupported[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("gbrp10le"));
    project
        .media
        .assets
        .get_mut(&asset_id)
        .unwrap()
        .probe
        .as_mut()
        .unwrap()
        .pixel_format = Some("yuv422p18le".into());
    let unsupported_depth =
        photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(unsupported_depth[0].interpretation, "unresolved");
    assert!(unsupported_depth[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("yuv422p18le"));
    project
        .media
        .assets
        .get_mut(&asset_id)
        .unwrap()
        .probe
        .as_mut()
        .unwrap()
        .pixel_format = Some("p210le".into());
    assert_eq!(
        photonic_video::color::inspect_inputs(&project, &project.sequences[&id])[0].interpretation,
        "explicit"
    );
    assert!(compile(&project, id, 0, Tick(0), Quality::FULL, None)
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == Some(CompileCode::ColorPipelineUnavailable)));
}

#[test]
fn unsupported_nested_color_is_reported_without_blocking_unrelated_legacy_sequences() {
    let (mut project, original) = project();
    let mut copy =
        photonic_core::timeline::color::conversion_copy(&project.sequences[&original], config())
            .unwrap();
    let managed = copy.id;
    copy.name = "managed draft".into();
    project.insert_sequence(copy);
    assert!(compile(&project, original, 0, Tick(0), Quality::FULL, None)
        .diagnostics
        .is_empty());
    project.sequences.get_mut(&original).unwrap().video_tracks[0].clips[0].source =
        ClipSource::NestedSequence { sequence: managed };
    let compiled = compile(&project, original, 0, Tick(0), Quality::FULL, None);
    assert!(compiled
        .diagnostics
        .iter()
        .any(|d| d.code == Some(CompileCode::ColorPipelineUnavailable)));
}

#[test]
fn input_preflight_reports_missing_assumed_and_explicit_sources() {
    let (mut project, id) = project();
    let mut asset = MediaAsset::from_file(AssetKind::Video, "unused-input-test.mov");
    let asset_id = asset.id;
    let mut clip = Clip::new(ClipSource::Asset { asset: asset_id }, Tick(0), Tick(1));
    let clip_id = clip.id;
    project.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0] = clip.clone();
    let mut cfg = config();
    project.sequences.get_mut(&id).unwrap().color =
        SequenceColorConfig::Managed(Box::new(cfg.clone()));

    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].clip, clip_id);
    assert_eq!(findings[0].interpretation, "unresolved");
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("missing"));

    project.media.insert(asset.clone());
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("unknown"));

    cfg.unknown_input = photonic_core::timeline::color::UnknownInputPolicy::AssumeColorSpace {
        name: "sRGB - Texture".into(),
    };
    project.sequences.get_mut(&id).unwrap().color =
        SequenceColorConfig::Managed(Box::new(cfg.clone()));
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "unresolved");
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("signal range and YUV matrix"));
    let mut probe = MediaProbe::basic(Tick(1), "mov", "prores");
    probe.video = Some(VideoStreamInfo {
        width: 32,
        height: 32,
        frame_rate: FrameRate::FPS_30,
        pixel_aspect: 1.0,
        color: ProbedColor {
            matrix: Some("bt709".into()),
            full_range: Some(true),
            ..Default::default()
        },
        keyframe_index_cached: false,
        scan: Default::default(),
    });
    project.media.assets.get_mut(&asset_id).unwrap().probe = Some(probe.clone());
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "assumed");
    assert_eq!(findings[0].color_space.as_deref(), Some("sRGB - Texture"));

    asset.input_color = Some(photonic_core::timeline::color::InputColorInterpretation {
        config_sha256: cfg.ocio.sha256.clone(),
        color_space: "ACEScg".into(),
        range: photonic_core::timeline::color::InputSignalRange::Full,
        matrix: photonic_core::timeline::color::InputMatrix::Rgb,
    });
    project.media.assets.insert(asset_id, asset);
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "explicit");
    assert_eq!(findings[0].color_space.as_deref(), Some("ACEScg"));

    {
        let asset = project.media.assets.get_mut(&asset_id).unwrap();
        let input = asset.input_color.as_mut().unwrap();
        input.range = photonic_core::timeline::color::InputSignalRange::FromMetadata;
        input.matrix = photonic_core::timeline::color::InputMatrix::FromMetadata;
        asset.probe = None;
    }
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "unresolved");
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("signal range and YUV matrix"));
    clip.input_color = Some(photonic_core::timeline::color::InputColorInterpretation {
        config_sha256: cfg.ocio.sha256.clone(),
        color_space: "ACEScct".into(),
        range: photonic_core::timeline::color::InputSignalRange::Full,
        matrix: photonic_core::timeline::color::InputMatrix::Rgb,
    });
    project.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0] = clip.clone();
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "explicit");
    assert_eq!(findings[0].color_space.as_deref(), Some("ACEScct"));
    clip.input_color = None;
    project.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0] = clip.clone();
    {
        let asset = project.media.assets.get_mut(&asset_id).unwrap();
        asset.probe = Some(probe);
    }
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "explicit");

    clip.input_color = Some(photonic_core::timeline::color::InputColorInterpretation {
        config_sha256: photonic_core::timeline::color::ColorDigest::try_from("0".repeat(64))
            .unwrap(),
        color_space: "ACEScct".into(),
        range: photonic_core::timeline::color::InputSignalRange::Full,
        matrix: photonic_core::timeline::color::InputMatrix::Rgb,
    });
    project.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0] = clip;
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "unresolved");
    assert!(findings[0]
        .diagnostic
        .as_deref()
        .unwrap()
        .contains("different OCIO"));
}

#[test]
fn still_image_input_needs_no_video_range_or_matrix_tags() {
    let (mut project, id) = project();
    let mut cfg = config();
    cfg.unknown_input = photonic_core::timeline::color::UnknownInputPolicy::AssumeColorSpace {
        name: "sRGB - Texture".into(),
    };
    project.sequences.get_mut(&id).unwrap().color =
        SequenceColorConfig::Managed(Box::new(cfg.clone()));
    let mut asset = MediaAsset::from_file(AssetKind::Image, "still.png");
    let asset_id = asset.id;
    project.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0].source =
        ClipSource::Asset { asset: asset_id };
    project.media.insert(asset.clone());
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "assumed");

    asset.input_color = Some(photonic_core::timeline::color::InputColorInterpretation {
        config_sha256: cfg.ocio.sha256,
        color_space: "sRGB - Texture".into(),
        range: photonic_core::timeline::color::InputSignalRange::FromMetadata,
        matrix: photonic_core::timeline::color::InputMatrix::FromMetadata,
    });
    project.media.assets.insert(asset_id, asset);
    let findings = photonic_video::color::inspect_inputs(&project, &project.sequences[&id]);
    assert_eq!(findings[0].interpretation, "explicit");
}
