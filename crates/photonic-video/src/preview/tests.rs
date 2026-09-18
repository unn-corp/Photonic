use super::*;
use crate::graph::ir::ContentHash;
use crate::session::{PreviewQuality, ProxyMode};
use photonic_core::timeline::{FrameRate, SequenceId, Tick};
use std::sync::atomic::AtomicBool;

fn context(sequence: SequenceId) -> PreviewContext {
    PreviewContext {
        sequence,
        format_index: 0,
        width: 128,
        height: 128,
        frame_rate: FrameRate::FPS_30,
        quality: PreviewQuality::Full,
        proxy_mode: ProxyMode::ForceOriginal,
        use_proxy: false,
        source_signature: ContentHash(11),
        profile: PreviewProfile::default(),
    }
}
fn signature(sequence: SequenceId, frame: i64) -> ChunkSignature {
    let context = context(sequence);
    let span =
        ChunkSpan::covering(context.frame_rate, context.frame_rate.frame_start(frame)).unwrap();
    let hashes = (0..span.frame_count)
        .map(|i| ContentHash(i as u128 + frame as u128))
        .collect();
    ChunkSignature::new(context, span, hashes).unwrap()
}
struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("photonic-preview-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn open_cache(root: &Temp, entries: usize, bytes: u64) -> PreviewCache {
    PreviewCache::open(
        root.0.clone(),
        CacheConfig {
            max_bytes: bytes,
            max_entries: entries,
        },
    )
    .unwrap()
}
fn insert(
    cache: &PreviewCache,
    signature: ChunkSignature,
    payload: &[u8],
) -> std::sync::Arc<PreviewChunk> {
    let staged = cache.staging(signature).unwrap();
    std::fs::write(&staged.path, payload).unwrap();
    cache.commit(staged, &AtomicBool::new(false)).unwrap()
}

#[test]
fn fractional_chunks_use_stable_frame_boundaries_and_half_open_ranges() {
    let rate = FrameRate {
        num: 30000,
        den: 1001,
    };
    let first = ChunkSpan::covering(rate, Tick(0)).unwrap();
    assert_eq!(first.frame_count, 30);
    assert_eq!(first.end, rate.frame_start(30));
    assert!(first.contains(rate.frame_start(29)));
    assert!(!first.contains(first.end));
    assert_eq!(
        ChunkSpan::covering(rate, first.end).unwrap().first_frame,
        30
    );
    let chunks: Vec<_> = chunks_for_range(rate, (rate.frame_start(29), rate.frame_start(61)))
        .unwrap()
        .collect();
    assert_eq!(
        chunks.iter().map(|s| s.first_frame).collect::<Vec<_>>(),
        vec![0, 30, 60]
    );
    assert_eq!(
        chunks_for_range(rate, (Tick(0), first.end))
            .unwrap()
            .count(),
        1
    );
    assert!(chunks_for_range(rate, (Tick(-1), first.end)).is_err());
    assert!(ChunkSpan::covering(FrameRate { num: 0, den: 1 }, Tick(0)).is_err());
}
#[test]
fn every_frame_order_and_render_context_participate_in_key() {
    let original = signature(SequenceId::new(), 0);
    for index in 0..original.frame_hashes.len() {
        let mut frames = original.frame_hashes.clone();
        frames[index].0 ^= 1;
        assert_ne!(
            ChunkSignature::new(original.context.clone(), original.span, frames)
                .unwrap()
                .key,
            original.key
        );
    }
    let mut reversed = original.frame_hashes.clone();
    reversed.reverse();
    assert_ne!(
        ChunkSignature::new(original.context.clone(), original.span, reversed)
            .unwrap()
            .key,
        original.key
    );
    let changes: [fn(&mut PreviewContext); 7] = [
        |c| c.format_index += 1,
        |c| c.width += 1,
        |c| c.source_signature.0 += 1,
        |c| c.quality = PreviewQuality::Draft,
        |c| c.proxy_mode = ProxyMode::ForceProxy,
        |c| c.use_proxy = true,
        |c| c.profile.scale = PreviewScale::Half,
    ];
    for change in changes {
        let mut context = original.context.clone();
        change(&mut context);
        assert_ne!(
            ChunkSignature::new(context, original.span, original.frame_hashes.clone())
                .unwrap()
                .key,
            original.key
        );
    }
    assert!(ChunkSignature::new(
        original.context.clone(),
        original.span,
        original.frame_hashes[..29].to_vec()
    )
    .is_err());
    assert_eq!(
        ChunkSignature::new(
            original.context.clone(),
            original.span,
            original.frame_hashes.clone()
        )
        .unwrap(),
        original,
        "undo restores identical content key"
    );
}
#[test]
fn manifest_media_publish_together_and_survive_reopen() {
    let root = Temp::new();
    let sequence = SequenceId::new();
    let sig = signature(sequence, 0);
    let cache = open_cache(&root, 8, 1_000_000);
    let chunk = insert(&cache, sig.clone(), b"complete media");
    let path = chunk.path().to_owned();
    assert_eq!(chunk.local_time(sig.span.end), None);
    assert_eq!(chunk.local_time(sig.span.start), Some(Tick(0)));
    drop(chunk);
    drop(cache);
    let cache = open_cache(&root, 8, 1_000_000);
    assert_eq!(
        std::fs::read(cache.lookup(&sig).unwrap().path()).unwrap(),
        b"complete media"
    );
    assert!(path.parent().unwrap().join("manifest.json").is_file());
}
#[test]
fn cancelled_publication_leaves_no_partial_or_old_destination_damage() {
    let root = Temp::new();
    let cache = open_cache(&root, 8, 1_000_000);
    let sequence = SequenceId::new();
    let old = signature(sequence, 0);
    let old_chunk = insert(&cache, old.clone(), b"old good media");
    let staged = cache.staging(signature(sequence, 30)).unwrap();
    let staging_path = staged.directory.clone();
    std::fs::write(&staged.path, b"partial").unwrap();
    assert!(matches!(
        cache.commit(staged, &AtomicBool::new(true)),
        Err(PreviewError::Cancelled)
    ));
    assert!(!staging_path.exists());
    assert!(cache.lookup(&old).is_some());
    assert_eq!(std::fs::read(old_chunk.path()).unwrap(), b"old good media");
    assert_eq!(cache.status().cache.entries, 1);
}
#[test]
fn lru_prefers_unmarked_chunks_and_preserves_reader_leases() {
    let root = Temp::new();
    let cache = open_cache(&root, 2, 1_000_000);
    let sequence = SequenceId::new();
    let first = signature(sequence, 0);
    let second = signature(sequence, 30);
    let third = signature(sequence, 60);
    drop(insert(&cache, first.clone(), b"first"));
    drop(insert(&cache, second.clone(), b"second"));
    cache.set_zones(sequence, &[(first.span.start, first.span.end)]);
    drop(insert(&cache, third.clone(), b"third"));
    assert!(cache.lookup(&first).is_some());
    assert!(cache.lookup(&second).is_none());
    assert!(cache.lookup(&third).is_some());
    let lease = cache.lookup(&first).unwrap();
    let path = lease.path().to_owned();
    assert_eq!(
        cache.clear(sequence, Some((first.span.start, first.span.end))),
        1
    );
    assert!(cache.lookup(&first).is_none());
    assert!(path.is_file());
    assert_eq!(std::fs::read(lease.path()).unwrap(), b"first");
    drop(lease);
    cache.status();
    assert!(!path.exists());
}
#[test]
fn oversized_chunk_cannot_evict_good_content() {
    let root = Temp::new();
    let cache = open_cache(&root, 8, 10_000);
    let sequence = SequenceId::new();
    let old = signature(sequence, 0);
    drop(insert(&cache, old.clone(), b"small"));
    let staged = cache.staging(signature(sequence, 30)).unwrap();
    std::fs::write(&staged.path, vec![1; 20_000]).unwrap();
    assert!(matches!(
        cache.commit(staged, &AtomicBool::new(false)),
        Err(PreviewError::BudgetExhausted)
    ));
    assert!(cache.lookup(&old).is_some());
    assert_eq!(cache.status().cache.evicted, 0);
}
#[test]
fn changed_media_is_never_served_and_corrupt_reopen_can_rebuild() {
    let root = Temp::new();
    let sig = signature(SequenceId::new(), 0);
    let cache = open_cache(&root, 8, 1_000_000);
    let chunk = insert(&cache, sig.clone(), b"valid");
    std::fs::write(chunk.path(), b"bad truncated replacement").unwrap();
    assert!(cache.lookup(&sig).is_none());
    drop(chunk);
    cache.status();
    drop(cache);
    let cache = open_cache(&root, 8, 1_000_000);
    drop(insert(&cache, sig.clone(), b"rebuilt"));
    assert_eq!(
        std::fs::read(cache.lookup(&sig).unwrap().path()).unwrap(),
        b"rebuilt"
    );
}
#[test]
fn other_sidecar_data_counts_towards_preview_budget() {
    let root = Temp::new();
    std::fs::write(root.0.join("existing.proxy.mp4"), vec![0; 9000]).unwrap();
    let cache = open_cache(&root, 8, 10_000);
    let staged = cache.staging(signature(SequenceId::new(), 0)).unwrap();
    std::fs::write(&staged.path, vec![1; 2000]).unwrap();
    assert!(matches!(
        cache.commit(staged, &AtomicBool::new(false)),
        Err(PreviewError::BudgetExhausted)
    ));
    assert_eq!(cache.status().cache.external_bytes, 9000);
}
#[test]
fn default_profile_is_full_resolution_intra_h264() {
    let profile = PreviewProfile::default();
    assert_eq!(profile.codec, PreviewCodec::IntraH264);
    assert_eq!(profile.scale, PreviewScale::Full);
    assert_eq!(profile.quality, 18);
}

#[test]
fn production_session_serves_cache_with_codec_tolerance_and_undo_reuse() {
    use crate::session::RenderSnapshot;
    use crate::{EngineCmd, GpuContext, VideoEngine};
    use photonic_core::timeline::{
        Clip, ClipSource, PreviewZone, Sequence, TimelineProject, Track, TrackKind,
        TICKS_PER_SECOND,
    };
    use photonic_core::{Color, CommandHistory, Document};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    let Some(gpu) = GpuContext::request_blocking() else {
        assert!(
            std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
            "required preview GPU unavailable"
        );
        eprintln!("no GPU: preview production test skipped");
        return;
    };
    let Some(tools) = crate::media::ffmpeg_locate::locate_for_test() else {
        assert!(
            std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
            "required preview FFmpeg unavailable"
        );
        eprintln!("no FFmpeg: preview production test skipped");
        return;
    };
    let root = Temp::new();
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("cached", FrameRate::FPS_30, 128, 128);
    let id = seq.id;
    seq.preview_zones = vec![PreviewZone {
        start: Tick(0),
        end: Tick(TICKS_PER_SECOND),
    }];
    let mut track = Track::new(TrackKind::Video, "V1");
    track.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 0.25,
                g: 0.75,
                b: 0.5,
                a: 1.0,
            },
        },
        Tick(0),
        Tick(TICKS_PER_SECOND),
    ));
    seq.video_tracks.push(track);
    project.insert_sequence(seq);
    project.active_sequence = Some(id);
    let project = Arc::new(project);
    let mut doc = Document::new("cached", 128.0, 128.0);
    doc.timeline = Some(project.as_ref().clone());
    let doc = Arc::new(doc);
    let snapshot = RenderSnapshot {
        revision: 1,
        project: Some(project.clone()),
        document: Some(doc.clone()),
    };
    let session = VideoEngine::new(gpu.clone()).open_session(
        Arc::new(Mutex::new(doc.as_ref().clone())),
        Arc::new(Mutex::new(CommandHistory::new(64))),
    );
    session.publish_snapshot(snapshot.clone());
    assert!(session.send(EngineCmd::SetPreviewCacheDir {
        path: root.0.clone()
    }));
    assert!(session.send(EngineCmd::SetPreviewQuality(PreviewQuality::Full)));
    assert!(session.send(EngineCmd::SetProxyMode(ProxyMode::ForceOriginal)));
    assert!(session.send(EngineCmd::Seek(Tick(0))));
    let deadline = Instant::now() + Duration::from_secs(20);
    let native = loop {
        if let Some(frame) = session.latest_frame().filter(|f| {
            f.doc_revision == 1
                && f.preview_quality == PreviewQuality::Full
                && f.proxy_mode == ProxyMode::ForceOriginal
        }) {
            break frame;
        }
        assert!(Instant::now() < deadline, "native preview never published");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!native.cached_preview);
    let native_pixels = crate::graph::eval::read_texture_rgba16f(&gpu, &native.texture, 128, 128);
    assert!(session.send(EngineCmd::RenderPreview {
        sequence: id,
        range: None
    }));
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let state = session.preview_status();
        assert!(
            state.error.is_none(),
            "preview planning error: {:?}",
            state.error
        );
        assert!(
            !state.chunks.iter().any(|c| c.state == ChunkState::Failed),
            "preview render failed: {:?}",
            state.chunks
        );
        if state.chunks.iter().any(|c| c.state == ChunkState::Rendered) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "preview never rendered: {:?}",
            state
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let cached_media = std::fs::read_dir(root.0.join("preview").join(id.to_string()).join("0"))
        .unwrap()
        .map(|e| e.unwrap().path().join("media.mp4"))
        .find(|p| p.is_file())
        .unwrap();
    let stamp = std::fs::metadata(&cached_media)
        .unwrap()
        .modified()
        .unwrap();
    assert!(session.send(EngineCmd::SetLoop(Some((Tick(0), Tick(TICKS_PER_SECOND))))));
    assert!(session.send(EngineCmd::Seek(Tick(0))));
    assert!(session.send(EngineCmd::Play));
    let deadline = Instant::now() + Duration::from_secs(15);
    let cached = loop {
        if let Some(frame) = session
            .latest_frame()
            .filter(|f| f.cached_preview && f.doc_revision == 1)
        {
            break frame;
        }
        assert!(
            Instant::now() < deadline,
            "production playback never used a cached frame: {:?}",
            session.preview_status()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let cached_pixels = crate::graph::eval::read_texture_rgba16f(&gpu, &cached.texture, 128, 128);
    let mse = native_pixels
        .iter()
        .zip(&cached_pixels)
        .flat_map(|(a, b)| a[..3].iter().zip(&b[..3]))
        .map(|(a, b)| f64::from(a - b).powi(2))
        .sum::<f64>()
        / (128.0 * 128.0 * 3.0);
    let psnr = -10.0 * mse.max(1e-12).log10();
    assert!(
        psnr >= 36.0,
        "H264 CRF18 preview tolerance is at least36dB RGB PSNR; measured {psnr:.3}"
    );
    // A changed frame never consumes the previous chunk.
    let mut edited = project.as_ref().clone();
    edited.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0].source =
        ClipSource::SolidColor {
            color: Color {
                r: 0.75,
                g: 0.1,
                b: 0.2,
                a: 1.0,
            },
        };
    session.publish_snapshot(RenderSnapshot {
        revision: 2,
        project: Some(Arc::new(edited)),
        document: Some(doc.clone()),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = session.latest_frame().filter(|f| f.doc_revision == 2) {
            assert!(!frame.cached_preview);
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    session.publish_snapshot(RenderSnapshot {
        revision: 3,
        ..snapshot.clone()
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if session
            .latest_frame()
            .is_some_and(|f| f.doc_revision == 3 && f.cached_preview)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "undo did not reuse chunk: {:?}",
            session.preview_status()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::fs::metadata(&cached_media)
            .unwrap()
            .modified()
            .unwrap(),
        stamp,
        "undo must not encode again"
    );
    assert!(session.send(EngineCmd::Pause));
    // The same snapshot exports byte-identical PNGs before/after cache clear.
    use crate::export::presets::{
        Container, ExportPreset, FrameRatePolicy, QualityMode, ResolutionSpec, VideoCodec,
        VideoEncodeSpec,
    };
    let job = crate::session::ExportJob {
        sequence: id,
        format_index: 0,
        output: root.0.join("with-%03d.png"),
        range: Some((Tick(0), FrameRate::FPS_30.frame_start(1))),
        preset: ExportPreset {
            name: "preview-isolation".into(),
            container: Container::ImageSequence,
            video: Some(VideoEncodeSpec {
                codec: VideoCodec::Png,
                quality: QualityMode::Lossless,
            }),
            audio: None,
            resolution: ResolutionSpec::SourceFormat,
            frame_rate: FrameRatePolicy::MatchSequence,
            alpha: true,
            faststart: false,
            loudness_target: None,
            stems: false,
        },
        options: Default::default(),
    };
    crate::export::job::run_export_snapshot(
        gpu.clone(),
        &snapshot,
        &job,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert!(session.send(EngineCmd::ClearPreview {
        sequence: id,
        range: None
    }));
    let without = crate::session::ExportJob {
        output: root.0.join("without-%03d.png"),
        ..job
    };
    crate::export::job::run_export_snapshot(
        gpu,
        &snapshot,
        &without,
        &tools,
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        std::fs::read(root.0.join("with-001.png")).unwrap(),
        std::fs::read(root.0.join("without-001.png")).unwrap()
    );
    session.shutdown();
}
