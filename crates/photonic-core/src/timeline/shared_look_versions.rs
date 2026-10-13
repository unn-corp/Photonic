//! Explicit project-shared look versions. All writes protect every linked track.
use super::grade::{SharedLook, SharedLookVersion};
use super::{ops, SharedLookId, TimelineCmd, TimelineProject};
use ops::EditError;

pub const MAX_SHARED_LOOK_VERSIONS: usize = 64;
#[derive(Clone, Debug)]
pub enum SharedLookVersionEdit {
    Add(String),
    Activate(uuid::Uuid),
    Rename(uuid::Uuid, String),
    Remove(uuid::Uuid),
}
fn invalid(message: &str) -> EditError {
    EditError::InvalidGradeVersion(message.into())
}
fn name(value: &str) -> Result<String, EditError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 128 || value.contains('\0') {
        return Err(invalid("name must contain 1–128 characters and no NUL"));
    }
    Ok(value.into())
}
fn validate_state(look: &SharedLook) -> Result<(), EditError> {
    if look.versions.len() > MAX_SHARED_LOOK_VERSIONS {
        return Err(invalid("shared look exceeds 64 versions"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for version in &look.versions {
        let normalized = name(&version.name)?;
        if !ids.insert(version.id) || !names.insert(normalized.to_ascii_lowercase()) {
            return Err(invalid("shared look has duplicate version IDs or names"));
        }
    }
    if look.active_version.is_some_and(|id| !ids.contains(&id)) {
        return Err(invalid("active shared look version is missing"));
    }
    Ok(())
}

/// Returns a single whole-look undoable command; no mutation occurs on refusal.
/// Activation propagates through the existing shared stage, never clip grades.
pub fn edit_shared_look_version(
    project: &TimelineProject,
    id: SharedLookId,
    edit: SharedLookVersionEdit,
) -> Result<TimelineCmd, EditError> {
    let old = project
        .shared_looks
        .get(&id)
        .ok_or(EditError::NoSharedLook(id))?;
    validate_state(old)?;
    // Reuse the exact all-sequence linked-track guard and authored-grade checks.
    ops::update_shared_look(project, id, &old.name, old.grade.clone())?;
    let mut new = old.clone();
    new.sync_active_version();
    match edit {
        SharedLookVersionEdit::Add(value) => {
            let value = name(&value)?;
            if new.versions.len() >= MAX_SHARED_LOOK_VERSIONS {
                return Err(invalid("shared look supports at most 64 versions"));
            }
            if new
                .versions
                .iter()
                .any(|v| v.name.eq_ignore_ascii_case(&value))
            {
                return Err(invalid("name already exists"));
            }
            let version = uuid::Uuid::new_v4();
            new.versions.push(SharedLookVersion {
                id: version,
                name: value,
                grade: new.grade.clone(),
            });
            new.active_version = Some(version);
        }
        SharedLookVersionEdit::Activate(version) => {
            if new.active_version == Some(version) {
                return Err(invalid("version is already active"));
            }
            let selected = new
                .versions
                .iter()
                .find(|v| v.id == version)
                .ok_or_else(|| invalid("version not found"))?;
            new.grade = selected.grade.clone();
            new.active_version = Some(version);
        }
        SharedLookVersionEdit::Rename(version, value) => {
            let value = name(&value)?;
            if new
                .versions
                .iter()
                .any(|v| v.id != version && v.name.eq_ignore_ascii_case(&value))
            {
                return Err(invalid("name already exists"));
            }
            let selected = new
                .versions
                .iter_mut()
                .find(|v| v.id == version)
                .ok_or_else(|| invalid("version not found"))?;
            if selected.name == value {
                return Err(invalid("name is unchanged"));
            }
            selected.name = value;
        }
        SharedLookVersionEdit::Remove(version) => {
            let index = new
                .versions
                .iter()
                .position(|v| v.id == version)
                .ok_or_else(|| invalid("version not found"))?;
            new.versions.remove(index);
            if new.active_version == Some(version) {
                new.active_version = None;
            }
        }
    }
    validate_state(&new)?;
    ops::update_shared_look(project, id, &new.name, new.grade.clone())?;
    Ok(TimelineCmd::SetSharedLook {
        id,
        old: Some(Box::new(old.clone())),
        new: Some(Box::new(new)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::CommandHistory;
    use crate::timeline::{
        Clip, ClipLook, ClipSource, FrameRate, Grade, GradeOp, GradeOpKind, GradeOpParams,
        Sequence, Tick, Track, TrackKind,
    };
    use crate::{Command, Document};
    fn fixture() -> (Document, SharedLookId) {
        let mut doc = Document::new("shared versions", 16., 16.);
        let mut p = TimelineProject::new();
        let id = SharedLookId::new();
        p.shared_looks.insert(
            id,
            SharedLook {
                id,
                name: "Scene look".into(),
                grade: Grade::new(),
                versions: vec![],
                active_version: None,
            },
        );
        for i in 0..2 {
            let mut seq = Sequence::new(format!("Sequence {i}"), FrameRate::FPS_24, 16, 16);
            let mut t = Track::new(TrackKind::Video, "V1");
            let mut c = Clip::new(ClipSource::Adjustment, Tick::ZERO, Tick::from_seconds(1));
            c.look = Some(ClipLook::Shared(id));
            t.clips.push(c);
            seq.video_tracks.push(t);
            p.insert_sequence(seq);
        }
        doc.timeline = Some(p);
        (doc, id)
    }
    fn run(
        doc: &mut Document,
        history: &mut CommandHistory,
        id: SharedLookId,
        action: SharedLookVersionEdit,
    ) {
        let cmd = edit_shared_look_version(doc.timeline.as_ref().unwrap(), id, action).unwrap();
        history.execute_discrete(Command::Timeline(cmd), doc);
    }
    #[test]
    fn shared_look_versions_preserve_edits_switch_undo_and_legacy_serde() {
        let (mut doc, id) = fixture();
        let mut history = CommandHistory::new(32);
        let legacy =
            serde_json::to_value(&doc.timeline.as_ref().unwrap().shared_looks[&id]).unwrap();
        assert!(legacy.get("versions").is_none());
        assert!(legacy.get("active_version").is_none());
        let decoded: SharedLook = serde_json::from_value(legacy).unwrap();
        assert!(decoded.versions.is_empty());
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Add("Neutral".into()),
        );
        let neutral = doc.timeline.as_ref().unwrap().shared_looks[&id]
            .active_version
            .unwrap();
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Add("Bright".into()),
        );
        let bright = doc.timeline.as_ref().unwrap().shared_looks[&id]
            .active_version
            .unwrap();
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        ));
        let cmd = ops::update_shared_look(
            doc.timeline.as_ref().unwrap(),
            id,
            "Scene look",
            grade.clone(),
        )
        .unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        let look = &doc.timeline.as_ref().unwrap().shared_looks[&id];
        assert_eq!(
            look.versions.iter().find(|v| v.id == bright).unwrap().grade,
            grade
        );
        let before = serde_json::to_value(&doc).unwrap();
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Activate(neutral),
        );
        assert_eq!(
            doc.timeline.as_ref().unwrap().shared_looks[&id].grade,
            Grade::new()
        );
        assert!(doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .values()
            .all(
                |s| s.video_tracks[0].clips[0].look == Some(ClipLook::Shared(id))
                    && s.video_tracks[0].clips[0].grade.is_none()
            ));
        history.undo(&mut doc);
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        let loaded: Document = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(loaded).unwrap(), before);
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Remove(bright),
        );
        let look = &doc.timeline.as_ref().unwrap().shared_looks[&id];
        assert_eq!(look.grade, grade);
        assert_eq!(look.active_version, None);
    }
    #[test]
    fn shared_look_versions_refuse_all_linked_locks_duplicates_and_capacity() {
        let (mut doc, id) = fixture();
        let mut history = CommandHistory::new(8);
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Add("A".into()),
        );
        let active = doc.timeline.as_ref().unwrap().shared_looks[&id]
            .active_version
            .unwrap();
        for action in [
            SharedLookVersionEdit::Add("a".into()),
            SharedLookVersionEdit::Add(" ".into()),
            SharedLookVersionEdit::Activate(active),
            SharedLookVersionEdit::Remove(uuid::Uuid::new_v4()),
            SharedLookVersionEdit::Rename(active, "A".into()),
        ] {
            let before = serde_json::to_value(&doc).unwrap();
            assert!(edit_shared_look_version(doc.timeline.as_ref().unwrap(), id, action).is_err());
            assert_eq!(serde_json::to_value(&doc).unwrap(), before);
        }
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .values_mut()
            .last()
            .unwrap()
            .video_tracks[0]
            .locked = true;
        for action in [
            SharedLookVersionEdit::Add("B".into()),
            SharedLookVersionEdit::Rename(active, "B".into()),
            SharedLookVersionEdit::Remove(active),
        ] {
            assert_eq!(
                edit_shared_look_version(doc.timeline.as_ref().unwrap(), id, action),
                Err(EditError::TrackLocked)
            );
        }
        for seq in doc.timeline.as_mut().unwrap().sequences.values_mut() {
            seq.video_tracks[0].locked = false;
        }
        for i in 1..MAX_SHARED_LOOK_VERSIONS {
            run(
                &mut doc,
                &mut history,
                id,
                SharedLookVersionEdit::Add(format!("V{i}")),
            );
        }
        assert!(edit_shared_look_version(
            doc.timeline.as_ref().unwrap(),
            id,
            SharedLookVersionEdit::Add("overflow".into())
        )
        .is_err());
    }
    #[test]
    fn shared_look_versions_synchronize_keyframe_edits_and_undo() {
        use crate::timeline::{
            AnimTarget, GradeInspectionStage, Interp, Keyframe, PropPath, PropValue,
        };
        let (mut doc, id) = fixture();
        let mut history = CommandHistory::new(32);
        let mut grade = Grade::new();
        let op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 0.0 },
        );
        let oid = op.id;
        grade.ops.push(op);
        let cmd = ops::update_shared_look(doc.timeline.as_ref().unwrap(), id, "Scene look", grade)
            .unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Add("Animated".into()),
        );
        let cid = doc
            .timeline
            .as_ref()
            .unwrap()
            .sequences
            .values()
            .next()
            .unwrap()
            .video_tracks[0]
            .clips[0]
            .id;
        let target = AnimTarget::GradeStageOp {
            clip: cid,
            stage: GradeInspectionStage::SharedLook { id },
            op: oid,
        };
        let path = PropPath::new("params.stops");
        let before = serde_json::to_value(&doc).unwrap();
        let cmd = ops::set_keyframe_checked(
            doc.timeline.as_ref().unwrap(),
            target.clone(),
            path.clone(),
            Keyframe::new(Tick::ZERO, PropValue::Float(1.0), Interp::Linear),
        )
        .unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        let look = &doc.timeline.as_ref().unwrap().shared_looks[&id];
        assert_eq!(look.grade, look.versions[0].grade);
        let cmd = ops::set_keyframe_interp(
            doc.timeline.as_ref().unwrap(),
            target.clone(),
            path.clone(),
            Tick::ZERO,
            Interp::Hold,
        )
        .unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        let look = &doc.timeline.as_ref().unwrap().shared_looks[&id];
        assert_eq!(look.grade, look.versions[0].grade);
        let cmd =
            ops::remove_keyframe(doc.timeline.as_ref().unwrap(), target, path, Tick::ZERO).unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        let look = &doc.timeline.as_ref().unwrap().shared_looks[&id];
        assert_eq!(look.grade, look.versions[0].grade);
        for _ in 0..3 {
            history.undo(&mut doc);
        }
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }
    #[test]
    fn shared_look_versions_keep_inactive_lut_dependencies_and_detach_current_only() {
        use crate::timeline::{AssetKind, MediaAsset};
        let (mut doc, id) = fixture();
        let mut history = CommandHistory::new(32);
        let lut = doc
            .timeline
            .as_mut()
            .unwrap()
            .media
            .insert(MediaAsset::from_file(
                AssetKind::Lut3d,
                "/tmp/shared-version.cube",
            ));
        let mut grade = Grade::new();
        grade.ops.push(GradeOp::new(
            GradeOpKind::Lut3d,
            GradeOpParams::Lut3d {
                asset: lut,
                intensity: 1.0,
                interp: crate::timeline::grade::LutInterp::Trilinear,
            },
        ));
        let cmd = ops::update_shared_look(doc.timeline.as_ref().unwrap(), id, "Scene look", grade)
            .unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Add("LUT alternative".into()),
        );
        run(
            &mut doc,
            &mut history,
            id,
            SharedLookVersionEdit::Add("Neutral".into()),
        );
        let cmd = ops::update_shared_look(
            doc.timeline.as_ref().unwrap(),
            id,
            "Scene look",
            Grade::new(),
        )
        .unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        assert!(!ops::unused_assets(doc.timeline.as_ref().unwrap()).contains(&lut));
        let p = doc.timeline.as_mut().unwrap();
        let seq = p.sequences.values_mut().next().unwrap();
        seq.video_tracks[0].locked = true;
        assert_eq!(
            ops::set_asset_lut_hash(p, lut, Some("changed".into())),
            Err(EditError::TrackLocked)
        );
        let seq = p.sequences.values_mut().next().unwrap();
        seq.video_tracks[0].locked = false;
        let (sid, tid, cid) = (
            seq.id,
            seq.video_tracks[0].id,
            seq.video_tracks[0].clips[0].id,
        );
        let cmd = ops::make_shared_look_independent(p, sid, tid, cid).unwrap();
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        let p = doc.timeline.as_ref().unwrap();
        assert_eq!(p.shared_looks[&id].versions.len(), 2);
        assert!(
            matches!(&p.sequences[&sid].video_tracks[0].clips[0].look,Some(ClipLook::Local(g)) if **g==Grade::new())
        );
        assert!(!ops::unused_assets(p).contains(&lut));
    }
}
