//! CAP-022 crash-recovery integration test (11 §6/§7's P3 exit criterion —
//! "kill the process mid-edit on a timeline project; relaunch offers
//! recovery; restored document contains the timeline state").
//!
//! Drives the REAL recovery machinery — not a reimplementation of it:
//! `recovery_write_for_test`/`recovery_load_for_test` (see
//! `crates/photonic-gui/src/app/autosave.rs`) are thin pass-throughs to the
//! exact `write_photon_file`/`load_document` functions `autosave_all`
//! (autosave.rs) and `draw_recovery_prompt`'s Restore branch (recovery.rs)
//! call in production. Those two functions being private to the `app` module
//! tree — and only reachable from here through a fully `pub` module chain —
//! is why `app::autosave` moved from `pub(crate)` to `pub` (see the doc
//! comment on that `mod` declaration in `app/mod.rs`); no other visibility
//! changed.

use photonic_core::timeline::{
    Clip, ClipSource, FrameRate, Marker, Sequence, Tick, TimelineProject, Track, TrackKind,
};
use photonic_core::{Color, CommandHistory, Document};
use photonic_gui::app::autosave::{recovery_load_for_test, recovery_write_for_test};

/// A populated `TimelineProject`: two sequences (one nested into the other via
/// `ClipSource::NestedSequence`), video + audio tracks, multiple clips with
/// non-default names/positions, a work range, and markers (one with a note) —
/// enough surface area that a lossy round trip would show up in the equality
/// check below.
fn populated_project() -> TimelineProject {
    let mut project = TimelineProject::new();

    let mut main = Sequence::new("Main", FrameRate::FPS_30, 1920, 1080);
    let main_id = main.id;

    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0,
            },
        },
        Tick(0),
        Tick::from_seconds(3),
    ));
    let mut card = Clip::new(
        ClipSource::SolidColor {
            color: Color {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
        },
        Tick::from_seconds(3),
        Tick::from_seconds(2),
    );
    card.name = "white card".to_string();
    v1.clips.push(card);
    main.video_tracks.push(v1);
    main.audio_tracks.push(Track::new(TrackKind::Audio, "A1"));

    main.markers
        .push(Marker::new(Tick::from_seconds(1), "beat 1"));
    let mut beat2 = Marker::new(Tick::from_seconds(4), "beat 2");
    beat2.note = "sync point".to_string();
    main.markers.push(beat2);
    main.work_range = Some((Tick(0), Tick::from_seconds(5)));

    project.insert_sequence(main);
    project.active_sequence = Some(main_id);

    // A second sequence, nested into the first via a clip — exercises
    // multi-sequence + `ClipSource::NestedSequence` round trip too.
    let nested = Sequence::new("Nested", FrameRate::FPS_30, 640, 360);
    let nested_id = nested.id;
    project.insert_sequence(nested);
    if let Some(main) = project.sequences.get_mut(&main_id) {
        let mut v2 = Track::new(TrackKind::Video, "V2");
        v2.clips.push(Clip::new(
            ClipSource::NestedSequence {
                sequence: nested_id,
            },
            Tick::from_seconds(5),
            Tick::from_seconds(1),
        ));
        main.video_tracks.push(v2);
    }

    project
}

#[test]
fn recovery_round_trip_preserves_timeline() {
    // Hermetic recovery-file location: a dedicated temp dir rather than the
    // real `recovery_dir()` (the user's OS crash-report cache), so the test
    // can't collide with or leave litter in an actual crash-recovery folder.
    // `write_photon_file`/`load_document` operate on a plain path either way,
    // so this exercises the same code the real `recovery_dir()`-rooted path
    // would run.
    // Instant Debug includes ':' / braces that are invalid on Windows paths
    // (ERROR_DIRECTORY 267). Use a path-safe nanos suffix instead.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "photonic-recovery-test-{}-{}",
        std::process::id(),
        stamp
    ));
    std::fs::create_dir_all(&dir).expect("create hermetic recovery-test dir");
    let path = dir.join("untitled-1.photon");

    // ── Arrange: an in-memory Document with a populated timeline, exactly as
    //    an untitled tab under active edit holds it. ───────────────────────
    let mut doc = Document::new("Untitled", 1920.0, 1080.0);
    let project = populated_project();
    doc.timeline = Some(project.clone());
    let mut history = CommandHistory::new(64);

    // ── Act 1: the real crash-recovery snapshot write path — exactly what
    //    `autosave_all` runs for a dirty untitled tab. ──────────────────────
    recovery_write_for_test(&path, &doc, &mut history)
        .expect("recovery snapshot write must succeed");
    assert!(
        path.is_file(),
        "recovery snapshot must land on disk at the recovery path"
    );

    // ── Act 2: the real relaunch scan/load path — exactly what
    //    `draw_recovery_prompt`'s Restore branch runs for each `.photon` file
    //    `recovery_dir()`'s directory scan turns up. ─────────────────────────
    let (loaded_doc, _snapshot) =
        recovery_load_for_test(&path).expect("recovery snapshot load must succeed");

    // ── Assert: the timeline survives the crash-recovery round trip with
    //    full fidelity (sequences, tracks, clips, transform state, markers,
    //    work range, and the nested-sequence reference) — `TimelineProject`'s
    //    derived `PartialEq` is a deep structural comparison, so this is a
    //    byte-for-byte round trip of every field, not just a spot check. ────
    assert_eq!(
        loaded_doc.timeline,
        Some(project),
        "timeline must round-trip through the CAP-022 recovery path with no data loss"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
