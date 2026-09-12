//! Pure precision edit planning. GUI previews and automation commit the same
//! commands; cancellation never requires an inverse edit.
use super::{
    ops, Clip, ClipId, ClipSource, ClipTiming, SequenceId, Tick, TimelineCmd, TimelineProject,
    TrackId,
};
use ops::EditError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrimTarget {
    pub sequence: SequenceId,
    pub track: TrackId,
    pub outgoing: ClipId,
    pub incoming: ClipId,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrimMode {
    #[default]
    Roll,
    RippleOutgoing,
    RippleIncoming,
    SlipOutgoing,
    SlipIncoming,
}
impl TrimMode {
    pub const ALL: [Self; 5] = [
        Self::Roll,
        Self::RippleOutgoing,
        Self::RippleIncoming,
        Self::SlipOutgoing,
        Self::SlipIncoming,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Roll => "Roll both sides",
            Self::RippleOutgoing => "Ripple outgoing Out",
            Self::RippleIncoming => "Ripple incoming In",
            Self::SlipOutgoing => "Slip outgoing source",
            Self::SlipIncoming => "Slip incoming source",
        }
    }
    pub fn next(self) -> Self {
        Self::ALL[(Self::ALL.iter().position(|v| *v == self).unwrap_or(0) + 1) % Self::ALL.len()]
    }
}

/// Resolve the nearest flush edit involving the primary selected clip, or the
/// nearest edit to the playhead when no clip is selected. Gaps are not cuts.
pub fn resolve_target(
    project: &TimelineProject,
    sequence: SequenceId,
    selected: Option<ClipId>,
    at: Tick,
) -> Result<TrimTarget, EditError> {
    let seq = project
        .sequences
        .get(&sequence)
        .ok_or(EditError::NoSequence(sequence))?;
    let mut nearest = None;
    for track in seq.tracks() {
        let mut clips: Vec<_> = track.clips.iter().collect();
        clips.sort_by_key(|clip| clip.start);
        for pair in clips.windows(2) {
            let [left, right] = pair else {
                continue;
            };
            if left.start.0.checked_add(left.duration.0) != Some(right.start.0)
                || selected.is_some_and(|id| id != left.id && id != right.id)
                || matches!(left.source, ClipSource::Adjustment)
                || matches!(right.source, ClipSource::Adjustment)
            {
                continue;
            }
            let distance = right.start.0.abs_diff(at.0);
            if nearest.as_ref().is_none_or(|(best, _)| distance < *best) {
                nearest = Some((
                    distance,
                    TrimTarget {
                        sequence,
                        track: track.id,
                        outgoing: left.id,
                        incoming: right.id,
                    },
                ));
            }
        }
    }
    nearest
        .map(|(_, target)| target)
        .ok_or(EditError::InvalidSplit)
}

fn add(a: Tick, b: Tick) -> Result<Tick, EditError> {
    a.0.checked_add(b.0)
        .map(Tick)
        .ok_or(EditError::InvalidSplit)
}
fn sub(a: Tick, b: Tick) -> Result<Tick, EditError> {
    a.0.checked_sub(b.0)
        .map(Tick)
        .ok_or(EditError::InvalidSplit)
}
fn get_clip(
    project: &TimelineProject,
    target: TrimTarget,
    track: TrackId,
    id: ClipId,
) -> Result<&Clip, EditError> {
    project
        .sequences
        .get(&target.sequence)
        .ok_or(EditError::NoSequence(target.sequence))?
        .track(track)
        .ok_or(EditError::NoTrack(track))?
        .clips
        .iter()
        .find(|clip| clip.id == id)
        .ok_or(EditError::NoClip(id))
}

/// All required linked partners and sync-locked downstream tracks are checked
/// before returning any commands. Delta uses the sequence clock. The caller
/// wraps this vector in one history Batch.
pub fn plan_trim(
    project: &TimelineProject,
    target: TrimTarget,
    mode: TrimMode,
    delta: Tick,
) -> Result<Vec<TimelineCmd>, EditError> {
    let sequence = project
        .sequences
        .get(&target.sequence)
        .ok_or(EditError::NoSequence(target.sequence))?;
    let left = get_clip(project, target, target.track, target.outgoing)?;
    let right = get_clip(project, target, target.track, target.incoming)?;
    if add(left.start, left.duration)? != right.start
        || left.id == right.id
        || matches!(left.source, ClipSource::Adjustment)
        || matches!(right.source, ClipSource::Adjustment)
    {
        return Err(EditError::InvalidSplit);
    }
    if mode == TrimMode::Roll {
        // Include every linked outgoing/incoming partner. A matching cut is
        // required on each involved track, so no partner is silently stranded.
        let moving = ops::linked_moving_set(
            project,
            target.sequence,
            &[(target.track, left.id), (target.track, right.id)],
        )?;
        let mut tracks = Vec::new();
        for (track, _) in moving {
            if !tracks.contains(&track) {
                tracks.push(track);
            }
        }
        let mut result = Vec::new();
        for track in tracks {
            let t = sequence.track(track).ok_or(EditError::NoTrack(track))?;
            let outgoing = t
                .clips
                .iter()
                .find(|clip| clip.start.0.checked_add(clip.duration.0) == Some(right.start.0))
                .ok_or(EditError::InvalidSplit)?;
            let incoming = t
                .clips
                .iter()
                .find(|clip| clip.start == right.start)
                .ok_or(EditError::InvalidSplit)?;
            let command = ops::roll_edit(
                project,
                target.sequence,
                track,
                outgoing.id,
                incoming.id,
                delta,
            )?;
            if delta != Tick::ZERO {
                result.push(command);
            }
        }
        return Ok(result);
    }
    let outgoing = matches!(mode, TrimMode::RippleOutgoing | TrimMode::SlipOutgoing);
    let edited = if outgoing { left } else { right };
    let participants =
        ops::linked_moving_set(project, target.sequence, &[(target.track, edited.id)])?;
    let slip = matches!(mode, TrimMode::SlipOutgoing | TrimMode::SlipIncoming);
    let ripple_shift = if outgoing {
        delta
    } else {
        sub(Tick::ZERO, delta)?
    };
    let point = add(edited.start, edited.duration)?;
    let mut result = Vec::new();
    for track in sequence.tracks() {
        let members: Vec<_> = participants
            .iter()
            .filter(|(id, _)| *id == track.id)
            .map(|(_, id)| *id)
            .collect();
        if members.is_empty()
            && (slip || !track.sync_lock || !track.clips.iter().any(|clip| clip.start >= point))
        {
            continue;
        }
        if track.locked {
            return Err(EditError::TrackLocked);
        }
        let mut changes = Vec::new();
        for clip in &track.clips {
            let old = ClipTiming::of(clip);
            let mut new = old;
            if members.contains(&clip.id) {
                if clip.start != edited.start || clip.duration != edited.duration {
                    return Err(EditError::InvalidSplit);
                }
                if slip || !outgoing {
                    new.source_in = add(
                        clip.source_in,
                        ops::checked_source_delta(&clip.speed, delta)?,
                    )?;
                }
                if !slip {
                    new.duration = add(clip.duration, ripple_shift)?;
                }
                ops::validate_source_timing(project, clip, new)?;
            } else if !slip && clip.start >= point {
                new.start = add(clip.start, ripple_shift)?;
                if new.start < Tick::ZERO {
                    return Err(EditError::InvalidSplit);
                }
            }
            if old != new {
                changes.push((clip.id, old, new));
            }
        }
        // Reject downstream collisions instead of skipping required changes.
        let mut timings: Vec<_> = track
            .clips
            .iter()
            .map(|clip| {
                changes
                    .iter()
                    .find(|(id, _, _)| *id == clip.id)
                    .map_or(ClipTiming::of(clip), |(_, _, new)| *new)
            })
            .collect();
        timings.sort_by_key(|timing| timing.start);
        for pair in timings.windows(2) {
            if add(pair[0].start, pair[0].duration)? > pair[1].start {
                return Err(EditError::Overlap);
            }
        }
        if !changes.is_empty() {
            result.push(TimelineCmd::RippleEdit {
                seq: target.sequence,
                track: track.id,
                changes,
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::clip::LinkGroupId;
    use crate::timeline::{AssetId, FrameRate, Ratio, Sequence, SpeedMap, Track, TrackKind};
    use crate::{
        document::Document,
        history::{Command, CommandHistory},
    };
    fn fixture() -> (TimelineProject, TrimTarget) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("edit", FrameRate::FPS_30, 1920, 1080);
        let a = LinkGroupId::new();
        let b = LinkGroupId::new();
        for kind in [TrackKind::Video, TrackKind::Audio] {
            let mut track = Track::new(kind, "linked");
            let mut left = Clip::new(
                ClipSource::Asset {
                    asset: AssetId::new(),
                },
                Tick(0),
                Tick(100),
            );
            left.link_group = Some(a);
            left.source_in = Tick(100);
            let mut right = Clip::new(left.source.clone(), Tick(100), Tick(100));
            right.link_group = Some(b);
            right.source_in = Tick(100);
            track.clips = vec![left, right];
            if kind == TrackKind::Video {
                seq.video_tracks.push(track)
            } else {
                seq.audio_tracks.push(track)
            };
        }
        let track = &seq.video_tracks[0];
        let target = TrimTarget {
            sequence: seq.id,
            track: track.id,
            outgoing: track.clips[0].id,
            incoming: track.clips[1].id,
        };
        project.insert_sequence(seq);
        (project, target)
    }
    #[test]
    fn precision_roll_preserves_linked_sync_and_one_undo() {
        let (project, target) = fixture();
        let commands = plan_trim(&project, target, TrimMode::Roll, Tick(10)).unwrap();
        assert_eq!(commands.len(), 2);
        let mut doc = Document::new("test", 1., 1.);
        doc.timeline = Some(project.clone());
        let mut history = CommandHistory::new(8);
        history.execute_discrete(
            Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
            &mut doc,
        );
        let seq = &doc.timeline.as_ref().unwrap().sequences[&target.sequence];
        for t in seq.tracks() {
            assert_eq!(t.clips[0].duration, Tick(110));
            assert_eq!(t.clips[1].start, Tick(110));
        }
        history.undo(&mut doc);
        assert_eq!(doc.timeline, Some(project));
    }
    #[test]
    fn precision_refuses_locked_linked_and_sync_tracks_atomically() {
        let (mut project, target) = fixture();
        project
            .sequences
            .get_mut(&target.sequence)
            .unwrap()
            .audio_tracks[0]
            .locked = true;
        assert_eq!(
            plan_trim(&project, target, TrimMode::Roll, Tick(10)),
            Err(EditError::TrackLocked)
        );
        let audio = &mut project
            .sequences
            .get_mut(&target.sequence)
            .unwrap()
            .audio_tracks[0];
        audio
            .clips
            .iter_mut()
            .for_each(|clip| clip.link_group = None);
        audio.sync_lock = true;
        assert_eq!(
            plan_trim(&project, target, TrimMode::RippleOutgoing, Tick(10)),
            Err(EditError::TrackLocked)
        );
    }
    #[test]
    fn precision_roll_uses_source_speed_and_rejects_gaps_overflow_bounds() {
        let (mut project, target) = fixture();
        project
            .sequences
            .get_mut(&target.sequence)
            .unwrap()
            .video_tracks[0]
            .clips[1]
            .speed = SpeedMap::Constant(Ratio::new(2, 1));
        let commands = plan_trim(&project, target, TrimMode::Roll, Tick(10)).unwrap();
        let TimelineCmd::RollEdit { changes, .. } = &commands[0] else {
            panic!()
        };
        assert_eq!(changes[1].2.source_in, Tick(120));
        assert!(plan_trim(&project, target, TrimMode::Roll, Tick(i64::MAX)).is_err());
        assert!(plan_trim(&project, target, TrimMode::SlipOutgoing, Tick(-101)).is_err());
        project
            .sequences
            .get_mut(&target.sequence)
            .unwrap()
            .video_tracks[0]
            .clips[1]
            .start = Tick(101);
        assert!(plan_trim(&project, target, TrimMode::Roll, Tick(1)).is_err());
    }
    #[test]
    fn precision_roll_checks_known_source_end_and_keyframed_mapping() {
        let (mut project, target) = fixture();
        let left = project.sequences[&target.sequence].video_tracks[0].clips[0].clone();
        let ClipSource::Asset { asset: id } = left.source else {
            panic!()
        };
        let mut asset =
            crate::timeline::MediaAsset::from_file(crate::timeline::AssetKind::Video, "short.mp4");
        asset.id = id;
        asset.probe = Some(crate::timeline::MediaProbe::basic(Tick(205), "mp4", "h264"));
        project.media.assets.insert(id, asset);
        assert!(plan_trim(&project, target, TrimMode::Roll, Tick(10)).is_err());
        project.media.assets.clear();
        let right = &mut project
            .sequences
            .get_mut(&target.sequence)
            .unwrap()
            .video_tracks[0]
            .clips[1];
        right.speed = SpeedMap::Keyframed {
            keys: vec![
                crate::timeline::clip::SpeedKey::new(Tick::ZERO, Ratio::new(2, 1)),
                crate::timeline::clip::SpeedKey::new(Tick(50), Ratio::new(3, 1)),
            ],
        };
        let commands = plan_trim(&project, target, TrimMode::Roll, Tick(20)).unwrap();
        let TimelineCmd::RollEdit { changes, .. } = &commands[0] else {
            panic!()
        };
        assert_eq!(changes[1].2.source_in, Tick(140));
    }

    #[test]
    fn precision_ripple_moves_linked_and_sync_tracks_once() {
        let (mut project, target) = fixture();
        let seq = project.sequences.get_mut(&target.sequence).unwrap();
        seq.audio_tracks[0].sync_lock = true;
        let mut music = Track::new(TrackKind::Audio, "music");
        music.sync_lock = true;
        music.clips.push(Clip::new(
            ClipSource::Asset {
                asset: AssetId::new(),
            },
            Tick(200),
            Tick(100),
        ));
        seq.audio_tracks.push(music);
        let commands = plan_trim(&project, target, TrimMode::RippleOutgoing, Tick(-10)).unwrap();
        assert_eq!(commands.len(), 3);
        for command in &commands {
            let TimelineCmd::RippleEdit { changes, .. } = command else {
                panic!()
            };
            for (_, old, new) in changes {
                if old.start >= Tick(100) {
                    assert_eq!(new.start, old.start - Tick(10));
                }
            }
        }
    }
}
