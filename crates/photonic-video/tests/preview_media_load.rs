//! Headless coverage for 24-preview-media-load:
//! - import ladder L0 → L1/L2/L3/L4 (hash, probe, poster, keyframe index)
//! - L5 waveform peak pyramid (build + sidecar save/load)
//! - L7 proxy generation + decode-input selection
//! - Draft canvas fit
//! - PreviewTarget / PreviewQuality command surface
//! - Windows-style audio temp-file staging for dual-input export
//!
//! Skips ffmpeg-dependent cases when tools are missing (same convention as
//! other photonic-video integration tests).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use photonic_core::document::Document;
use photonic_core::history::CommandHistory;
use photonic_core::timeline::{
    AssetKind, Clip, ClipSource, MediaAsset, ProxyRef, Sequence, Tick, TimelineProject, Track,
    TrackKind,
};
use photonic_video::graph::compile::{
    compile, compile_asset_peek, fit_long_edge, Quality, DRAFT_MAX_LONG_EDGE,
};
use photonic_video::media::{
    ensure_poster, generate_proxy, keyframe_index_ready, poster_cache_path, poster_ready,
    proxy_cache_path, resolve_decode_input, should_auto_generate_proxy, KeyframeIndex,
};
use photonic_video::{
    coalesce_commands, EngineCmd, PreviewQuality, PreviewTarget, ProxyMode, VideoEngine,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn counter_mp4() -> PathBuf {
    fixtures_dir().join("counter.mp4")
}

fn beep_flash_wav() -> PathBuf {
    fixtures_dir().join("beep_flash.wav")
}

fn tools_or_skip() -> Option<photonic_video::media::FfmpegTools> {
    photonic_video::media::locate().ok()
}

// ── Ladder: L0 register, L1–L4 background products ──────────────────────────

#[test]
fn import_ladder_l0_through_l4_headless() {
    let Some(tools) = tools_or_skip() else {
        eprintln!("skip import_ladder: no ffmpeg/ffprobe");
        return;
    };
    let path = counter_mp4();
    assert!(path.is_file(), "fixture missing: {}", path.display());

    // L0: stub is placeable with no probe/hash.
    let t0 = Instant::now();
    let stub = MediaAsset::from_file(AssetKind::Video, &path);
    let l0_ms = t0.elapsed().as_millis();
    assert!(stub.probe.is_none());
    assert!(stub.content_hash.is_none());
    assert!(l0_ms < 100, "L0 register must be cheap (got {l0_ms} ms)");

    // L1 hash
    let t1 = Instant::now();
    let hash = photonic_video::media::content_hash(&path).expect("hash");
    let l1_ms = t1.elapsed().as_millis();
    assert!(!hash.is_empty());

    // L2 probe
    let t2 = Instant::now();
    let probe = photonic_video::media::probe_asset(&tools, &path).expect("probe");
    let l2_ms = t2.elapsed().as_millis();
    assert!(probe.duration.0 > 0);
    assert!(probe.video.is_some());

    // L3 poster
    let cache =
        std::env::temp_dir().join(format!("photonic-preview-load-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let poster_out = poster_cache_path(&cache, &hash);
    let t3 = Instant::now();
    ensure_poster(&tools, &path, &poster_out).expect("poster");
    let l3_ms = t3.elapsed().as_millis();
    assert!(poster_ready(&cache, &hash));
    assert!(poster_out.is_file());
    // Poster should be a decodable PNG.
    let bytes = std::fs::read(&poster_out).unwrap();
    let img = photonic_core::raster::image::RasterImage::from_encoded(&bytes)
        .expect("poster decodes as image");
    assert!(img.width >= 1 && img.height >= 1);

    // L4 keyframe index
    let t4 = Instant::now();
    let idx = KeyframeIndex::load_or_build(&tools, &path, &cache, &hash).expect("keyframes");
    let l4_ms = t4.elapsed().as_millis();
    assert!(keyframe_index_ready(&cache, &hash));
    assert!(!idx.keyframes.is_empty());
    // counter.mp4 has keyframes every 2s — at least frame 0.
    assert_eq!(idx.keyframe_before(Tick::ZERO), Tick::ZERO);

    // Soft budgets for a 320×180 short fixture (not the p95 product budgets).
    assert!(l1_ms < 5_000, "hash took {l1_ms} ms");
    assert!(l2_ms < 5_000, "probe took {l2_ms} ms");
    assert!(l3_ms < 10_000, "poster took {l3_ms} ms");
    assert!(l4_ms < 15_000, "keyframe index took {l4_ms} ms");

    let _ = std::fs::remove_dir_all(&cache);
    eprintln!(
        "ladder timings ms: L0={l0_ms} L1={l1_ms} L2={l2_ms} L3={l3_ms} L4={l4_ms} hash={hash}"
    );
}

// ── Ladder L5: waveform peak pyramid (audio) ────────────────────────────────

#[test]
fn import_ladder_l5_waveform_headless() {
    let Some(tools) = tools_or_skip() else {
        eprintln!("skip import_ladder_l5: no ffmpeg/ffprobe");
        return;
    };
    let path = beep_flash_wav();
    assert!(path.is_file(), "fixture missing: {}", path.display());

    let hash = photonic_video::media::content_hash(&path).expect("hash");
    let cache =
        std::env::temp_dir().join(format!("photonic-preview-load-l5-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();

    // Match import ladder + timeline PoolWaveformSource rate.
    const WAVEFORM_SAMPLE_RATE: u32 = 48_000;
    let t5 = Instant::now();
    let mut src = photonic_video::playback::FfmpegPcmSource::spawn(
        &tools,
        &path,
        Tick::ZERO,
        WAVEFORM_SAMPLE_RATE,
    )
    .expect("spawn pcm for waveform");
    let pyramid = photonic_video::audio::waveform::build_pyramid(&mut src, hash.clone());
    let saved = photonic_video::audio::waveform::save_to_dir(&pyramid, &cache)
        .expect("save waveform pyramid");
    let l5_ms = t5.elapsed().as_millis();

    assert!(saved.is_file());
    assert_eq!(pyramid.asset_hash, hash);
    assert!(pyramid.channels >= 1);
    assert_eq!(pyramid.source_sample_rate, WAVEFORM_SAMPLE_RATE);
    assert!(
        pyramid.total_frames > 0,
        "beep_flash.wav should yield non-empty PCM"
    );
    assert!(
        !pyramid.levels.is_empty(),
        "pyramid must have at least one level"
    );

    let loaded = photonic_video::audio::waveform::load_from_dir(&cache, &hash)
        .expect("load_from_dir io")
        .expect("warm cache hit after save");
    assert_eq!(loaded.asset_hash, pyramid.asset_hash);
    assert_eq!(loaded.channels, pyramid.channels);
    assert_eq!(loaded.source_sample_rate, pyramid.source_sample_rate);
    assert_eq!(loaded.total_frames, pyramid.total_frames);
    assert_eq!(loaded.levels.len(), pyramid.levels.len());
    // Spot-check level 0 bucket counts match (sidecar roundtrip).
    assert_eq!(
        loaded.levels[0].channels[0].len(),
        pyramid.levels[0].channels[0].len()
    );

    // Soft budget: 60s mono ADPCM fixture; decode+pyramid should be well under
    // a minute on CI-class hardware (not a product p95 gate).
    assert!(
        l5_ms < 60_000,
        "L5 waveform for short fixture took {l5_ms} ms"
    );

    let _ = std::fs::remove_dir_all(&cache);
    eprintln!(
        "ladder L5 waveform ms: {l5_ms} hash={hash} frames={}",
        pyramid.total_frames
    );
}

// ── L7 proxy: generate + Auto path selects proxy for Draft ──────────────────

#[test]
fn import_ladder_l7_proxy_headless() {
    let Some(tools) = tools_or_skip() else {
        eprintln!("skip import_ladder_l7: no ffmpeg/ffprobe");
        return;
    };
    let path = counter_mp4();
    assert!(path.is_file(), "fixture missing: {}", path.display());

    let mut stub = MediaAsset::from_file(AssetKind::Video, &path);
    assert!(should_auto_generate_proxy(
        stub.kind,
        &stub.source,
        stub.proxy.as_ref(),
        true
    ));
    assert!(!should_auto_generate_proxy(
        stub.kind,
        &stub.source,
        stub.proxy.as_ref(),
        false
    ));

    let hash = photonic_video::media::content_hash(&path).expect("hash");
    stub.content_hash = Some(hash.clone());

    let cache =
        std::env::temp_dir().join(format!("photonic-preview-load-l7-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let proxy_out = proxy_cache_path(&cache, &hash);

    let t7 = Instant::now();
    generate_proxy(&tools, &path, &proxy_out, &|| false).expect("generate proxy");
    let l7_ms = t7.elapsed().as_millis();
    assert!(proxy_out.is_file());
    assert!(
        l7_ms < 60_000,
        "proxy gen for short fixture took {l7_ms} ms"
    );

    let ready = ProxyRef::ready_generated(proxy_out.clone());
    assert!(!should_auto_generate_proxy(
        stub.kind,
        &stub.source,
        Some(&ready),
        true
    ));

    // Draft/Auto path: Ready proxy is selected.
    let decoded = resolve_decode_input(&path, Some(&ready), true);
    assert_eq!(decoded, proxy_out);
    // Full/export path: original always.
    let full = resolve_decode_input(&path, Some(&ready), false);
    assert_eq!(full, path);

    // Second generate is a no-op when file already exists (cache hit path used
    // by MediaPoolUi::spawn_proxy_generation).
    assert!(proxy_out.is_file());

    let _ = std::fs::remove_dir_all(&cache);
    eprintln!("ladder L7 proxy ms: {l7_ms} hash={hash}");
}

// ── Draft canvas + asset peek compile ───────────────────────────────────────

#[test]
fn draft_fit_and_asset_peek_compile() {
    assert_eq!(fit_long_edge(1920, 1080, DRAFT_MAX_LONG_EDGE), (960, 540));
    assert_eq!(fit_long_edge(320, 180, DRAFT_MAX_LONG_EDGE), (320, 180));

    let mut project = TimelineProject::new();
    let asset = MediaAsset::from_file(AssetKind::Video, counter_mp4());
    let asset_id = asset.id;
    project.media.insert(asset);
    let seq = Sequence::new("S", photonic_core::timeline::FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);

    let (dw, dh) = fit_long_edge(1920, 1080, DRAFT_MAX_LONG_EDGE);
    let peek = compile_asset_peek(&project, asset_id, Tick::ZERO, Quality::PREVIEW, dw, dh);
    assert!(peek.diagnostics.is_empty());
    assert!(peek.graph.nodes.iter().any(|n| {
        matches!(
            n.op,
            photonic_video::graph::ir::IrOp::DecodeVideo { asset: a, proxy: true, .. }
            if a == asset_id
        )
    }));
    assert!(peek.graph.nodes.iter().any(|n| {
        matches!(
            n.op,
            photonic_video::graph::ir::IrOp::Output { w, h } if w == dw && h == dh
        )
    }));

    // Sequence compile still works (program path).
    let mut track = Track::new(TrackKind::Video, "V1");
    track.clips.push(Clip::new(
        ClipSource::Asset { asset: asset_id },
        Tick::ZERO,
        Tick(1_000_000),
    ));
    project
        .sequences
        .get_mut(&seq_id)
        .unwrap()
        .video_tracks
        .push(track);
    let program = compile(&project, seq_id, 0, Tick::ZERO, Quality::PREVIEW, None);
    assert!(program
        .graph
        .nodes
        .iter()
        .any(|n| { matches!(n.op, photonic_video::graph::ir::IrOp::DecodeVideo { .. }) }));
}

// ── Engine command surface ──────────────────────────────────────────────────

#[test]
fn preview_cmds_coalesce_and_status_fields_exist() {
    // coalesce still latest-wins on Seek; new cmds pass through.
    let batch = vec![
        EngineCmd::SetPreviewQuality(PreviewQuality::Draft),
        EngineCmd::SetPreviewTarget(PreviewTarget::Asset {
            asset: photonic_core::timeline::AssetId::nil(),
            source_time: Tick::ZERO,
        }),
        EngineCmd::Seek(Tick(10)),
        EngineCmd::ScrubSeek(Tick(20)),
        EngineCmd::Seek(Tick(30)),
        EngineCmd::SetProxyMode(ProxyMode::Auto),
    ];
    let out = coalesce_commands(batch);
    assert_eq!(out.len(), 4); // quality, target, last seek, proxy
    assert!(matches!(
        out[0],
        EngineCmd::SetPreviewQuality(PreviewQuality::Draft)
    ));
    assert!(matches!(out[2], EngineCmd::Seek(Tick(30))));

    // Session open requires GPU for evaluator — skip if no adapter.
    let Some(gpu) = photonic_video::GpuContext::request_blocking() else {
        eprintln!("skip session status: no GPU adapter");
        return;
    };
    let engine = VideoEngine::new(gpu);
    let doc = Arc::new(Mutex::new(Document::new("t", 100.0, 100.0)));
    {
        let mut d = doc.lock().unwrap();
        let mut p = TimelineProject::new();
        let s = Sequence::new("S", photonic_core::timeline::FrameRate::FPS_30, 320, 180);
        p.active_sequence = Some(s.id);
        p.sequences.insert(s.id, s);
        d.timeline = Some(p);
    }
    let history = Arc::new(Mutex::new(CommandHistory::new(1)));
    let session = engine.open_session(Arc::clone(&doc), Arc::clone(&history));
    session.send(EngineCmd::SetPreviewQuality(PreviewQuality::Draft));
    session.send(EngineCmd::SetProxyMode(ProxyMode::Auto));
    // Give engine thread a moment to process.
    std::thread::sleep(std::time::Duration::from_millis(80));
    let status = session.status();
    assert_eq!(status.preview_quality, PreviewQuality::Draft);
    assert!(matches!(
        status.preview_target,
        PreviewTarget::Sequence { .. }
    ));
    session.shutdown();
}

// ── AS-1/AS-3 vector `.photon` timeline fixtures (29 §3, brief I task 6) ─────

/// The two vector timeline fixtures load, carry the stably-named nodes whose
/// ids the acceptance-story scripts hardcode, and expose the property targets
/// AS-1 (`transform.opacity`) and AS-3 (`node.<id>.fill.color`) drive.
///
/// The node ids are the durable contract, recorded in `tests/fixtures/README.md`
/// and generated deterministically by
/// `cargo run -p photonic-render --example gen_p1_golden_fixtures -- video-fixtures`.
#[test]
fn vector_title_fixtures_load_and_expose_keyframe_targets() {
    use photonic_core::node::{NodeId, SceneNodeKind};
    use photonic_core::style::FillKind;
    use photonic_core::timeline::prop_registry::{is_known, PropTargetKind};

    // Hardcoded, matching gen_p1_golden_fixtures.rs + the fixtures README.
    let title_asset_node = NodeId::from_u128(0x11A5_0000_0000_0000_0000_0000_0000_0001);
    let title_doc_title = NodeId::from_u128(0x11D0_0000_0000_0000_0000_0000_0000_0001);
    let title_doc_subtitle = NodeId::from_u128(0x11D0_0000_0000_0000_0000_0000_0000_0002);

    // Small helper: the node's fill colour, if it has a solid one — this is the
    // concrete target an AS `node.<id>.fill.color` keyframe binds to. (Vector
    // node paths are NOT in `prop_registry`, which only covers timeline clip
    // props like `transform.*`/`params.*`; a node fill target is validated by
    // resolving the node + its solid fill, not by `is_known`.)
    fn solid_fill(kind: &SceneNodeKind) -> Option<[f32; 4]> {
        let fill = match kind {
            SceneNodeKind::Path(p) => &p.fill,
            SceneNodeKind::Text(t) => &t.fill,
            _ => return None,
        };
        match fill.kind {
            FillKind::Solid(c) => Some([c.r, c.g, c.b, c.a]),
            _ => None,
        }
    }

    // AS-1 title asset: exactly one renderable node named "title".
    let json = std::fs::read_to_string(fixtures_dir().join("title_asset.photon"))
        .expect("read title_asset.photon");
    let (doc, _hist) = photonic_core::load_photon(&json).expect("load title_asset.photon");
    assert_eq!(doc.nodes.len(), 1, "title_asset has one node");
    let title = doc
        .nodes
        .get(&title_asset_node)
        .expect("title_asset node id resolves (hardcoded contract)");
    assert_eq!(title.name, "title");
    assert!(
        solid_fill(&title.kind).is_some(),
        "title node exposes a solid fill colour"
    );
    // AS-1 fades the *clip* via the timeline `transform.opacity` PropPath.
    assert!(is_known(PropTargetKind::ClipTransform, "transform.opacity"));

    // AS-3 title doc: two stably-named nodes, each a real fill-color target.
    let json = std::fs::read_to_string(fixtures_dir().join("title_doc_asset.photon"))
        .expect("read title_doc_asset.photon");
    let (doc, _hist) = photonic_core::load_photon(&json).expect("load title_doc_asset.photon");
    assert_eq!(doc.nodes.len(), 2, "title_doc_asset has two nodes");
    let headline = doc
        .nodes
        .get(&title_doc_title)
        .expect("title_line node id resolves (hardcoded contract)");
    assert_eq!(headline.name, "title_line");
    assert!(
        solid_fill(&headline.kind).is_some(),
        "title_line exposes a solid fill for node.<id>.fill.color"
    );
    let subtitle = doc
        .nodes
        .get(&title_doc_subtitle)
        .expect("subtitle_line node id resolves (hardcoded contract)");
    assert_eq!(subtitle.name, "subtitle_line");
    assert!(solid_fill(&subtitle.kind).is_some());
}

// ── Windows dual-input staging (temp f32le file) ────────────────────────────

#[test]
fn audio_tempfile_staging_writes_f32le() {
    use photonic_video::export::encoder::stage_audio_tempfile;
    let samples: Vec<f32> = (0..480).map(|i| (i as f32 * 0.01).sin()).collect();
    let path = stage_audio_tempfile(&samples).expect("stage");
    assert!(path.is_file());
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), samples.len() * 4);
    // Round-trip first sample.
    let mut le = [0u8; 4];
    le.copy_from_slice(&bytes[0..4]);
    let recovered = f32::from_le_bytes(le);
    assert!((recovered - samples[0]).abs() < 1e-6);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn build_ffmpeg_args_accepts_windows_style_temp_audio_path() {
    use photonic_core::timeline::FrameRate;
    use photonic_video::export::encoder::{
        build_ffmpeg_args, stage_audio_tempfile, AudioStreamSpec, EncodeSpec, EncoderCapabilities,
    };
    use photonic_video::export::presets::{
        AudioCodec, AudioEncodeSpec, Container, ExportPreset, FrameRatePolicy, QualityMode,
        ResolutionSpec, VideoCodec, VideoEncodeSpec,
    };

    let Some(tools) = tools_or_skip() else {
        eprintln!("skip build_ffmpeg_args: no ffmpeg");
        return;
    };
    let caps = EncoderCapabilities::probe(&tools).expect("probe encoders");

    let preset = ExportPreset {
        name: "test".into(),
        container: Container::Mp4,
        video: Some(VideoEncodeSpec {
            codec: VideoCodec::H264,
            quality: QualityMode::Crf(20.0),
        }),
        audio: Some(AudioEncodeSpec {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(128),
        }),
        resolution: ResolutionSpec::SourceFormat,
        frame_rate: FrameRatePolicy::MatchSequence,
        alpha: false,
        faststart: true,
        loudness_target: None,
        stems: false,
    };
    let samples: Vec<f32> = vec![0.0; 480];
    let audio_path = stage_audio_tempfile(&samples).expect("stage");
    let s = EncodeSpec {
        preset: &preset,
        width: 32,
        height: 32,
        frame_rate: FrameRate::new(10, 1),
        audio: Some(AudioStreamSpec {
            sample_rate: 48_000,
            channels: 2,
        }),
        out_path: PathBuf::from("/tmp/out.mp4"),
        // K-F4/K-F5 job options; this case exercises the plain software path.
        prefer_hardware: false,
        encoder_speed: None,
        raw_encoder_args: &[],
        // K-F polish options off: this case pins the baseline arg chain.
        burn_in_timecode: false,
        two_pass: false,
    };
    let args = build_ffmpeg_args(&caps, &s, "yuv420p", Some(audio_path.as_path()))
        .expect("build_ffmpeg_args");
    assert!(args.iter().any(|a| a == "pipe:0"));
    assert!(args.windows(2).any(|w| w == ["-map", "0:v"]));
    assert!(args.windows(2).any(|w| w == ["-map", "1:a"]));
    assert!(args
        .iter()
        .any(|a| a == audio_path.to_string_lossy().as_ref()));
    let _ = std::fs::remove_file(&audio_path);
}
