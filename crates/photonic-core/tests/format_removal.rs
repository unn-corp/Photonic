use photonic_core::{
    history::{Command, CommandHistory},
    timeline::*,
    Document,
};

#[test]
fn removing_middle_format_preserves_active_identity_and_reframes_through_undo() {
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("Formats", FrameRate::new(30, 1), 1920, 1080);
    sequence
        .formats
        .push(SequenceFormat::new("Square", 1080, 1080));
    sequence
        .formats
        .push(SequenceFormat::new("Portrait", 1080, 1920));
    sequence.active_format = 2;
    let mut track = Track::new(TrackKind::Video, "V1");
    track.locked = true;
    let mut clip = Clip::new(
        ClipSource::SolidColor {
            color: photonic_core::Color::new(1.0, 0.0, 0.0, 1.0),
        },
        Tick::ZERO,
        Tick::from_seconds(5),
    );
    clip.reframe.insert(
        1,
        ClipTransform {
            x: 12.0,
            ..Default::default()
        },
    );
    clip.reframe.insert(
        2,
        ClipTransform {
            x: 34.0,
            ..Default::default()
        },
    );
    track.clips.push(clip);
    sequence.video_tracks.push(track);
    let id = project.insert_sequence(sequence);
    project.active_sequence = Some(id);
    let before = project.clone();
    let commands = ops::remove_sequence_format(&project, id, 1).unwrap();
    let mut doc = Document::new("test", 1920.0, 1080.0);
    doc.timeline = Some(project);
    let mut history = CommandHistory::new(64);
    history.execute_discrete(
        Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
        &mut doc,
    );
    let seq = &doc.timeline.as_ref().unwrap().sequences[&id];
    assert_eq!(seq.active_format, 1);
    assert_eq!(seq.formats[seq.active_format].name, "Portrait");
    assert_eq!(seq.video_tracks[0].clips[0].reframe.len(), 1);
    assert_eq!(seq.video_tracks[0].clips[0].reframe[&1].x, 34.0);
    let after = doc.timeline.clone();
    history.undo(&mut doc);
    assert_eq!(
        serde_json::to_value(doc.timeline.as_ref().unwrap()).unwrap(),
        serde_json::to_value(before).unwrap()
    );
    history.redo(&mut doc);
    assert_eq!(
        serde_json::to_value(doc.timeline).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}

#[test]
fn removing_last_or_missing_format_is_rejected() {
    let mut project = TimelineProject::new();
    let id = project.insert_sequence(Sequence::new("Formats", FrameRate::new(30, 1), 1920, 1080));
    assert!(ops::remove_sequence_format(&project, id, 0).is_err());
    assert!(ops::remove_sequence_format(&project, id, 1).is_err());
}
