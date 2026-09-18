//! `EngineBridge` integration: the GUI-side mirror (tokio-app ↔ std-mutex
//! engine lock-flavor bridge in `app/engine.rs`) really feeds the engine, and
//! the 03 §5 present path registers an egui native texture — all against a
//! REAL headless `VideoEngine`. Skips (never fails) without a GPU adapter,
//! matching the photonic-video suite's convention.

use std::time::{Duration, Instant};

use photonic_core::timeline::{
    Clip, ClipSource, FrameRate, Sequence, Tick, TimelineProject, Track, TrackKind,
};
use photonic_core::{Color, CommandHistory, Document};
use photonic_gui::EngineBridge;
use photonic_video::{EngineCmd, VideoEngine};

fn solid_project() -> TimelineProject {
    let mut project = TimelineProject::new();
    let mut seq = Sequence::new("seq", FrameRate::FPS_30, 320, 180);
    let seq_id = seq.id;
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 1.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
        },
        Tick(0),
        Tick::from_seconds(5),
    ));
    seq.video_tracks.push(v1);
    project.insert_sequence(seq);
    project.active_sequence = Some(seq_id);
    project
}

#[test]
fn bridge_mirrors_document_and_presents_to_egui_texture() {
    let Some(engine) = VideoEngine::headless() else {
        eprintln!("no GPU adapter — skipping engine bridge test");
        return;
    };
    // Grab owned device/queue handles for the egui renderer before the bridge
    // takes ownership of the engine.
    let device = std::sync::Arc::clone(engine.gpu().device());
    let queue = std::sync::Arc::clone(engine.gpu().queue());

    let mut bridge = EngineBridge::new(engine);

    // The app-side document/history — deliberately NOT the mirror pair.
    let mut doc = Document::new("bridge-test", 320.0, 180.0);
    doc.timeline = Some(solid_project());
    let history = CommandHistory::new(8);

    // 1. Mirror sync: one call must be enough for the engine to observe the
    //    timeline (revision-bumped mirror → engine re-snapshot → present).
    bridge.sync_document(&doc, &history);
    bridge.session().send(EngineCmd::Seek(Tick(0)));

    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(f) = bridge.session().latest_frame() {
            break f;
        }
        assert!(
            Instant::now() < deadline,
            "engine never produced a frame from the mirrored document"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(frame.time, Tick(0));
    let active = doc.timeline.as_ref().unwrap().active_sequence.unwrap();
    assert_eq!(frame.sequence, active);

    // 2. Present path (03 §5): present_latest must register an egui native
    //    texture sized to the frame's physical (bucket-padded) texture.
    let mut egui_renderer =
        egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm, None, 1, false);
    bridge.present_latest(&device, &queue, &mut egui_renderer);
    let tex = bridge
        .monitor_tex_info()
        .expect("present_latest registers the monitor texture");
    assert_eq!(tex.0, (frame.texture.width(), frame.texture.height()));
    // Logical 320x180 in a 64px-bucket physical texture: width matches, height
    // is padded up (192) — exactly what the monitor's UV crop compensates for.
    assert!(tex.0 .0 >= 320 && tex.0 .1 >= 180);

    // 3. Status flows: the engine observed the immutable snapshot published
    // by the bridge. A freshly-created history legitimately starts at
    // revision zero; generation is the snapshot-observation barrier.
    let status = bridge.status();
    assert!(
        status.snapshot_generation > 0,
        "engine snapshot not observed"
    );
    assert_eq!(status.doc_revision, history.revision());
}
