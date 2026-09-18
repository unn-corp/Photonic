//! Derived transcript views and shared, undoable text/timeline edit plans.
//!
//! Words remain in caption cues. The projection is disposable UI/API data;
//! deleting program time uses the existing lift/extract operations and caption
//! commands together, so callers commit the entire plan as one history batch.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::{
    ops, CaptionCmd, CaptionCue, CaptionTrack, ClipId, CueId, EditError, FrameRate, Sequence,
    SequenceId, Tick, TimelineCmd, TimelineProject, TrackId,
};
use crate::Document;

/// A word addressed in its original caption cue, with sequence-time bounds.
/// Passing the complete reference back to a planner detects stale previews.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptTokenRef {
    pub track: TrackId,
    pub cue: CueId,
    pub word_index: usize,
    pub text: String,
    pub start: Tick,
    pub end: Tick,
}

/// A cache-only transcript, ordered by start tick, then source cue/word order.
#[derive(Clone, Debug, Default)]
pub struct TranscriptProjection {
    pub tokens: Vec<TranscriptTokenRef>,
    /// Number of words clamped to their cue or omitted for empty timing.
    pub adjusted_word_count: usize,
    /// Overlaps remain visible for recovery, but timing edits refuse them.
    pub has_overlapping_words: bool,
    prefix_max_end: Vec<Tick>,
}

impl TranscriptProjection {
    /// Finds an active word in O(log n), including overlapping provider output.
    /// At a cut the following word wins; word intervals are half-open.
    pub fn active_at(&self, at: Tick) -> Option<usize> {
        let started = self.tokens.partition_point(|word| word.start <= at);
        let first_active = self.prefix_max_end[..started].partition_point(|end| *end <= at);
        (first_active < started).then_some(first_active)
    }

    /// Resolve a preview/selection reference, refusing changed text or timing.
    pub fn resolve(&self, reference: &TranscriptTokenRef) -> Result<usize, TranscriptError> {
        let first = self
            .tokens
            .partition_point(|word| word.start < reference.start);
        self.tokens[first..]
            .iter()
            .take_while(|word| word.start == reference.start)
            .position(|word| word == reference)
            .map(|offset| first + offset)
            .ok_or(TranscriptError::StaleToken)
    }
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum TranscriptError {
    #[error("{0}")]
    Edit(#[from] EditError),
    #[error("caption track {0} does not belong to the specified sequence")]
    NoCaptionTrack(TrackId),
    #[error("caption cue {0} no longer exists on this track")]
    NoCue(CueId),
    #[error("word index {0} no longer exists in this cue")]
    NoWord(usize),
    #[error("the transcript selection changed; select or preview the words again")]
    StaleToken,
    #[error("select words with a positive, non-negative time range")]
    InvalidRange,
    #[error("the sequence has an invalid frame rate")]
    InvalidFrameRate,
    #[error("select a dialogue clip to establish the target tracks")]
    EmptyScope,
    #[error("select one dialogue clip or clips in the same linked group")]
    AmbiguousDialogueSelection,
    #[error("required track {0} is locked; no part of this edit was applied")]
    TrackLocked(TrackId),
    #[error("caption words overlap; correct their timing before deleting program time")]
    OverlappingWordTiming,
    #[error(
        "caption words have invalid timing; correct their timing before deleting program time"
    )]
    InvalidWordTiming,
    #[error("caption cues overlap or have invalid bounds; correct their timing before deleting program time")]
    InvalidCueTiming,
    #[error("the selected range contains no media on the target tracks")]
    NoMediaInRange,
}

fn sequence(project: &TimelineProject, id: SequenceId) -> Result<&Sequence, TranscriptError> {
    project
        .sequences
        .get(&id)
        .ok_or_else(|| EditError::NoSequence(id).into())
}

fn caption_track(
    project: &TimelineProject,
    sequence_id: SequenceId,
    track: TrackId,
) -> Result<&CaptionTrack, TranscriptError> {
    sequence(project, sequence_id)?
        .caption_tracks
        .iter()
        .find(|candidate| candidate.id == track)
        .ok_or(TranscriptError::NoCaptionTrack(track))
}

/// Project caption words without changing saved text or provider timings.
/// Malformed bounds are clamped for display; the original caption remains
/// available for correction and exact undo restoration.
pub fn get_transcript(
    project: &TimelineProject,
    sequence_id: SequenceId,
    track: TrackId,
) -> Result<TranscriptProjection, TranscriptError> {
    let track = caption_track(project, sequence_id, track)?;
    let mut projection = TranscriptProjection::default();
    for cue in &track.cues {
        let cue_start = cue.start.max(Tick::ZERO);
        let cue_end = cue.end.max(cue_start);
        for (word_index, word) in cue.words.iter().enumerate() {
            let start = word.start.clamp(cue_start, cue_end);
            let end = word.end.clamp(start, cue_end);
            if start != word.start || end != word.end || end <= start {
                projection.adjusted_word_count += 1;
            }
            if end <= start {
                continue;
            }
            projection.tokens.push(TranscriptTokenRef {
                track: track.id,
                cue: cue.id,
                word_index,
                text: word.text.clone(),
                start,
                end,
            });
        }
    }
    // Stable sorting preserves cue/word order when provider start ticks tie.
    projection.tokens.sort_by_key(|word| word.start);
    let mut max_end = Tick::ZERO;
    for word in &projection.tokens {
        projection.has_overlapping_words |= word.start < max_end;
        max_end = max_end.max(word.end);
        projection.prefix_max_end.push(max_end);
    }
    Ok(projection)
}

/// Replace a word's wording, preserving every timing and style field.
pub fn edit_transcript_word(
    project: &TimelineProject,
    sequence_id: SequenceId,
    track: TrackId,
    cue: CueId,
    word_index: usize,
    text: String,
) -> Result<Vec<TimelineCmd>, TranscriptError> {
    let cue = caption_track(project, sequence_id, track)?
        .cues
        .iter()
        .find(|candidate| candidate.id == cue)
        .ok_or(TranscriptError::NoCue(cue))?;
    let word = cue
        .words
        .get(word_index)
        .ok_or(TranscriptError::NoWord(word_index))?;
    if word.text == text {
        return Ok(Vec::new());
    }
    let mut new_words = cue.words.clone();
    new_words[word_index].text = text;
    Ok(vec![TimelineCmd::CaptionEdit(CaptionCmd::SetCueText {
        track,
        cue: cue.id,
        old_words: cue.words.clone(),
        new_words,
    })])
}

/// Remove selected wording only. Surviving words and all media retain their
/// exact positions. An emptied cue retains its timing as an empty placeholder.
pub fn delete_transcript_text(
    project: &TimelineProject,
    sequence_id: SequenceId,
    track: TrackId,
    words: &[TranscriptTokenRef],
) -> Result<Vec<TimelineCmd>, TranscriptError> {
    let projection = get_transcript(project, sequence_id, track)?;
    for word in words {
        projection.resolve(word)?;
    }
    let selected: HashSet<_> = words
        .iter()
        .map(|word| (word.cue, word.word_index))
        .collect();
    let track = caption_track(project, sequence_id, track)?;
    let mut commands = Vec::new();
    for cue in &track.cues {
        if !selected.iter().any(|(id, _)| *id == cue.id) {
            continue;
        }
        let new_words: Vec<_> = cue
            .words
            .iter()
            .enumerate()
            .filter(|(index, _)| !selected.contains(&(cue.id, *index)))
            .map(|(_, word)| word.clone())
            .collect();
        commands.push(TimelineCmd::CaptionEdit(CaptionCmd::SetCueText {
            track: track.id,
            cue: cue.id,
            old_words: cue.words.clone(),
            new_words,
        }));
    }
    Ok(commands)
}

/// Expand the GUI's selected dialogue clip to its linked group and every
/// sync-locked track. Locked participants are included and rejected atomically.
/// MCP instead supplies an explicit target-track list to the edit planners.
pub fn dialogue_track_scope(
    project: &TimelineProject,
    sequence_id: SequenceId,
    selection: &[ClipId],
) -> Result<Vec<TrackId>, TranscriptError> {
    let sequence = sequence(project, sequence_id)?;
    let first = selection.first().ok_or(TranscriptError::EmptyScope)?;
    let (primary_track, primary_clip) = sequence
        .tracks()
        .find_map(|track| {
            track
                .clips
                .iter()
                .find(|clip| clip.id == *first)
                .map(|clip| (track, clip))
        })
        .ok_or(EditError::NoClip(*first))?;
    for clip_id in selection {
        let clip = sequence
            .tracks()
            .flat_map(|track| &track.clips)
            .find(|clip| clip.id == *clip_id)
            .ok_or(EditError::NoClip(*clip_id))?;
        if clip.id != primary_clip.id
            && (primary_clip.link_group.is_none() || clip.link_group != primary_clip.link_group)
        {
            return Err(TranscriptError::AmbiguousDialogueSelection);
        }
    }
    let targets: Vec<_> = sequence
        .tracks()
        .filter(|track| {
            track.id == primary_track.id
                || track.sync_lock
                || primary_clip.link_group.is_some_and(|group| {
                    track
                        .clips
                        .iter()
                        .any(|clip| clip.link_group == Some(group))
                })
        })
        .map(|track| track.id)
        .collect();
    validate_tracks(sequence, &targets)?;
    Ok(targets)
}

fn validate_tracks(sequence: &Sequence, targets: &[TrackId]) -> Result<(), TranscriptError> {
    if targets.is_empty() {
        return Err(TranscriptError::EmptyScope);
    }
    for id in targets {
        let track = sequence.track(*id).ok_or(EditError::NoTrack(*id))?;
        if track.locked {
            return Err(TranscriptError::TrackLocked(*id));
        }
    }
    Ok(())
}

/// Snap a half-open range outwards so selected speech is fully removed.
pub fn snap_range_outward(
    range: (Tick, Tick),
    frame_rate: FrameRate,
) -> Result<(Tick, Tick), TranscriptError> {
    let (start, end) = range;
    if start < Tick::ZERO || end <= start {
        return Err(TranscriptError::InvalidRange);
    }
    if frame_rate.num == 0 || frame_rate.den == 0 {
        return Err(TranscriptError::InvalidFrameRate);
    }
    let frame = frame_rate.ticks_per_frame().0.max(1);
    let start = Tick(start.0 / frame * frame);
    let remainder = end.0 % frame;
    let end = if remainder == 0 {
        end
    } else {
        Tick(
            end.0
                .checked_add(frame - remainder)
                .ok_or(TranscriptError::InvalidRange)?,
        )
    };
    Ok((start, end))
}

/// Map selected tokens to their enclosing, frame-aligned program interval.
pub fn token_range(
    words: &[TranscriptTokenRef],
    frame_rate: FrameRate,
) -> Result<(Tick, Tick), TranscriptError> {
    let start = words
        .iter()
        .map(|word| word.start)
        .min()
        .ok_or(TranscriptError::InvalidRange)?;
    let end = words
        .iter()
        .map(|word| word.end)
        .max()
        .ok_or(TranscriptError::InvalidRange)?;
    snap_range_outward((start, end), frame_rate)
}

/// Cut the explicit A/V tracks and the specified caption track in one plan.
/// No implicit targets are added; GUI and MCP both use this function.
pub fn delete_transcript_range(
    project: &TimelineProject,
    sequence_id: SequenceId,
    caption_track: TrackId,
    target_tracks: &[TrackId],
    range: (Tick, Tick),
    ripple: bool,
) -> Result<Vec<TimelineCmd>, TranscriptError> {
    plan_delete_ranges(
        project,
        sequence_id,
        caption_track,
        target_tracks,
        &[range],
        ripple,
    )
}

/// Default local hesitation lexicon. Punctuation and case are ignored, while
/// the match itself is exact ("umbrella" never matches "um").
pub const DEFAULT_FILLER_WORDS: &[&str] = &["um", "uh", "erm", "er", "hmm"];

fn normalized_word(text: &str) -> String {
    text.trim_matches(|character: char| !character.is_alphanumeric())
        .to_lowercase()
}

/// Find exact normalized filler tokens; repeated hesitations produce separate
/// preview entries that can each be excluded before removal.
pub fn find_filler_words(
    projection: &TranscriptProjection,
    lexicon: &[String],
) -> Vec<TranscriptTokenRef> {
    let lexicon: HashSet<_> = lexicon
        .iter()
        .map(|word| normalized_word(word))
        .filter(|word| !word.is_empty())
        .collect();
    projection
        .tokens
        .iter()
        .filter(|word| lexicon.contains(&normalized_word(&word.text)))
        .cloned()
        .collect()
}

/// Remove only the previewed words. Validate every reference before planning;
/// merge touching/overlapping snapped ranges and apply them right to left.
pub fn remove_filler_words(
    project: &TimelineProject,
    sequence_id: SequenceId,
    caption_track: TrackId,
    target_tracks: &[TrackId],
    matches: &[TranscriptTokenRef],
    ripple: bool,
) -> Result<Vec<TimelineCmd>, TranscriptError> {
    let projection = get_transcript(project, sequence_id, caption_track)?;
    for word in matches {
        projection.resolve(word)?;
    }
    let ranges: Vec<_> = matches.iter().map(|word| (word.start, word.end)).collect();
    plan_delete_ranges(
        project,
        sequence_id,
        caption_track,
        target_tracks,
        &ranges,
        ripple,
    )
}

fn plan_delete_ranges(
    project: &TimelineProject,
    sequence_id: SequenceId,
    caption_track_id: TrackId,
    target_tracks: &[TrackId],
    ranges: &[(Tick, Tick)],
    ripple: bool,
) -> Result<Vec<TimelineCmd>, TranscriptError> {
    let sequence = sequence(project, sequence_id)?;
    validate_tracks(sequence, target_tracks)?;
    let captions = caption_track(project, sequence_id, caption_track_id)?;
    let mut cue_ranges: Vec<_> = captions
        .cues
        .iter()
        .map(|cue| (cue.start, cue.end))
        .collect();
    cue_ranges.sort_unstable();
    if cue_ranges
        .iter()
        .any(|(start, end)| *start < Tick::ZERO || end <= start)
        || cue_ranges.windows(2).any(|pair| pair[0].1 > pair[1].0)
    {
        return Err(TranscriptError::InvalidCueTiming);
    }
    let projection = get_transcript(project, sequence_id, caption_track_id)?;
    if projection.has_overlapping_words {
        return Err(TranscriptError::OverlappingWordTiming);
    }
    if projection.adjusted_word_count != 0 {
        return Err(TranscriptError::InvalidWordTiming);
    }
    let mut ranges = ranges
        .iter()
        .map(|range| snap_range_outward(*range, sequence.frame_rate))
        .collect::<Result<Vec<_>, _>>()?;
    ranges.sort_unstable();
    let mut merged: Vec<(Tick, Tick)> = Vec::new();
    for (start, end) in ranges {
        if let Some(previous) = merged.last_mut().filter(|previous| start <= previous.1) {
            previous.1 = previous.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    for (start, end) in &merged {
        if !sequence
            .tracks()
            .filter(|track| target_tracks.contains(&track.id))
            .flat_map(|track| &track.clips)
            .any(|clip| clip.start < *end && clip.end() > *start)
        {
            return Err(TranscriptError::NoMediaInRange);
        }
    }
    if merged.is_empty() {
        return Ok(Vec::new());
    }
    // The standard extract planner also shifts sync-locked siblings. Each
    // required track is cut explicitly here, so disable that second expansion
    // only in the disposable planning copy. No sync-lock setting is persisted.
    let mut working_sequence = sequence.clone();
    for track in working_sequence
        .video_tracks
        .iter_mut()
        .chain(&mut working_sequence.audio_tracks)
    {
        track.sync_lock = false;
    }
    let mut working_project = TimelineProject::new();
    working_project.insert_sequence(working_sequence);
    let mut scratch = Document::new("Transcript edit planning", 1.0, 1.0);
    scratch.timeline = Some(working_project);
    let mut targets = target_tracks.to_vec();
    targets.sort_unstable();
    targets.dedup();
    let mut commands = Vec::new();
    for range in merged.into_iter().rev() {
        let working = scratch.timeline.as_ref().ok_or(EditError::NoProject)?;
        let mut range_commands = Vec::new();
        for track in &targets {
            range_commands.extend(if ripple {
                ops::extract_edit(working, sequence_id, *track, range)?
            } else {
                ops::lift_edit(working, sequence_id, *track, range)?
            });
        }
        let captions = caption_track(working, sequence_id, caption_track_id)?;
        let caption_command = cut_caption_range(captions, range, ripple);
        for command in &range_commands {
            command.apply(&mut scratch);
        }
        if let Some(command) = caption_command {
            command.apply(&mut scratch);
        }
        commands.extend(range_commands);
    }
    // Store one caption snapshot for the complete edit. Repeated filler cuts
    // otherwise capture the entire caption suffix for every range, making the
    // undo record grow quadratically on long transcripts.
    let original = caption_track(project, sequence_id, caption_track_id)?;
    let edited = caption_track(
        scratch.timeline.as_ref().ok_or(EditError::NoProject)?,
        sequence_id,
        caption_track_id,
    )?;
    if edited.cues != original.cues {
        if let (Some(start), Some(end)) = (
            original.cues.iter().map(|cue| cue.start).min(),
            original.cues.iter().map(|cue| cue.end).max(),
        ) {
            commands.push(replace_caption_cues(
                caption_track_id,
                start,
                end,
                original.cues.clone(),
                edited.cues.clone(),
            ));
        }
    }
    Ok(commands)
}

fn replace_caption_cues(
    track: TrackId,
    start: Tick,
    end: Tick,
    replaced: Vec<CaptionCue>,
    cues: Vec<CaptionCue>,
) -> TimelineCmd {
    TimelineCmd::CaptionEdit(CaptionCmd::BulkInsertCues {
        track,
        cues,
        replace_range: Some((start, end)),
        replaced,
        created_track: None,
    })
}

fn caption_segment(cue: &CaptionCue, start: Tick, end: Tick, shift: Tick) -> Option<CaptionCue> {
    if end <= start {
        return None;
    }
    let mut segment = cue.clone();
    segment.start = start + shift;
    segment.end = end + shift;
    segment
        .words
        .retain(|word| word.start < end && word.end > start);
    for word in &mut segment.words {
        word.start = word.start.max(start) + shift;
        word.end = word.end.min(end) + shift;
    }
    (!segment.words.is_empty()).then_some(segment)
}

fn cut_caption_range(
    track: &CaptionTrack,
    range: (Tick, Tick),
    ripple: bool,
) -> Option<TimelineCmd> {
    let (start, end) = range;
    let delta = if ripple { start - end } else { Tick::ZERO };
    let affected: Vec<_> = track
        .cues
        .iter()
        .filter(|cue| cue.end > start && (ripple || cue.start < end))
        .cloned()
        .collect();
    let replace_start = affected.iter().map(|cue| cue.start).min()?;
    let replace_end = affected.iter().map(|cue| cue.end).max()?;
    let mut replacement = Vec::new();
    for cue in &affected {
        if cue.start >= end {
            let mut shifted = cue.clone();
            shifted.start = shifted.start + delta;
            shifted.end = shifted.end + delta;
            for word in &mut shifted.words {
                word.start = word.start + delta;
                word.end = word.end + delta;
            }
            replacement.push(shifted);
            continue;
        }
        let left = caption_segment(cue, cue.start, cue.end.min(start), Tick::ZERO);
        let right = caption_segment(cue, cue.start.max(end), cue.end, delta);
        match (left, right) {
            (Some(mut left), Some(mut right)) if ripple => {
                left.end = right.end;
                // A single word spanning the cut remains one trimmed word.
                if let (Some(last), Some(first)) = (left.words.last_mut(), right.words.first()) {
                    if cue
                        .words
                        .iter()
                        .any(|word| word.start < start && word.end > end)
                    {
                        last.end = first.end;
                        right.words.remove(0);
                    }
                }
                left.words.extend(right.words);
                replacement.push(left);
            }
            (Some(left), Some(mut right)) => {
                right.id = CueId::new();
                replacement.extend([left, right]);
            }
            (Some(segment), None) | (None, Some(segment)) => replacement.push(segment),
            (None, None) => {}
        }
    }
    Some(replace_caption_cues(
        track.id,
        replace_start,
        replace_end,
        affected,
        replacement,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{Command, CommandHistory};
    use crate::timeline::{clip::LinkGroupId, CaptionWord, Clip, ClipSource, Track, TrackKind};

    fn seconds(value: i64) -> Tick {
        Tick::from_seconds(value)
    }

    fn fixture() -> (Document, SequenceId, TrackId, Vec<TrackId>, ClipId) {
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Dialogue", FrameRate::FPS_30, 1920, 1080);
        let link = LinkGroupId::new();
        let mut tracks = Vec::new();
        let mut selected = ClipId::new();
        for (index, kind) in [
            TrackKind::Video,
            TrackKind::Audio,
            TrackKind::Audio,
            TrackKind::Video,
        ]
        .into_iter()
        .enumerate()
        {
            let mut track = Track::new(kind, format!("Track {index}"));
            let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, seconds(6));
            if index < 2 {
                clip.link_group = Some(link);
            }
            if index == 0 {
                selected = clip.id;
            }
            track.clips = vec![
                clip,
                Clip::new(ClipSource::Adjustment, seconds(6), seconds(4)),
            ];
            track.sync_lock = index == 2;
            tracks.push(track.id);
            sequence.tracks_for_mut(kind).push(track);
        }
        let mut captions = CaptionTrack::new("Dialogue captions");
        captions.cues = vec![
            CaptionCue::new(
                Tick::ZERO,
                seconds(5),
                vec![
                    CaptionWord::new("Hello,", Tick::ZERO, seconds(1)),
                    CaptionWord::new("Um!", seconds(1), seconds(2)),
                    CaptionWord::new("world", seconds(2), seconds(3)),
                    CaptionWord::new("uh", seconds(3), seconds(4)),
                    CaptionWord::new("again.", seconds(4), seconds(5)),
                ],
            ),
            CaptionCue::new(
                seconds(6),
                seconds(8),
                vec![
                    CaptionWord::new("Later", seconds(6), seconds(7)),
                    CaptionWord::new("words", seconds(7), seconds(8)),
                ],
            ),
        ];
        let caption_id = captions.id;
        sequence.caption_tracks.push(captions);
        let sequence_id = project.insert_sequence(sequence);
        let mut doc = Document::new("Transcript test", 1920.0, 1080.0);
        doc.timeline = Some(project);
        (doc, sequence_id, caption_id, tracks, selected)
    }

    fn apply(doc: &mut Document, commands: Vec<TimelineCmd>) -> CommandHistory {
        let mut history = CommandHistory::new(20);
        history.execute_discrete(
            Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
            doc,
        );
        history
    }

    #[test]
    fn projection_is_stable_clamps_provider_times_and_preserves_original_text() {
        let (mut doc, sequence, track, _, _) = fixture();
        let project = doc.timeline.as_mut().unwrap();
        let captions = &mut project.sequences.get_mut(&sequence).unwrap().caption_tracks[0];
        captions.cues.reverse();
        captions.cues[1].words[0].start = Tick(-2);
        let original = captions.clone();
        let view = get_transcript(project, sequence, track).unwrap();
        assert_eq!(view.tokens[0].text, "Hello,");
        assert_eq!(view.tokens[0].start, Tick::ZERO);
        assert_eq!(view.adjusted_word_count, 1);
        assert_eq!(view.active_at(seconds(1)), Some(1));
        assert_eq!(view.active_at(seconds(5)), None);
        assert_eq!(view.active_at(seconds(6)), Some(5));
        assert_eq!(project.sequences[&sequence].caption_tracks[0], original);
    }

    #[test]
    fn overlapping_words_remain_navigable_but_cannot_delete_media() {
        let (mut doc, sequence, track, targets, _) = fixture();
        let project = doc.timeline.as_mut().unwrap();
        project.sequences.get_mut(&sequence).unwrap().caption_tracks[0].cues[0].words[0].end =
            seconds(3);
        let view = get_transcript(project, sequence, track).unwrap();
        assert!(view.has_overlapping_words);
        assert_eq!(view.active_at(seconds(2)), Some(0));
        assert_eq!(
            delete_transcript_range(
                project,
                sequence,
                track,
                &targets[..2],
                (seconds(1), seconds(2)),
                true
            ),
            Err(TranscriptError::OverlappingWordTiming)
        );
    }

    #[test]
    fn range_mapping_snaps_outward_and_rejects_overflow() {
        let frame = FrameRate::FPS_30.ticks_per_frame();
        assert_eq!(
            snap_range_outward(
                (Tick(frame.0 + 1), Tick(2 * frame.0 + 1)),
                FrameRate::FPS_30
            )
            .unwrap(),
            (frame, Tick(3 * frame.0))
        );
        assert_eq!(
            snap_range_outward((frame, Tick(2 * frame.0)), FrameRate::FPS_30).unwrap(),
            (frame, Tick(2 * frame.0))
        );
        assert_eq!(
            snap_range_outward((Tick(1), Tick(i64::MAX)), FrameRate::FPS_30),
            Err(TranscriptError::InvalidRange)
        );
        assert_eq!(
            snap_range_outward((Tick(0), frame), FrameRate::new(0, 1)),
            Err(TranscriptError::InvalidFrameRate)
        );
    }

    #[test]
    fn wording_edit_changes_only_text_and_undo_restores_it() {
        let (mut doc, sequence, track, _, _) = fixture();
        let before = doc.timeline.clone();
        let project = doc.timeline.as_ref().unwrap();
        let cue = project.sequences[&sequence].caption_tracks[0].cues[0].clone();
        let commands =
            edit_transcript_word(project, sequence, track, cue.id, 0, "Welcome,".into()).unwrap();
        let mut history = apply(&mut doc, commands);
        let after = &doc.timeline.as_ref().unwrap().sequences[&sequence].caption_tracks[0].cues[0];
        let mut expected = cue;
        expected.words[0].text = "Welcome,".into();
        assert_eq!(*after, expected);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before);
    }

    #[test]
    fn linked_dialogue_and_sync_locked_tracks_cut_with_captions_in_one_undo() {
        let (mut doc, sequence, caption, targets, selected) = fixture();
        let before = doc.timeline.clone();
        let project = doc.timeline.as_ref().unwrap();
        let scope = dialogue_track_scope(project, sequence, &[selected]).unwrap();
        assert_eq!(scope.len(), 3);
        let commands = delete_transcript_range(
            project,
            sequence,
            caption,
            &scope,
            (seconds(1), seconds(2)),
            true,
        )
        .unwrap();
        let mut history = apply(&mut doc, commands);
        let result = &doc.timeline.as_ref().unwrap().sequences[&sequence];
        for target in &targets[..3] {
            let clips = &result.track(*target).unwrap().clips;
            assert_eq!(
                clips
                    .iter()
                    .map(|clip| (clip.start, clip.end()))
                    .collect::<Vec<_>>(),
                vec![
                    (seconds(0), seconds(1)),
                    (seconds(1), seconds(5)),
                    (seconds(5), seconds(9))
                ]
            );
            assert_eq!(clips[1].source_in, seconds(2));
        }
        assert_eq!(
            result.track(targets[3]).unwrap(),
            before.as_ref().unwrap().sequences[&sequence]
                .track(targets[3])
                .unwrap()
        );
        assert_eq!(
            result.caption_tracks[0].cues[0].text(),
            "Hello, world uh again."
        );
        assert_eq!(result.caption_tracks[0].cues[0].words[1].start, seconds(1));
        assert_eq!(result.caption_tracks[0].cues[1].start, seconds(5));
        assert_eq!(history.undo_depth(), 1);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before);
        assert!(history.redo(&mut doc));
        assert_eq!(
            doc.timeline.as_ref().unwrap().sequences[&sequence].caption_tracks[0].cues[1].start,
            seconds(5)
        );
    }

    #[test]
    fn locked_link_or_sync_partner_rejects_the_whole_plan() {
        for locked_index in [1, 2] {
            let (mut doc, sequence, caption, targets, selected) = fixture();
            let project = doc.timeline.as_mut().unwrap();
            project
                .sequences
                .get_mut(&sequence)
                .unwrap()
                .track_mut(targets[locked_index])
                .unwrap()
                .locked = true;
            let before = project.clone();
            assert_eq!(
                dialogue_track_scope(project, sequence, &[selected]),
                Err(TranscriptError::TrackLocked(targets[locked_index]))
            );
            assert_eq!(
                delete_transcript_range(
                    project,
                    sequence,
                    caption,
                    &targets[..3],
                    (seconds(1), seconds(2)),
                    true
                ),
                Err(TranscriptError::TrackLocked(targets[locked_index]))
            );
            assert_eq!(*project, before);
        }
    }

    #[test]
    fn lift_splits_captions_around_the_gap_without_shifting_following_words() {
        let (mut doc, sequence, caption, targets, _) = fixture();
        let before = doc.timeline.clone();
        let commands = delete_transcript_range(
            doc.timeline.as_ref().unwrap(),
            sequence,
            caption,
            &targets[..2],
            (seconds(1), seconds(2)),
            false,
        )
        .unwrap();
        let mut history = apply(&mut doc, commands);
        let result = &doc.timeline.as_ref().unwrap().sequences[&sequence];
        assert_eq!(result.caption_tracks[0].cues.len(), 3);
        assert_eq!(result.caption_tracks[0].cues[0].end, seconds(1));
        assert_eq!(result.caption_tracks[0].cues[1].start, seconds(2));
        assert_eq!(result.caption_tracks[0].cues[2].start, seconds(6));
        assert_eq!(result.track(targets[0]).unwrap().clips[1].start, seconds(2));
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before);
    }

    #[test]
    fn fillers_match_exactly_and_remove_right_to_left_with_single_undo() {
        let (mut doc, sequence, caption, targets, _) = fixture();
        let before = doc.timeline.clone();
        let project = doc.timeline.as_ref().unwrap();
        let view = get_transcript(project, sequence, caption).unwrap();
        let lexicon = vec!["UM".into(), "uh".into(), "word".into()];
        let matches = find_filler_words(&view, &lexicon);
        assert_eq!(
            matches
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>(),
            ["Um!", "uh"]
        );
        let commands =
            remove_filler_words(project, sequence, caption, &targets[..3], &matches, true).unwrap();
        let mut history = apply(&mut doc, commands);
        let result = &doc.timeline.as_ref().unwrap().sequences[&sequence];
        assert_eq!(
            result.caption_tracks[0].cues[0].text(),
            "Hello, world again."
        );
        assert_eq!(
            result.caption_tracks[0].cues[0]
                .words
                .iter()
                .map(|word| word.start)
                .collect::<Vec<_>>(),
            [seconds(0), seconds(1), seconds(2)]
        );
        assert_eq!(result.caption_tracks[0].cues[1].start, seconds(4));
        for target in &targets[..3] {
            assert_eq!(
                result.track(*target).unwrap().clips.last().unwrap().end(),
                seconds(8)
            );
        }
        assert_eq!(history.undo_depth(), 1);
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before);
    }

    #[test]
    fn stale_filler_preview_and_empty_media_refuse_without_mutation() {
        let (mut doc, sequence, caption, targets, _) = fixture();
        let project = doc.timeline.as_mut().unwrap();
        let view = get_transcript(project, sequence, caption).unwrap();
        let stale = vec![view.tokens[1].clone()];
        project.sequences.get_mut(&sequence).unwrap().caption_tracks[0].cues[0].words[1].text =
            "Changed".into();
        assert_eq!(
            remove_filler_words(project, sequence, caption, &targets[..2], &stale, true),
            Err(TranscriptError::StaleToken)
        );
        assert_eq!(
            delete_transcript_range(
                project,
                sequence,
                caption,
                &targets[..2],
                (seconds(11), seconds(12)),
                true
            ),
            Err(TranscriptError::NoMediaInRange)
        );
    }

    #[test]
    fn text_only_deletion_preserves_media_and_surviving_word_times() {
        let (mut doc, sequence, caption, _, _) = fixture();
        let before = doc.timeline.clone();
        let project = doc.timeline.as_ref().unwrap();
        let view = get_transcript(project, sequence, caption).unwrap();
        let commands =
            delete_transcript_text(project, sequence, caption, &view.tokens[1..2]).unwrap();
        let mut history = apply(&mut doc, commands);
        let result = &doc.timeline.as_ref().unwrap().sequences[&sequence];
        assert_eq!(
            result.video_tracks,
            before.as_ref().unwrap().sequences[&sequence].video_tracks
        );
        assert_eq!(
            result.audio_tracks,
            before.as_ref().unwrap().sequences[&sequence].audio_tracks
        );
        assert_eq!(result.caption_tracks[0].cues[0].words[1].start, seconds(2));
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline, before);
    }

    #[test]
    fn touching_snapped_filler_ranges_remove_each_frame_once() {
        let (mut doc, sequence, caption, tracks, _) = fixture();
        let frame = FrameRate::FPS_30.ticks_per_frame();
        let project = doc.timeline.as_mut().unwrap();
        project.sequences.get_mut(&sequence).unwrap().caption_tracks[0].cues[0].words = vec![
            CaptionWord::new("um", Tick(frame.0 / 4), Tick(frame.0 / 2)),
            CaptionWord::new("uh", Tick(3 * frame.0 / 4), frame),
            CaptionWord::new("speech", seconds(1), seconds(2)),
        ];
        let before = project.clone();
        let view = get_transcript(project, sequence, caption).unwrap();
        let plan = remove_filler_words(
            project,
            sequence,
            caption,
            &tracks[..3],
            &view.tokens[..2],
            true,
        )
        .unwrap();
        assert_eq!(
            plan.iter()
                .filter(|command| matches!(command, TimelineCmd::CaptionEdit(_)))
                .count(),
            1
        );
        let mut history = apply(&mut doc, plan);
        let result = &doc.timeline.as_ref().unwrap().sequences[&sequence];
        assert_eq!(
            result.track(tracks[0]).unwrap().clips.last().unwrap().end(),
            seconds(10) - frame
        );
        assert_eq!(result.caption_tracks[0].cues[0].words[0].text, "speech");
        assert!(history.undo(&mut doc));
        assert_eq!(doc.timeline.as_ref().unwrap(), &before);
    }

    #[test]
    fn overlapping_cue_envelopes_cannot_remove_unrelated_caption_content() {
        let (mut doc, sequence, caption, tracks, _) = fixture();
        let project = doc.timeline.as_mut().unwrap();
        project.sequences.get_mut(&sequence).unwrap().caption_tracks[0].cues[1].start = seconds(4);
        let before = project.clone();
        assert!(
            !get_transcript(project, sequence, caption)
                .unwrap()
                .has_overlapping_words
        );
        assert_eq!(
            delete_transcript_range(
                project,
                sequence,
                caption,
                &tracks[..2],
                (seconds(1), seconds(2)),
                true
            ),
            Err(TranscriptError::InvalidCueTiming)
        );
        assert_eq!(*project, before);
    }
}
