//! Write a minimal `.photon` that already carries a timeline project, so the
//! GUI auto-enters video mode when it opens the file (04 §1.2 "Auto-enter on
//! open"). Used for eyeballing the video-editor layout without clicking through
//! the welcome screen.
//!
//! `cargo run -p photonic-core --example gen_timeline_fixture -- /tmp/vid.photon`

use photonic_core::timeline::sequence::{Sequence, Track};
use photonic_core::timeline::{ops, FrameRate, TrackKind};
use photonic_core::{Command, CommandHistory, Document};

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/vid.photon".to_string());

    let mut doc = Document::new("Timeline Fixture", 1920.0, 1080.0);
    let mut history = CommandHistory::default();
    history.execute(Command::Timeline(ops::create_project()), &mut doc);
    let seq = Sequence::new("Sequence 1", FrameRate::FPS_30, 1920, 1080);
    let seq_id = seq.id;
    history.execute(Command::Timeline(ops::add_sequence(seq)), &mut doc);
    for (kind, name) in [
        (TrackKind::Video, "V1"),
        (TrackKind::Video, "V2"),
        (TrackKind::Text, "T1"),
        (TrackKind::Audio, "A1"),
        (TrackKind::Audio, "A2"),
    ] {
        let cmd = {
            let project = doc.timeline.as_ref().expect("timeline");
            ops::add_track(project, seq_id, Track::new(kind, name), None).expect("add track")
        };
        history.execute(Command::Timeline(cmd), &mut doc);
    }

    let json = photonic_core::photon_file::save_photon(&doc, None).expect("serialize");
    std::fs::write(&out, json).expect("write");
    println!("wrote {out}");
}
