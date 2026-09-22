//! Create camera groups and switch angles using the shared timeline commands.
use crate::panels::{PanelAction, PropPanelCtx};
use egui::Ui;
use photonic_core::timeline::{ops, ClipId, TimelineCmd, TimelineProject, TrackId};

/// Validate the whole selection before folding clips into the first angle.
fn create_group(
    project: &TimelineProject,
    selection: &[ClipId],
) -> Result<Vec<TimelineCmd>, String> {
    let sequence = project
        .active_sequence
        .and_then(|id| project.sequences.get(&id))
        .ok_or("Create a sequence first.")?;
    let mut targets: Vec<(TrackId, ClipId)> = Vec::new();
    for id in selection {
        if targets.iter().any(|(_, clip)| clip == id) {
            continue;
        }
        let (track, clip) = sequence
            .video_tracks
            .iter()
            .find_map(|track| {
                track
                    .clips
                    .iter()
                    .find(|clip| clip.id == *id)
                    .map(|clip| (track, clip))
            })
            .ok_or("Select video clips from the active sequence.")?;
        if track.locked {
            return Err("Unlock all selected tracks first.".into());
        }
        if clip.multicam.is_some() {
            return Err("Select individual clips to create a new camera group.".into());
        }
        if !matches!(track.kind, photonic_core::timeline::TrackKind::Video) {
            return Err("Camera angles must be on video tracks.".into());
        }
        targets.push((track.id, clip.id));
    }
    if targets.len() < 2 {
        return Err("Select at least two video clips in the timeline.".into());
    }
    ops::create_multicam_group(
        project,
        sequence.id,
        targets[0].0,
        targets[0].1,
        &targets[1..],
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn draw_multicam(ui: &mut Ui, ctx: &mut PropPanelCtx) {
    crate::theme::section_header(ui, "CAMERA ANGLES");
    let Some(project) = ctx.doc.timeline.as_ref() else {
        ui.label("Create a video project and add camera clips to get started.");
        return;
    };
    let Some(sequence) = project
        .active_sequence
        .and_then(|id| project.sequences.get(&id))
    else {
        ui.label("Create a sequence, then select its camera clips.");
        return;
    };
    let selected = ctx.video.selection.iter().find_map(|id| {
        sequence.video_tracks.iter().find_map(|track| {
            track
                .clips
                .iter()
                .find(|clip| clip.id == *id && clip.multicam.is_some())
                .map(|clip| (track, clip))
        })
    });
    if let Some((track, clip)) = selected {
        ui.label(egui::RichText::new(&clip.name).strong());
        ui.weak(
            "Choose the camera used for this entire clip. Split the clip to cut between angles.",
        );
        if track.locked {
            ui.label("Unlock this track to change camera angles.");
        }
        if let Some(group) = &clip.multicam {
            for (index, angle) in group.angles.iter().enumerate() {
                let label = format!("{} · {}", index + 1, angle.name);
                if ui
                    .add_enabled(
                        !track.locked,
                        egui::SelectableLabel::new(group.active == index, label),
                    )
                    .clicked()
                    && group.active != index
                {
                    match ops::set_multicam_active_angle(
                        project,
                        sequence.id,
                        track.id,
                        clip.id,
                        index,
                    ) {
                        Ok(command) => ctx.action = Some(PanelAction::ClipEditDiscrete(command)),
                        Err(error) => {
                            ui.colored_label(ui.visuals().error_fg_color, error.to_string());
                        }
                    }
                }
            }
        }
    } else {
        ui.label("Select two or more video clips to create a camera group.");
        ui.weak("The first selected clip keeps its timing and effects. Other selected clips become its alternate angles and are removed from their tracks. Each angle starts at its current source in point; align these before grouping.");
        let commands = create_group(project, ctx.video.selection);
        let hint = commands
            .as_ref()
            .err()
            .map(String::as_str)
            .unwrap_or("Create the camera group as one undo step");
        if ui
            .add_enabled(commands.is_ok(), egui::Button::new("Create camera group"))
            .on_hover_text(hint)
            .on_disabled_hover_text(hint)
            .clicked()
        {
            if let Ok(commands) = commands {
                ctx.action = Some(PanelAction::ClipEditBatch(commands));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::{
        history::{Command, CommandHistory},
        timeline::{Clip, ClipSource, FrameRate, Sequence, Tick, Track, TrackKind},
        Document,
    };
    #[test]
    fn group_creation_is_atomic_and_rejects_locked_alternate_tracks() {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("Cameras", FrameRate::new(30, 1), 1920, 1080);
        let mut ids = Vec::new();
        for index in 0..2 {
            let mut track = Track::new(TrackKind::Video, format!("Camera {index}"));
            let clip = Clip::new(
                ClipSource::SolidColor {
                    color: photonic_core::Color::new(index as f32, 0.0, 0.0, 1.0),
                },
                Tick::ZERO,
                Tick::from_seconds(5),
            );
            ids.push(clip.id);
            track.clips.push(clip);
            seq.video_tracks.push(track);
        }
        let id = project.insert_sequence(seq);
        project.active_sequence = Some(id);
        project.sequences.get_mut(&id).unwrap().video_tracks[1].locked = true;
        assert!(create_group(&project, &ids).is_err());
        project.sequences.get_mut(&id).unwrap().video_tracks[1].locked = false;
        let commands = create_group(&project, &ids).unwrap();
        let mut doc = Document::new("test", 1920.0, 1080.0);
        doc.timeline = Some(project);
        let mut history = CommandHistory::new(64);
        history.execute_discrete(
            Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
            &mut doc,
        );
        let seq = &doc.timeline.as_ref().unwrap().sequences[&id];
        assert_eq!(
            seq.video_tracks[0].clips[0]
                .multicam
                .as_ref()
                .unwrap()
                .angles
                .len(),
            2
        );
        assert!(seq.video_tracks[1].clips.is_empty());
        history.undo(&mut doc);
        let seq = &doc.timeline.as_ref().unwrap().sequences[&id];
        assert!(seq.video_tracks[0].clips[0].multicam.is_none());
        assert_eq!(seq.video_tracks[1].clips[0].id, ids[1]);
    }
}
