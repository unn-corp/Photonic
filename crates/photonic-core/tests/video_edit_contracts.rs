use photonic_core::{
    history::{Command, CommandHistory},
    timeline::{
        clip::LinkGroupId, ops, Clip, ClipSource, FrameRate, PreviewZone, Sequence, Tick,
        TimelineProject, Track, TrackKind,
    },
    Color, Document,
};

fn fixture() -> (
    Document,
    photonic_core::timeline::SequenceId,
    photonic_core::timeline::TrackId,
    photonic_core::timeline::ClipId,
    photonic_core::timeline::TrackId,
) {
    let mut project = TimelineProject::new();
    let mut sequence = Sequence::new("linked", FrameRate::FPS_30, 320, 180);
    let mut video = Track::new(TrackKind::Video, "picture");
    let mut audio = Track::new(TrackKind::Audio, "sound");
    let group = LinkGroupId::new();
    for track in [&mut video, &mut audio] {
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: Color::rgb(1.0, 0.0, 0.0),
            },
            Tick(0),
            Tick(100),
        );
        clip.link_group = Some(group);
        track.clips.push(clip);
    }
    let ids = (sequence.id, video.id, video.clips[0].id, audio.id);
    sequence.video_tracks.push(video);
    sequence.audio_tracks.push(audio);
    project.insert_sequence(sequence);
    let mut document = Document::new("test", 320.0, 180.0);
    document.timeline = Some(project);
    (document, ids.0, ids.1, ids.2, ids.3)
}

#[test]
fn colliding_or_locked_link_partner_rejects_whole_move() {
    let (mut document, sequence, video, clip, audio) = fixture();
    let project = document.timeline.as_mut().unwrap();
    let lane = project
        .sequences
        .get_mut(&sequence)
        .unwrap()
        .track_mut(audio)
        .unwrap();
    lane.clips
        .push(Clip::new(ClipSource::Adjustment, Tick(200), Tick(100)));
    assert!(ops::move_linked_clip(project, sequence, video, clip, Tick(200), None).is_err());
    let lane = project
        .sequences
        .get_mut(&sequence)
        .unwrap()
        .track_mut(audio)
        .unwrap();
    lane.clips.pop();
    lane.locked = true;
    assert!(matches!(
        ops::move_linked_clip(project, sequence, video, clip, Tick(200), None),
        Err(ops::EditError::TrackLocked)
    ));
    assert_eq!(
        project.sequences[&sequence].track(video).unwrap().clips[0].start,
        Tick(0)
    );
}

#[test]
fn linked_cross_lane_move_and_undo_keep_audio_lane_and_offset() {
    let (mut document, sequence, video, clip, audio) = fixture();
    let project = document.timeline.as_mut().unwrap();
    let destination = Track::new(TrackKind::Video, "V2");
    let destination_id = destination.id;
    project
        .sequences
        .get_mut(&sequence)
        .unwrap()
        .video_tracks
        .push(destination);
    let before = project.clone();
    let commands = ops::move_linked_clip(
        project,
        sequence,
        video,
        clip,
        Tick(200),
        Some(destination_id),
    )
    .unwrap();
    let mut history = CommandHistory::new(10);
    history.execute_discrete(
        Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
        &mut document,
    );
    let result = &document.timeline.as_ref().unwrap().sequences[&sequence];
    assert_eq!(
        result.track(destination_id).unwrap().clips[0].start,
        Tick(200)
    );
    assert_eq!(result.track(audio).unwrap().clips[0].start, Tick(200));
    assert!(history.undo(&mut document));
    assert_eq!(document.timeline.as_ref().unwrap(), &before);
}

#[test]
fn preview_zones_merge_snap_serialize_and_undo() {
    let (mut document, sequence, _, _, _) = fixture();
    let rate = FrameRate::FPS_30;
    let frame = rate.ticks_per_frame().0;
    let command = ops::set_preview_zones(
        document.timeline.as_ref().unwrap(),
        sequence,
        &[
            PreviewZone {
                start: Tick(1),
                end: Tick(frame + 1),
            },
            PreviewZone {
                start: Tick(frame),
                end: Tick(3 * frame),
            },
            PreviewZone {
                start: Tick(5 * frame),
                end: Tick(6 * frame),
            },
        ],
    )
    .unwrap();
    let mut history = CommandHistory::new(10);
    history.execute_discrete(Command::Timeline(command), &mut document);
    let zones = &document.timeline.as_ref().unwrap().sequences[&sequence].preview_zones;
    assert_eq!(
        zones,
        &[
            PreviewZone {
                start: Tick(0),
                end: Tick(3 * frame)
            },
            PreviewZone {
                start: Tick(5 * frame),
                end: Tick(6 * frame)
            }
        ]
    );
    let saved = serde_json::to_value(document.timeline.as_ref().unwrap()).unwrap();
    let loaded: TimelineProject = serde_json::from_value(saved).unwrap();
    assert_eq!(&loaded.sequences[&sequence].preview_zones, zones);
    assert!(history.undo(&mut document));
    assert!(document.timeline.as_ref().unwrap().sequences[&sequence]
        .preview_zones
        .is_empty());
}
