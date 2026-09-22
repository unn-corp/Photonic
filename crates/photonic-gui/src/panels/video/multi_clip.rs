//! Common-value editing for a timeline selection. Blank mixed fields stay untouched.
use crate::panels::{PanelAction, PropPanelCtx};
use photonic_core::timeline::{
    ops, Clip, ClipId, ClipTransform, SequenceId, TimelineCmd, TimelineProject, TrackId,
};

const LABELS: [&str; 6] = ["X", "Y", "Scale X", "Scale Y", "Rotation (rad)", "Opacity"];
fn values(transform: ClipTransform) -> [f64; 6] {
    [
        transform.x,
        transform.y,
        transform.scale_x,
        transform.scale_y,
        transform.rotation,
        transform.opacity,
    ]
}

#[derive(Clone, Default)]
struct Draft {
    source: Vec<(ClipId, ClipTransform, bool)>,
    fields: [String; 6],
    enabled: Option<bool>,
}

fn build_commands(
    project: &TimelineProject,
    targets: &[(SequenceId, TrackId, &Clip)],
    fields: &[Option<f64>; 6],
    enabled: Option<bool>,
) -> Result<Vec<TimelineCmd>, String> {
    if fields.iter().flatten().any(|value| !value.is_finite()) {
        return Err("Enter finite numeric values.".into());
    }
    if fields[2].is_some_and(|value| !(0.0..=1000.0).contains(&value))
        || fields[3].is_some_and(|value| !(0.0..=1000.0).contains(&value))
        || fields[5].is_some_and(|value| !(0.0..=1.0).contains(&value))
    {
        return Err("Scale must be 0–1000 and opacity must be 0–1.".into());
    }
    let mut commands = Vec::new();
    for &(seq, track, clip) in targets {
        let mut new = clip.clone();
        let t = &mut new.transform.base;
        for (field, value) in [
            &mut t.x,
            &mut t.y,
            &mut t.scale_x,
            &mut t.scale_y,
            &mut t.rotation,
            &mut t.opacity,
        ]
        .into_iter()
        .zip(fields)
        {
            if let Some(value) = value {
                *field = *value;
            }
        }
        if let Some(enabled) = enabled {
            new.enabled = enabled;
        }
        if new != *clip {
            commands.push(
                ops::set_clip_prop(project, seq, track, new).map_err(|error| error.to_string())?,
            );
        }
    }
    Ok(commands)
}

pub(crate) fn draw(ui: &mut egui::Ui, ctx: &mut PropPanelCtx) {
    let Some(project) = ctx.doc.timeline.as_ref() else {
        return;
    };
    let Some(seq) = project
        .active_sequence
        .and_then(|id| project.sequences.get(&id))
    else {
        return;
    };
    let mut targets = Vec::new();
    for id in ctx.video.selection {
        if targets
            .iter()
            .any(|(_, _, clip): &(SequenceId, TrackId, &Clip)| clip.id == *id)
        {
            continue;
        }
        let Some((track, clip)) = seq.tracks().find_map(|track| {
            track
                .clips
                .iter()
                .find(|clip| clip.id == *id)
                .map(|clip| (track, clip))
        }) else {
            ui.label("The selection changed. Select the clips again to edit together.");
            return;
        };
        targets.push((seq.id, track.id, clip));
    }
    if targets.is_empty() {
        return;
    }
    ui.label(egui::RichText::new(format!("{} clips selected", targets.len())).strong());
    ui.weak("Mixed fields stay unchanged until you enter a value. Apply changes to edit every selected clip in one undo step.");
    let source: Vec<_> = targets
        .iter()
        .map(|(_, _, clip)| (clip.id, clip.transform.base, clip.enabled))
        .collect();
    let common: [Option<f64>; 6] = std::array::from_fn(|index| {
        let first = values(source[0].1)[index];
        source
            .iter()
            .all(|(_, transform, _)| values(*transform)[index] == first)
            .then_some(first)
    });
    let common_enabled = source
        .iter()
        .all(|(_, _, enabled)| *enabled == source[0].2)
        .then_some(source[0].2);
    let id = ui.id().with(("multi_clip_draft", seq.id));
    let mut draft = ui
        .data(|data| data.get_temp::<Draft>(id))
        .unwrap_or_default();
    if draft.source != source {
        draft = Draft {
            source,
            fields: common.map(|value| value.map(|v| v.to_string()).unwrap_or_default()),
            enabled: common_enabled,
        };
    }
    let locked = targets
        .iter()
        .any(|(_, track, _)| seq.track(*track).is_some_and(|track| track.locked));
    if locked {
        ui.label("Unlock all selected tracks to edit this selection.");
    }
    ui.add_enabled_ui(!locked, |ui| {
        egui::Grid::new("multi_clip_properties").num_columns(2).show(ui, |ui| {
            for (index, label) in LABELS.iter().enumerate() {
                ui.label(*label);
                ui.add(egui::TextEdit::singleline(&mut draft.fields[index]).desired_width(110.0).hint_text("Mixed"));
                ui.end_row();
            }
            ui.label("Enabled");
            egui::ComboBox::from_id_salt("multi_clip_enabled")
                .selected_text(match draft.enabled {Some(true)=>"Enabled",Some(false)=>"Disabled",None=>"Mixed / unchanged"})
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut draft.enabled, None, "Unchanged");
                    ui.selectable_value(&mut draft.enabled, Some(true), "Enabled");
                    ui.selectable_value(&mut draft.enabled, Some(false), "Disabled");
                });
            ui.end_row();
        });
        if targets.iter().any(|(_, _, clip)| !clip.transform.tracks.is_empty() || !clip.reframe.is_empty()) {
            ui.weak("These are base transforms. Existing keyframes and per-format reframing remain in effect.");
        }
        let mut fields = [None; 6];
        let mut error = None;
        for (index, field) in draft.fields.iter().enumerate() {
            if field.trim().is_empty() { continue; }
            match field.trim().parse::<f64>() {
                Ok(value) if value.is_finite() => { if Some(value) != common[index] {fields[index] = Some(value);} }
                _ => { error = Some(format!("{} requires a finite number.", LABELS[index])); }
            }
        }
        let enabled = if draft.enabled == common_enabled {None} else {draft.enabled};
        let commands = if let Some(error) = error {Err(error)} else {build_commands(project, &targets, &fields, enabled)};
        if let Err(error) = &commands {ui.colored_label(ui.visuals().error_fg_color, error);}
        if ui.add_enabled(commands.as_ref().is_ok_and(|commands| !commands.is_empty()), egui::Button::new("Apply to selected clips")).clicked() {
            if let Ok(commands) = commands {ctx.action = Some(PanelAction::ClipEditBatch(commands));}
        }
    });
    ui.data_mut(|data| data.insert_temp(id, draft));
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::{
        history::{Command, CommandHistory},
        timeline::{ClipSource, FrameRate, Sequence, Tick, Track, TrackKind},
        Document,
    };
    fn fixture() -> (Document, SequenceId) {
        let mut project = TimelineProject::new();
        let mut seq = Sequence::new("s", FrameRate::new(30, 1), 1920, 1080);
        for (index, x) in [10.0, 20.0].into_iter().enumerate() {
            let mut track = Track::new(TrackKind::Video, format!("V{index}"));
            let mut clip = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(5));
            clip.transform.base.x = x;
            track.clips.push(clip);
            seq.video_tracks.push(track);
        }
        let id = project.insert_sequence(seq);
        project.active_sequence = Some(id);
        let mut doc = Document::new("test", 1920.0, 1080.0);
        doc.timeline = Some(project);
        (doc, id)
    }
    #[test]
    fn bulk_edit_changes_only_requested_fields_and_undoes_as_one_step() {
        let (mut doc, id) = fixture();
        let project = doc.timeline.as_ref().unwrap();
        let seq = &project.sequences[&id];
        let targets: Vec<_> = seq
            .tracks()
            .map(|track| (id, track.id, &track.clips[0]))
            .collect();
        let commands = build_commands(
            project,
            &targets,
            &[None, None, None, None, None, Some(0.4)],
            Some(false),
        )
        .unwrap();
        let mut history = CommandHistory::new(64);
        history.execute_discrete(
            Command::Batch(commands.into_iter().map(Command::Timeline).collect()),
            &mut doc,
        );
        for (index, track) in doc.timeline.as_ref().unwrap().sequences[&id]
            .tracks()
            .enumerate()
        {
            assert_eq!(track.clips[0].transform.base.x, [10.0, 20.0][index]);
            assert_eq!(track.clips[0].transform.base.opacity, 0.4);
            assert!(!track.clips[0].enabled);
        }
        history.undo(&mut doc);
        assert!(doc.timeline.as_ref().unwrap().sequences[&id]
            .tracks()
            .all(|track| track.clips[0].enabled && track.clips[0].transform.base.opacity == 1.0));
    }
    #[test]
    fn bulk_edit_rejects_locked_target_and_invalid_numbers_without_partial_commands() {
        let (mut doc, id) = fixture();
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&id)
            .unwrap()
            .video_tracks[1]
            .locked = true;
        let project = doc.timeline.as_ref().unwrap();
        let targets: Vec<_> = project.sequences[&id]
            .tracks()
            .map(|track| (id, track.id, &track.clips[0]))
            .collect();
        assert!(build_commands(
            project,
            &targets,
            &[Some(40.0), None, None, None, None, None],
            None
        )
        .is_err());
        assert!(build_commands(
            project,
            &targets,
            &[None, None, None, None, None, Some(2.0)],
            None
        )
        .is_err());
        assert!(build_commands(
            project,
            &targets,
            &[Some(f64::NAN), None, None, None, None, None],
            None
        )
        .is_err());
    }
}
