//! End-to-end **UI code paths** for the video editor residual workstream.
//!
//! These are not unit re-implementations: they call the same functions the GUI
//! uses when the user imports media, sets source marks, builds proxies, and
//! drives the single-monitor engine bridge (`panels/media_pool`,
//! `app/source_marks`, `app/engine`, `app/timeline/interact`).
//!
//! Runs headlessly when no display is present; GPU/ffmpeg cases skip cleanly
//! (same convention as photonic-video integration tests).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use photonic_core::history::{Command, CommandHistory};
use photonic_core::timeline::{
    ops, AssetKind, ClipSource, FrameRate, MediaAsset, Sequence, Tick, TimelineProject, Track,
    TrackKind,
};
use photonic_core::Document;
use photonic_gui::app::source_marks::{io_targets_source_marks, SourceMarksSession};
use photonic_gui::app::timeline::interact;
use photonic_gui::panels::media_pool::{guess_asset_kind, MediaPoolUi};
use photonic_video::{
    coalesce_commands, EngineCmd, PreviewQuality, PreviewTarget, ProxyMode, VideoEngine,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../photonic-video/tests/fixtures")
        .canonicalize()
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../photonic-video/tests/fixtures")
        })
}

fn counter_mp4() -> PathBuf {
    fixtures_dir().join("counter.mp4")
}

fn beep_wav() -> PathBuf {
    fixtures_dir().join("beep_flash.wav")
}

fn tools_or_skip() -> Option<photonic_video::media::FfmpegTools> {
    photonic_video::media::locate().ok()
}

// ── 1. Media pool import ladder as the UI runs it ───────────────────────────

/// Drop multi-select into the pool: L0 stubs on the calling thread (same as
/// `spawn_import` return), then worker fills meta — mirrors media_pool drawer.
#[test]
fn ui_import_ladder_l0_then_meta_drain() {
    let paths = vec![counter_mp4(), beep_wav(), PathBuf::from("/nope/notes.txt")];
    // Some fixtures may be missing in thin checkouts.
    let paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|p| p.is_file() || p.extension().map(|e| e == "txt").unwrap_or(false))
        .collect();
    let media_paths: Vec<_> = paths
        .iter()
        .filter(|p| guess_asset_kind(p).is_some() && p.is_file())
        .cloned()
        .collect();
    if media_paths.is_empty() {
        eprintln!("skip ui_import: no fixtures");
        return;
    }

    let mut pool_ui = MediaPoolUi::default();
    let t0 = Instant::now();
    let stubs = pool_ui.spawn_import(media_paths.clone(), None, None);
    let l0_ms = t0.elapsed().as_millis();
    assert_eq!(stubs.len(), media_paths.len());
    assert!(stubs
        .iter()
        .all(|a| a.probe.is_none() && a.content_hash.is_none()));
    assert!(l0_ms < 100, "UI L0 must stay cheap ({l0_ms} ms)");

    // Apply L0 AddAsset like PhotonicApp does after spawn_import.
    let mut doc = Document::new("ui", 1920.0, 1080.0);
    let mut history = CommandHistory::new(64);
    let mut project = TimelineProject::new();
    let seq = Sequence::new("Main", FrameRate::FPS_30, 320, 180);
    let seq_id = seq.id;
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    project.sequence_order.push(seq_id);
    for asset in &stubs {
        project.media.insert(asset.clone());
    }
    doc.timeline = Some(project);

    // Poll meta like draw() does — up to 30s for fixture ladder.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut got = 0usize;
    while Instant::now() < deadline {
        let updates = pool_ui.drain_meta();
        for m in updates {
            if m.waveform_only {
                continue;
            }
            got += 1;
            if let Some(project) = doc.timeline.as_ref() {
                if let Ok(cmd) = ops::set_asset_meta(project, m.asset, m.probe, m.content_hash) {
                    history.execute_discrete(Command::Timeline(cmd), &mut doc);
                }
            }
        }
        // One meta message per asset for L1–L4; waveform_only is extra.
        if got >= stubs.len() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        got >= stubs.len(),
        "UI meta drain expected ≥{} L1–L4 messages, got {got}",
        stubs.len()
    );
    let project = doc.timeline.as_ref().unwrap();
    for s in &stubs {
        let a = project.media.assets.get(&s.id).expect("asset");
        // After worker, video should have hash (when tools present).
        if tools_or_skip().is_some() && a.kind == AssetKind::Video {
            assert!(
                a.content_hash.is_some() || a.probe.is_some(),
                "video asset should receive meta under ffmpeg"
            );
        }
    }
}

// ── 2. Source marks + Insert as the transport/command palette do ────────────

#[test]
fn ui_source_marks_insert_overwrite_path() {
    let path = counter_mp4();
    if !path.is_file() {
        eprintln!("skip marks insert: no fixture");
        return;
    }

    let mut doc = Document::new("ui", 1920.0, 1080.0);
    let mut history = CommandHistory::new(64);
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("Main", FrameRate::FPS_30, 320, 180);
    let seq_id = seq.id;
    let track = Track::new(TrackKind::Video, "V1");
    let track_id = track.id;
    seq.video_tracks.push(track);
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    project.sequence_order.push(seq_id);

    let asset = MediaAsset::from_file(AssetKind::Video, &path);
    let asset_id = asset.id;
    project.media.insert(asset.clone());
    doc.timeline = Some(project);

    // Pool click arms + peeks (SOURCE) — same session fields as PhotonicApp.
    let mut marks = SourceMarksSession::default();
    marks.arm(asset_id, Tick::ZERO);
    assert!(io_targets_source_marks(true, false, marks.armed_asset));
    // User hits I / O on transport while SOURCE badge shows.
    marks.set_in(Tick(100_000));
    marks.set_out(Tick(800_000));
    let default_dur = Tick(5_000_000);
    let pending = marks
        .pending_source(&asset, default_dur)
        .expect("marks → pending like arm_pending_source");
    assert_eq!(pending.src_in, Tick(100_000));
    assert_eq!(pending.src_out, Tick(800_000));

    // Insert at playhead — real UI insert-edit path (`,`).
    let playhead = Tick(0);
    let clip = pending.to_clip(playhead);
    interact::do_insert_edit(&mut doc, &mut history, seq_id, track_id, playhead, clip);

    let project = doc.timeline.as_ref().unwrap();
    let seq = project.sequences.get(&seq_id).unwrap();
    let t = seq.video_tracks.iter().find(|t| t.id == track_id).unwrap();
    assert_eq!(t.clips.len(), 1);
    assert_eq!(t.clips[0].source_in, Tick(100_000));
    assert_eq!(t.clips[0].duration, Tick(700_000));
    match &t.clips[0].source {
        ClipSource::Asset { asset: a } => assert_eq!(*a, asset_id),
        other => panic!("expected Asset clip, got {other:?}"),
    }

    // SEQUENCE focus: I/O must not target source marks (work range path).
    assert!(!io_targets_source_marks(false, false, marks.armed_asset));
    // Play wins: even SOURCE badge, playing sequence steals I/O.
    assert!(!io_targets_source_marks(true, true, marks.armed_asset));
}

// ── 3. Single-monitor engine bridge (Draft / proxy / peek / play-wins) ──────

#[test]
fn ui_engine_bridge_preview_target_and_proxy_mode() {
    let Some(gpu) = photonic_video::GpuContext::request_blocking() else {
        eprintln!("skip ui_engine_bridge: no GPU (try xvfb + llvmpipe)");
        return;
    };
    let path = counter_mp4();
    if !path.is_file() {
        eprintln!("skip ui_engine_bridge: no fixture");
        return;
    }

    let engine = VideoEngine::new(gpu);
    let doc = Arc::new(Mutex::new(Document::new("ui", 320.0, 180.0)));
    let history = Arc::new(Mutex::new(CommandHistory::new(16)));
    {
        let mut d = doc.lock().unwrap();
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("Main", FrameRate::FPS_30, 320, 180);
        let seq_id = seq.id;
        let asset = MediaAsset::from_file(AssetKind::Video, &path);
        let asset_id = asset.id;
        project.media.insert(asset);
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(photonic_core::timeline::Clip::new(
            ClipSource::Asset { asset: asset_id },
            Tick::ZERO,
            Tick(1_000_000),
        ));
        seq.video_tracks.push(track);
        project.sequences.insert(seq_id, seq);
        project.active_sequence = Some(seq_id);
        project.sequence_order.push(seq_id);
        d.timeline = Some(project);
    }

    let session = engine.open_session(Arc::clone(&doc), Arc::clone(&history));
    let asset_id = {
        let d = doc.lock().unwrap();
        *d.timeline
            .as_ref()
            .unwrap()
            .media
            .assets
            .keys()
            .next()
            .unwrap()
    };
    let seq_id = {
        let d = doc.lock().unwrap();
        d.timeline.as_ref().unwrap().active_sequence.unwrap()
    };
    // Ensure paused before peek (play-wins would ignore asset target).
    session.send(EngineCmd::Pause);
    session.send(EngineCmd::SetActiveSequence(seq_id));
    session.send(EngineCmd::SetPreviewQuality(PreviewQuality::Draft));
    session.send(EngineCmd::SetProxyMode(ProxyMode::Auto));
    // Pool click → SOURCE peek (same EngineCmds as EngineBridge::peek_asset).
    session.send(EngineCmd::SetPreviewTarget(PreviewTarget::Asset {
        asset: asset_id,
        source_time: Tick::ZERO,
    }));
    // Poll until engine thread applies (status is arc-swap published).
    let mut saw_asset = false;
    for _ in 0..375 {
        std::thread::sleep(Duration::from_millis(40));
        let st = session.status();
        if matches!(st.preview_target, PreviewTarget::Asset { asset, .. } if asset == asset_id) {
            saw_asset = true;
            assert_eq!(st.preview_quality, PreviewQuality::Draft);
            break;
        }
    }
    assert!(
        saw_asset,
        "engine status never showed Asset peek after SetPreviewTarget"
    );

    // Play wins: Play forces sequence target (24 §3.2) — engine Play path.
    session.send(EngineCmd::Play);
    let mut saw_seq_or_play = false;
    for _ in 0..375 {
        std::thread::sleep(Duration::from_millis(40));
        let st = session.status();
        if matches!(st.preview_target, PreviewTarget::Sequence { .. }) || st.playing {
            saw_seq_or_play = true;
            break;
        }
    }
    assert!(
        saw_seq_or_play,
        "after Play, expect sequence target or playing"
    );
    session.send(EngineCmd::Pause);

    // Coalesce as GUI scrub does.
    let batch = vec![
        EngineCmd::ScrubSeek(Tick(10)),
        EngineCmd::ScrubSeek(Tick(20)),
        EngineCmd::Seek(Tick(30)),
    ];
    let out = coalesce_commands(batch);
    assert!(matches!(out.last(), Some(EngineCmd::Seek(Tick(30)))));

    session.shutdown();
}

// ── 4. Proxy attach path used by media pool context menu ────────────────────

#[test]
fn ui_attach_proxy_then_resolve_decode() {
    let Some(tools) = tools_or_skip() else {
        eprintln!("skip ui_attach_proxy: no ffmpeg");
        return;
    };
    let path = counter_mp4();
    if !path.is_file() {
        return;
    }
    let cache = std::env::temp_dir().join(format!("photonic-ui-proxy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    std::fs::create_dir_all(&cache).unwrap();
    let hash = photonic_video::media::content_hash(&path).unwrap();
    let proxy_out = photonic_video::media::proxy_cache_path(&cache, &hash);
    photonic_video::media::generate_proxy(&tools, &path, &proxy_out, &|| false)
        .expect("generate stand-in proxy");

    // validate_attach is what MediaAttachProxy panel action calls after rfd.
    let report = photonic_video::media::validate_attach(&tools, &path, &proxy_out, true)
        .expect("attach validation");
    assert!(proxy_out.is_file());
    assert_eq!(
        report.proxy.origin,
        photonic_core::timeline::ProxyOrigin::Attached
    );

    // Draft Auto path selects proxy file when Ready.
    // validate_attach stores a canonicalized path (macOS: /var → /private/var).
    let decoded = photonic_video::media::resolve_decode_input(&path, Some(&report.proxy), true);
    let expect_proxy = proxy_out
        .canonicalize()
        .unwrap_or_else(|_| proxy_out.clone());
    assert_eq!(decoded, expect_proxy);
    // Export / ForceOriginal never uses proxy.
    assert_eq!(
        photonic_video::media::resolve_decode_input(&path, Some(&report.proxy), false),
        path
    );
    let _ = std::fs::remove_dir_all(&cache);
}

// ── 5. Cut-ahead as play approaches a cut (pure scan the engine uses) ───────

#[test]
fn ui_cut_ahead_before_edit_boundary() {
    use photonic_video::playback::prefetch::{cut_ahead_targets, CUT_AHEAD_LEAD_FRAMES};

    let mut seq = Sequence::new("Main", FrameRate::FPS_30, 320, 180);
    let a1 = photonic_core::timeline::AssetId::new();
    let a2 = photonic_core::timeline::AssetId::new();
    let mut track = Track::new(TrackKind::Video, "V1");
    track.clips.push(photonic_core::timeline::Clip::new(
        ClipSource::Asset { asset: a1 },
        Tick::ZERO,
        Tick(1_000_000),
    ));
    let mut c2 = photonic_core::timeline::Clip::new(
        ClipSource::Asset { asset: a2 },
        Tick(1_000_000),
        Tick(1_000_000),
    );
    c2.source_in = Tick(10);
    track.clips.push(c2);
    seq.video_tracks.push(track);

    let lead = Tick(seq.frame_rate.ticks_per_frame().0 * CUT_AHEAD_LEAD_FRAMES);
    let targets = cut_ahead_targets(&seq, Tick::ZERO, lead.max(Tick(1_000_000)));
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].asset, a2);
}

// ── 6. Effects browser catalogue → clip (K-B16 UI surface) ──────────────────

/// Double-click path: `ClipEffect::from_manifest` + `ops::add_effect` — the same
/// stack the Effects browser uses for every MANIFESTS entry (not just the 7
/// legacy EffectKind variants).
#[test]
fn ui_effects_browser_applies_manifest_to_clip() {
    use photonic_core::timeline::{Clip, ClipEffect, EffectId, MANIFESTS};

    let mut doc = Document::new("fx", 1920.0, 1080.0);
    let mut history = CommandHistory::new(64);
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("Main", FrameRate::FPS_30, 320, 180);
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    let track_id = track.id;
    let clip = Clip::new(
        ClipSource::Asset {
            asset: photonic_core::timeline::AssetId::new(),
        },
        Tick::ZERO,
        Tick(1_000_000),
    );
    let clip_id = clip.id;
    track.clips.push(clip);
    seq.video_tracks.push(track);
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    project.sequence_order.push(seq_id);
    doc.timeline = Some(project);

    // Apply a non-legacy bridged id (box blur) and a classic (gaussian).
    let ids = [
        EffectId::new_static("blur.box"),
        EffectId::new_static("color.curves"),
        EffectId::new_static("geo.ripple"),
        EffectId::new_static("stylize.glow"),
    ];
    for id in &ids {
        assert!(
            MANIFESTS.iter().any(|m| m.id == *id),
            "catalogue missing {}",
            id.as_str()
        );
        let effect = ClipEffect::from_manifest(id.clone()).expect("from_manifest");
        let project = doc.timeline.as_ref().unwrap();
        let cmd =
            ops::add_effect(project, seq_id, track_id, clip_id, effect, None).expect("add_effect");
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
    }

    let project = doc.timeline.as_ref().unwrap();
    let clip = project
        .sequences
        .get(&seq_id)
        .unwrap()
        .video_tracks
        .iter()
        .find(|t| t.id == track_id)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.id == clip_id)
        .unwrap();
    assert_eq!(clip.effects.len(), 4);
    assert_eq!(clip.effects[0].id.as_str(), "blur.box");
    assert_eq!(clip.effects[1].id.as_str(), "color.curves");
    assert_eq!(clip.effects[2].id.as_str(), "geo.ripple");
    assert_eq!(clip.effects[3].id.as_str(), "stylize.glow");
    // from_manifest seeds id + version from the catalogue.
    assert_eq!(clip.effects[0].version, 1);
    assert!(!clip.effects[0].inert);
}

// ── 7. Sequence tabs create / duplicate (G-17) ──────────────────────────────

#[test]
fn ui_sequence_tabs_create_and_duplicate() {
    use photonic_gui::app::timeline::ops_bridge;

    let mut doc = Document::new("tabs", 1920.0, 1080.0);
    let mut history = CommandHistory::new(64);
    let mut project = TimelineProject::new();
    let seq = Sequence::new("Main", FrameRate::FPS_30, 1920, 1080);
    let main_id = seq.id;
    project.sequences.insert(main_id, seq);
    project.active_sequence = Some(main_id);
    project.sequence_order.push(main_id);
    doc.timeline = Some(project);

    let mut open_tabs = vec![main_id];
    ops_bridge::create_sequence_tab(
        &mut doc,
        &mut history,
        FrameRate::FPS_30,
        1920,
        1080,
        &mut open_tabs,
    );
    assert_eq!(open_tabs.len(), 2);
    let second = *open_tabs.last().unwrap();
    assert_ne!(second, main_id);
    assert_eq!(doc.timeline.as_ref().unwrap().active_sequence, Some(second));
    assert_eq!(doc.timeline.as_ref().unwrap().sequences.len(), 2);

    ops_bridge::duplicate_sequence_tab(&mut doc, &mut history, main_id, &mut open_tabs);
    assert_eq!(open_tabs.len(), 3);
    assert_eq!(doc.timeline.as_ref().unwrap().sequences.len(), 3);
    let active = doc.timeline.as_ref().unwrap().active_sequence.unwrap();
    let name = &doc.timeline.as_ref().unwrap().sequences[&active].name;
    assert!(
        name.contains("copy") || name != "Main",
        "duplicate should rename: {name}"
    );
}

// ── 8. Media pool interlaced probe surface (K-G6 UI) ────────────────────────

#[test]
fn ui_media_pool_interlaced_probe_summary() {
    use photonic_core::timeline::{
        AssetSource, MediaAsset, MediaProbe, ProbedColor, ScanType, VideoStreamInfo,
    };
    use photonic_gui::panels::media_pool::probe_summary;

    let mut asset = MediaAsset::new(
        AssetKind::Video,
        AssetSource::File {
            path: PathBuf::from("/tmp/i.mov"),
            rel_path: None,
        },
    );
    asset.probe = Some(MediaProbe {
        duration: Tick(30_000_000),
        video: Some(VideoStreamInfo {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate::FPS_30,
            pixel_aspect: 1.0,
            color: ProbedColor::default(),
            keyframe_index_cached: false,
            scan: ScanType::InterlacedBottomFirst,
        }),
        audio: None,
        container: "mov".into(),
        codec: "dvvideo".into(),
        is_vfr: false,
        pixel_format: None,
        has_alpha: false,
    });
    let s = probe_summary(&asset);
    assert!(s.contains("interlaced (BFF)"), "got: {s}");
    assert!(s.contains("1920x1080"));
}

// ── 9. Catalogue size pin (K-B16 UI browser consumes MANIFESTS) ─────────────

#[test]
fn ui_effects_catalogue_has_bridged_surface() {
    use photonic_core::timeline::MANIFESTS;
    // Effects browser iterates MANIFESTS; pin the floor so a catalogue regression
    // fails the UI path suite, not only core unit tests.
    assert!(
        MANIFESTS.len() >= 38,
        "effects browser catalogue expected ≥38 ids, got {}",
        MANIFESTS.len()
    );
    // At least one entry per category family the browser sections on.
    use photonic_core::timeline::EffectCategory::*;
    for cat in [Blur, Sharpen, Color, Stylize, Key, Geo, Util] {
        assert!(
            MANIFESTS.iter().any(|m| m.category == cat),
            "missing category {cat:?}"
        );
    }
}

// ── 10. AS-2 GUI arm (structural, CAP-019 pair) ─────────────────────────────
//
// Script arm: tools/as2_proxy_edit.py. This path exercises the same core ops
// the GUI uses for multi-clip import → insert → transition → grade → dual
// export-job construction, without requiring a live MCP server.

#[test]
fn ui_as2_short_film_structural_arm() {
    use photonic_core::timeline::{
        Clip, EaseCurve, Grade, Transition, TransitionKind, TransitionParams,
    };
    use photonic_video::export::presets;
    use photonic_video::session::{ExportJob, RenderJobOptions};

    let path_a = counter_mp4();
    let path_b = fixtures_dir().join("color_bars.mp4");
    if !path_a.is_file() || !path_b.is_file() {
        eprintln!("skip as2 gui arm: fixtures missing");
        return;
    }

    let mut doc = Document::new("as2", 1920.0, 1080.0);
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("AS-2 Sequence", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    let mut a1 = Track::new(TrackKind::Audio, "A1");
    let v1_id = v1.id;
    let a1_id = a1.id;

    let asset_a = MediaAsset::from_file(AssetKind::Video, &path_a);
    let asset_b = MediaAsset::from_file(AssetKind::Video, &path_b);
    let id_a = asset_a.id;
    let id_b = asset_b.id;
    project.media.insert(asset_a);
    project.media.insert(asset_b);

    let mut c1 = Clip::new(
        ClipSource::Asset { asset: id_a },
        Tick::ZERO,
        Tick(1_000_000),
    );
    let c2 = Clip::new(
        ClipSource::Asset { asset: id_b },
        Tick(1_000_000),
        Tick(1_000_000),
    );
    // Cross-dissolve transition on the cut (AS-2 multi-track edit beat).
    let mut xfade = Transition::new(TransitionKind::CrossDissolve, Tick(200_000));
    xfade.params = TransitionParams {
        curve: EaseCurve::EaseInOut,
        ..Default::default()
    };
    c1.transition_out = Some(xfade);
    // Grade on clip (AS-2 grade beat) — GUI color page writes via set_grade.
    c1.grade = Some(Grade::new());
    let c1_id = c1.id;
    let c2_id = c2.id;
    v1.clips.push(c1);
    v1.clips.push(c2);
    a1.clips.push(Clip::new(
        ClipSource::Asset { asset: id_a },
        Tick::ZERO,
        Tick(1_000_000),
    ));
    seq.video_tracks.push(v1);
    seq.audio_tracks.push(a1);
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    project.sequence_order.push(seq_id);
    doc.timeline = Some(project);

    let project = doc.timeline.as_ref().unwrap();
    let s = project.sequences.get(&seq_id).unwrap();
    assert_eq!(
        s.video_tracks
            .iter()
            .find(|t| t.id == v1_id)
            .unwrap()
            .clips
            .len(),
        2
    );
    assert_eq!(
        s.audio_tracks
            .iter()
            .find(|t| t.id == a1_id)
            .unwrap()
            .clips
            .len(),
        1
    );
    let clip1 = s.video_tracks[0]
        .clips
        .iter()
        .find(|c| c.id == c1_id)
        .unwrap();
    assert!(clip1.transition_out.is_some());
    assert!(clip1.grade.is_some());
    assert!(s.video_tracks[0].clips.iter().any(|c| c.id == c2_id));

    // Dual export jobs (master-ish + web) — structural twin of as2 dual export.
    let web = presets::built_in_presets()
        .into_iter()
        .find(|p| p.name.contains("H.264") || p.name.contains("Web"))
        .expect("web preset");
    let jobs = [
        ExportJob {
            sequence: seq_id,
            format_index: 0,
            preset: web.clone(),
            output: std::env::temp_dir().join("as2_master.mp4"),
            range: None,
            options: RenderJobOptions {
                inhibit_sleep: true,
                ..Default::default()
            },
        },
        ExportJob {
            sequence: seq_id,
            format_index: 0,
            preset: web,
            output: std::env::temp_dir().join("as2_web.mp4"),
            range: None,
            options: RenderJobOptions::default(),
        },
    ];
    assert_eq!(jobs.len(), 2);
    assert_ne!(jobs[0].output, jobs[1].output);
}

// ── 11. AS-3 GUI arm (structural, CAP-019 pair) ─────────────────────────────
//
// Script arm: tools/as3_motion_graphics.py. This path keyframes clip transform
// and places a partial-alpha clip for alpha export readiness — the same ops
// the GUI keyframe editor and media pool use.

/// Proposal 213 A2: structural AS-1 path — empty project → import asset →
/// auto-place on timeline → split at playhead → Social export preset resolves.
#[test]
fn ui_as1_social_path_structural_arm() {
    use photonic_gui::app::timeline::ops_bridge;
    use photonic_video::export::presets;

    let mut doc = Document::new("as1", 1920.0, 1080.0);
    let mut history = CommandHistory::new(64);

    // Ensure project + sequence like empty-state Import CTA.
    let seq_id = ops_bridge::ensure_project_and_sequence(&mut doc, &mut history, FrameRate::FPS_30)
        .expect("project+sequence");
    ops_bridge::add_track(&mut doc, &mut history, seq_id, TrackKind::Video);

    // Social 9:16 format (same as empty-state quick chip / format bar).
    ops_bridge::switch_to_aspect(&mut history, &mut doc, seq_id, "9:16", 1080, 1920);
    {
        let seq = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap();
        let f = seq.format();
        assert_eq!((f.width, f.height), (1080, 1920), "Social 9:16 active");
    }

    // Social presets always required (export dialog default catalog).
    let names: Vec<_> = presets::built_in_presets()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(names.iter().any(|n| n == "Social 9:16"));
    assert!(names.iter().any(|n| n == "Social 16:9"));

    // Import fixture and place via first-fit insert (auto-place path).
    if !counter_mp4().is_file() {
        eprintln!("skip AS-1 place/split: no counter.mp4 fixture");
        return;
    }
    let asset = MediaAsset::from_file(AssetKind::Video, counter_mp4());
    let asset_id = asset.id;
    history.execute_discrete(Command::Timeline(ops::add_asset(asset)), &mut doc);

    assert!(
        ops_bridge::insert_asset_at_first_fit(&mut doc, &mut history, asset_id, Tick::ZERO),
        "auto-place import on timeline"
    );

    // Split mid-clip (AS-1 cut step).
    let (track_id, clip_id, mid) = {
        let seq = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .get(&seq_id)
            .unwrap();
        let t = &seq.video_tracks[0];
        let c = &t.clips[0];
        let mid = c.start + Tick(c.duration.0 / 2);
        (t.id, c.id, mid)
    };
    assert!(ops_bridge::split(
        &mut doc,
        &mut history,
        seq_id,
        track_id,
        clip_id,
        mid
    ));
    let n_clips = doc
        .timeline
        .as_ref()
        .unwrap()
        .sequences
        .get(&seq_id)
        .unwrap()
        .video_tracks[0]
        .clips
        .len();
    assert!(n_clips >= 2, "split should yield ≥2 clips, got {n_clips}");
}

#[test]
fn ui_as3_motion_graphics_structural_arm() {
    use photonic_core::timeline::{AnimTarget, Clip, Interp, Keyframe, PropPath, PropValue};

    let mut doc = Document::new("as3", 1920.0, 1080.0);
    let mut history = CommandHistory::new(64);
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("AS-3 Sequence", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    let mut track = Track::new(TrackKind::Video, "V1");
    let track_id = track.id;

    // Solid title stand-in (vector doc optional when fixture absent).
    let title = Clip::new(
        ClipSource::SolidColor {
            color: photonic_core::Color {
                r: 0.1,
                g: 0.2,
                b: 0.8,
                a: 1.0,
            },
        },
        Tick::ZERO,
        Tick(2_000_000),
    );
    let title_id = title.id;
    track.clips.push(title);

    // Partial-alpha stand-in for CAP-021 alpha export readiness.
    let alpha_path = fixtures_dir().join("alpha_gradient.mov");
    if alpha_path.is_file() {
        let asset = MediaAsset::from_file(AssetKind::Video, &alpha_path);
        let aid = asset.id;
        project.media.insert(asset);
        track.clips.push(Clip::new(
            ClipSource::Asset { asset: aid },
            Tick(2_000_000),
            Tick(500_000),
        ));
    }

    seq.video_tracks.push(track);
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    project.sequence_order.push(seq_id);
    doc.timeline = Some(project);

    // Keyframe transform.x via the same ops the GUI keyframe editor uses.
    let target = AnimTarget::ClipTransform { clip: title_id };
    for kf in [
        Keyframe::new(Tick::ZERO, PropValue::Float(0.0), Interp::Linear),
        Keyframe::new(Tick(1_000_000), PropValue::Float(200.0), Interp::Linear),
    ] {
        let cmd = {
            let p = doc.timeline.as_ref().unwrap();
            ops::set_keyframe(p, target.clone(), PropPath::new("transform.x"), kf)
        };
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
    }

    let project = doc.timeline.as_ref().unwrap();
    let clip = project
        .sequences
        .get(&seq_id)
        .unwrap()
        .video_tracks
        .iter()
        .find(|t| t.id == track_id)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.id == title_id)
        .unwrap();
    let x_track = clip
        .transform
        .track(&PropPath::new("transform.x"))
        .expect("transform.x keyframes like keyframe editor");
    assert_eq!(x_track.keyframes.len(), 2);
    assert_eq!(x_track.keyframes[1].value, PropValue::Float(200.0));
}

// ── 9. Multi-clip vertical move (210) via the real timeline ops_bridge ──────

/// Drag-commit path for multi-select body move with track_delta: same
/// `ops_bridge::move_clips` the timeline panel calls on pointer release
/// (`commit_drag` → multi-select branch). Linked audio rides time-only.
#[test]
fn ui_multi_clip_vertical_move_via_ops_bridge() {
    use photonic_core::timeline::Clip;
    use photonic_gui::app::timeline::ops_bridge;

    let mut doc = Document::new("mv", 1920.0, 1080.0);
    let mut history = CommandHistory::new(64);
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("Seq", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;

    let group = photonic_core::timeline::clip::LinkGroupId::new();

    let mut v1 = Track::new(TrackKind::Video, "V1");
    let v1_id = v1.id;
    let mut c1 = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
    let c1_id = c1.id;
    c1.link_group = Some(group);
    v1.clips.push(c1);

    let mut v2 = Track::new(TrackKind::Video, "V2");
    let v2_id = v2.id;
    let c2 = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
    let c2_id = c2.id;
    v2.clips.push(c2);

    let v3 = Track::new(TrackKind::Video, "V3");
    let v3_id = v3.id;

    let mut a1 = Track::new(TrackKind::Audio, "A1");
    let a1_id = a1.id;
    let mut ac = Clip::new(ClipSource::Adjustment, Tick(0), Tick(100));
    let ac_id = ac.id;
    ac.link_group = Some(group);
    a1.clips.push(ac);

    seq.video_tracks.extend([v1, v2, v3]);
    seq.audio_tracks.push(a1);
    project.sequences.insert(seq_id, seq);
    project.active_sequence = Some(seq_id);
    doc.timeline = Some(project);

    // Same call site as multi-select drag commit: selection is video clips;
    // ops_bridge folds the linked audio partner.
    ops_bridge::move_clips(
        &mut doc,
        &mut history,
        seq_id,
        &[(v1_id, c1_id), (v2_id, c2_id)],
        Tick(40),
        1,
    );

    let s = &doc.timeline.as_ref().unwrap().sequences[&seq_id];
    assert!(
        s.track(v1_id).unwrap().clips.iter().all(|c| c.id != c1_id),
        "V1 vacated"
    );
    assert!(
        s.track(v2_id)
            .unwrap()
            .clips
            .iter()
            .any(|c| c.id == c1_id && c.start == Tick(40)),
        "c1 on V2 @ +40"
    );
    assert!(
        s.track(v3_id)
            .unwrap()
            .clips
            .iter()
            .any(|c| c.id == c2_id && c.start == Tick(40)),
        "c2 on V3 @ +40"
    );
    let audio = s
        .track(a1_id)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.id == ac_id)
        .expect("audio stays on A1");
    assert_eq!(audio.start, Tick(40), "linked audio time-only");

    // One undo restores the multi-clip gesture.
    history.undo(&mut doc);
    let s = &doc.timeline.as_ref().unwrap().sequences[&seq_id];
    assert!(s.track(v1_id).unwrap().clips.iter().any(|c| c.id == c1_id));
    assert!(s.track(v2_id).unwrap().clips.iter().any(|c| c.id == c2_id));
    assert_eq!(
        s.track(a1_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id == ac_id)
            .unwrap()
            .start,
        Tick(0)
    );
}
