use super::*;
use photonic_core::timeline::{MediaAsset, Track, TrackKind};

fn test_session() -> EngineSession {
    EngineSession {
        snapshot_input: Arc::new(ArcSwapOption::empty()),
        next_snapshot_generation: AtomicU64::new(1),
        mailbox: Arc::new(CommandMailbox::new()),
        frame: Arc::new(ArcSwapOption::empty()),
        status: Arc::new(ArcSwap::from_pointee(EngineStatus::default())),
        preview_status: Arc::new(ArcSwap::from_pointee(
            crate::preview::PreviewStatusSnapshot::default(),
        )),
        join: None,
    }
}

#[test]
fn t005_mailbox_bounds_flood_and_preserves_semantic_order() {
    let mailbox = CommandMailbox::new();
    let started = Instant::now();
    for n in 0..100_000 {
        assert!(mailbox.send(EngineCmd::ScrubSeek(Tick(n))));
    }
    assert_eq!(mailbox.status().pending, 1);
    assert_eq!(mailbox.status().coalesced, 99_999);
    eprintln!(
        "T005 100000 latest-wins admissions: {:?}, pending=1",
        started.elapsed()
    );
    assert!(mailbox.send(EngineCmd::Play));
    assert!(mailbox.send(EngineCmd::Seek(Tick(100_000))));
    let mut batch = Vec::new();
    mailbox.drain_into(&mut batch);
    assert!(matches!(
        batch.as_slice(),
        [
            EngineCmd::ScrubSeek(Tick(99_999)),
            EngineCmd::Play,
            EngineCmd::Seek(Tick(100_000))
        ]
    ));
    let capacity = batch.capacity();
    batch.clear();
    mailbox.drain_into(&mut batch);
    assert_eq!(batch.capacity(), capacity);
}

#[test]
fn t005_sticky_settings_restore_when_semantic_queue_is_full() {
    let session = test_session();
    for n in 0..COMMAND_QUEUE_CAP {
        assert!(session.send(EngineCmd::Step(n as i32)));
    }
    assert!(!session.send(EngineCmd::Play));
    assert!(session.send(EngineCmd::SetPreviewQuality(PreviewQuality::Full)));
    assert_eq!(session.requested_preview_quality(), PreviewQuality::Full);
    assert!(session.send(EngineCmd::SetPreviewQuality(PreviewQuality::Draft)));
    assert!(session.send(EngineCmd::SetProxyMode(ProxyMode::ForceOriginal)));
    assert!(session.send(EngineCmd::SetProxyMode(ProxyMode::Auto)));
    assert_eq!(session.requested_preview_quality(), PreviewQuality::Draft);
    assert_eq!(session.requested_proxy_mode(), ProxyMode::Auto);
    let mut batch = Vec::new();
    session.mailbox.drain_into(&mut batch);
    assert!(matches!(
        batch[0],
        EngineCmd::SetPreviewQuality(PreviewQuality::Draft)
    ));
    assert!(matches!(batch[1], EngineCmd::SetProxyMode(ProxyMode::Auto)));
    assert_eq!(batch.len(), COMMANDS_PER_TICK + 2);
    let mut steps: Vec<_> = batch
        .into_iter()
        .filter_map(|cmd| {
            if let EngineCmd::Step(n) = cmd {
                Some(n)
            } else {
                None
            }
        })
        .collect();
    while session.mailbox.status().pending != 0 {
        let mut batch = Vec::new();
        session.mailbox.drain_into(&mut batch);
        steps.extend(batch.into_iter().filter_map(|cmd| {
            if let EngineCmd::Step(n) = cmd {
                Some(n)
            } else {
                None
            }
        }));
    }
    assert_eq!(steps, (0..COMMAND_QUEUE_CAP as i32).collect::<Vec<_>>());
    assert_eq!(session.mailbox.status().rejected, 1);
}

#[test]
fn t005_shutdown_is_always_admitted_and_stops_future_work() {
    let mailbox = CommandMailbox::new();
    for _ in 0..COMMAND_QUEUE_CAP {
        assert!(mailbox.send(EngineCmd::Play));
    }
    assert!(mailbox.send(EngineCmd::Shutdown));
    assert!(!mailbox.send(EngineCmd::SetPreviewQuality(PreviewQuality::Full)));
    let mut batch = Vec::new();
    mailbox.drain_into(&mut batch);
    assert!(matches!(batch.as_slice(), [EngineCmd::Shutdown]));
}

#[test]
fn t005_snapshots_reuse_arcs_and_vector_updates_change_source_namespace() {
    let session = test_session();
    let project = Arc::new(TimelineProject::new());
    let document = Arc::new(Document::new("test", 16.0, 16.0));
    let snapshot = RenderSnapshot {
        revision: 7,
        project: Some(project.clone()),
        document: Some(document.clone()),
    };
    assert_eq!(session.publish_snapshot(snapshot.clone()), 1);
    assert_eq!(session.publish_snapshot(snapshot), 2);
    let published = session.snapshot_input.load_full().unwrap();
    assert!(Arc::ptr_eq(
        published.value.project.as_ref().unwrap(),
        &project
    ));
    assert!(Arc::ptr_eq(
        published.value.document.as_ref().unwrap(),
        &document
    ));
    let mut media = MediaSources::new(None);
    media.set_project(project.clone());
    media.set_project(project.clone());
    assert!(Arc::ptr_eq(media.project.as_ref().unwrap(), &project));
    media.set_shared_document(Some(document.clone()), 7);
    let namespace = media.cache_namespace();
    media.set_shared_document(Some(document.clone()), 7);
    assert_eq!(namespace, media.cache_namespace());
    media.set_shared_document(Some(Arc::new((*document).clone())), 7);
    assert_ne!(
        namespace,
        media.cache_namespace(),
        "changed document at same revision must invalidate vector pixels"
    );
}

#[test]
fn t005_worker_permits_survive_request_invalidation() {
    let active = Arc::new(AtomicU64::new(0));
    let a = WorkPermit::acquire(&active, 2).unwrap();
    let b = WorkPermit::acquire(&active, 2).unwrap();
    assert!(WorkPermit::acquire(&active, 2).is_none());
    drop(a);
    let c = WorkPermit::acquire(&active, 2).unwrap();
    assert_eq!(active.load(Ordering::Acquire), 2);
    drop(b);
    drop(c);
    assert_eq!(active.load(Ordering::Acquire), 0);
}

#[test]
fn t005_upload_budget_accounts_bytes_and_keeps_hot_entries() {
    let mut cache = UploadCache::<u32, u32>::new(32).with_byte_budget(12);
    cache.insert_sized(1, 11, 1, 6);
    cache.insert_sized(2, 22, 2, 6);
    assert_eq!(cache.get(&1, 3), Some(&11));
    cache.insert_sized(3, 33, 4, 6);
    assert_eq!(cache.resident_bytes(), 12);
    assert!(!cache.contains_key(&2));
    cache.insert_sized(4, 44, 5, 13);
    assert_eq!(cache.len(), 2);
    assert!(!cache.contains_key(&4));
}

#[test]
fn t005_atomic_inspection_and_missing_source_retry_hold_last_frame() {
    let Some(gpu) = GpuContext::request_blocking() else {
        assert!(
            std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
            "GPU required"
        );
        return;
    };
    let doc = Arc::new(Mutex::new(Document::new("test", 16.0, 16.0)));
    let history = Arc::new(Mutex::new(CommandHistory::default()));
    let (_tx, rx) = crossbeam_channel::bounded(4);
    let frame = Arc::new(ArcSwapOption::empty());
    let mut engine = EngineThread::new(
        gpu,
        doc,
        history,
        rx,
        frame.clone(),
        Arc::new(ArcSwap::from_pointee(EngineStatus::default())),
    );
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("inspection", FrameRate::FPS_30, 16, 16);
    let mut track = Track::new(TrackKind::Video, "video");
    let clip = Clip::new(
        ClipSource::SolidColor {
            color: photonic_core::Color::new(1.0, 0.0, 0.0, 1.0),
        },
        Tick::ZERO,
        Tick(TICKS_PER_SECOND),
    );
    track.clips.push(clip);
    sequence.video_tracks.push(track);
    let seq = sequence.id;
    project.sequences.insert(seq, sequence);
    project.sequence_order.push(seq);
    project.active_sequence = Some(seq);
    engine.snapshot = Some(Arc::new(project));
    engine.media.set_project(engine.snapshot.clone().unwrap());
    engine.handle(EngineCmd::InspectFrame {
        request_id: 77,
        sequence: seq,
        time: Tick::ZERO,
        proxy_mode: ProxyMode::ForceOriginal,
        quality: PreviewQuality::Draft,
        scope_tap: ScopeTapPoint::Program,
    });
    engine.present();
    let first = frame.load_full().unwrap();
    assert_eq!(first.inspection_request_id, Some(77));
    let mut changed = (*engine.snapshot.as_ref().unwrap().as_ref()).clone();
    let asset = MediaAsset::from_file(AssetKind::Image, "/missing/t005-still.png");
    let asset_id = asset.id;
    changed.media.assets.insert(asset_id, asset);
    changed.sequences.get_mut(&seq).unwrap().video_tracks[0].clips[0].source =
        ClipSource::Asset { asset: asset_id };
    engine.snapshot = Some(Arc::new(changed));
    engine.media.set_project(engine.snapshot.clone().unwrap());
    engine.handle(EngineCmd::InspectFrame {
        request_id: 78,
        sequence: seq,
        time: Tick::ZERO,
        proxy_mode: ProxyMode::ForceOriginal,
        quality: PreviewQuality::Draft,
        scope_tap: ScopeTapPoint::Program,
    });
    engine.present();
    assert!(engine.buffering);
    assert!(Arc::ptr_eq(&first, &frame.load_full().unwrap()));
    assert!(
        matches!(engine.controller.tick(), PresentDecision::Present(_)),
        "paused misses must retry"
    );
}

#[test]
#[cfg(unix)]
fn t005_playing_and_scrub_misses_never_wait_per_source() {
    let Some(gpu) = GpuContext::request_blocking() else {
        assert!(
            std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
            "GPU required"
        );
        return;
    };
    let tools = FfmpegTools {
        ffmpeg: PathBuf::from("/bin/true"),
        ffprobe: PathBuf::from("/bin/true"),
    };
    let mut media = MediaSources::new(Some(tools.clone()));
    let mut project = TimelineProject::new();
    let mut ids = Vec::new();
    for n in 0..2 {
        let asset = MediaAsset::from_file(AssetKind::Video, format!("/nonexistent-t005-{n}.mp4"));
        let id = asset.id;
        ids.push(id);
        project.media.assets.insert(id, asset);
        let ring = SharedRing::preview();
        let decode = DecodeSource::new(
            tools.clone(),
            SourceParams {
                input: PathBuf::from("missing"),
                width: 2,
                height: 2,
                pix_fmt: PixFmt::Yuv420p,
                pts_kind: PtsKind::Cfr(FrameRate::FPS_30),
                keyframes: KeyframeIndex::default(),
            },
            ring.clone(),
        );
        let worker = DecodeWorker::spawn(Arc::new(Mutex::new(decode)), FrameRate::FPS_30);
        media.sources.insert(
            (id, false),
            Some(VideoSourceEntry {
                ring,
                worker,
                colorimetry: Colorimetry {
                    matrix: Matrix::Bt709,
                    range: Range::Limited,
                },
                rate: FrameRate::FPS_30,
                last_used: 0,
            }),
        );
    }
    media.set_project(Arc::new(project));
    media.begin_frame();
    let started = Instant::now();
    for (playing, scrubbing) in [(true, false), (false, true)] {
        media.set_playing(playing);
        media.set_scrubbing(scrubbing);
        for id in &ids {
            assert!(media.video_texture(&gpu, *id, Tick::ZERO, false).is_none());
        }
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(80),
        "four readiness misses waited per source: {elapsed:?}"
    );
    assert_eq!(media.readiness_status().requested, 4);
    assert_eq!(media.readiness_status().ready, 0);
    eprintln!("T005 four realtime source misses: {elapsed:?}");
}
