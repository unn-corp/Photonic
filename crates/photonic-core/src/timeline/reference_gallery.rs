//! Explicit immutable reference reuse across sequence-owned galleries.
use super::sequence::ReferenceStillOrigin;
use super::{ops, SequenceId, TimelineCmd, TimelineProject};
use ops::EditError;
pub const MAX_COPIED_GALLERY_SIZE: usize = 256;

/// A copy has its own gallery identity but shares the pinned immutable image.
/// No live ownership link, asset mutation, rendering or file I/O is performed.
pub fn copy_reference_still(
    project: &TimelineProject,
    source: SequenceId,
    destination: SequenceId,
    still_id: uuid::Uuid,
    name: Option<&str>,
) -> Result<TimelineCmd, EditError> {
    let invalid = |s: &str| EditError::InvalidReferenceStill(s.into());
    if source == destination {
        return Err(invalid("choose a different destination sequence"));
    }
    let from = project
        .sequences
        .get(&source)
        .ok_or_else(|| invalid("source sequence not found"))?;
    let to = project
        .sequences
        .get(&destination)
        .ok_or_else(|| invalid("destination sequence not found"))?;
    let original = from
        .reference_stills
        .iter()
        .find(|s| s.id == still_id)
        .ok_or_else(|| invalid("source still not found"))?;
    if to.reference_stills.len() >= MAX_COPIED_GALLERY_SIZE {
        return Err(invalid("destination gallery already has 256 stills"));
    }
    let source_format = from
        .formats
        .get(original.format_index)
        .ok_or_else(|| invalid("source reference format no longer exists"))?;
    let target_format = to
        .formats
        .get(to.active_format)
        .ok_or_else(|| invalid("destination active format is invalid"))?;
    if original.color != to.color
        || (source_format.width, source_format.height)
            != (target_format.width, target_format.height)
    {
        return Err(invalid(
            "reference color configuration and canvas dimensions must match destination",
        ));
    }
    // Validate original metadata before mapping its local format index.
    ops::validate_reference_still(project, source, original)?;
    let mut copied = original.clone();
    copied.id = uuid::Uuid::new_v4();
    if let Some(name) = name {
        copied.name = name.trim().into();
    }
    copied.copied_from = Some(
        original
            .copied_from
            .clone()
            .unwrap_or(ReferenceStillOrigin {
                sequence_id: source,
                still_id,
                format_index: original.format_index,
            }),
    );
    copied.format_index = to.active_format;
    ops::add_reference_still(project, destination, copied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::CommandHistory;
    use crate::timeline::{
        AssetKind, FrameRate, MediaAsset, ReferenceStill, Sequence, SequenceFormat, Tick,
    };
    use crate::{Command, Document};
    fn fixture() -> (Document, SequenceId, SequenceId, uuid::Uuid) {
        let mut d = Document::new("Gallery ownership", 16., 16.);
        let mut p = TimelineProject::new();
        let mut a = Sequence::new("Capture", FrameRate::FPS_30, 16, 16);
        let mut b = Sequence::new("Destination", FrameRate::FPS_30, 32, 16);
        b.formats.push(SequenceFormat::new("Matching", 16, 16));
        b.active_format = 1;
        let asset = p
            .media
            .insert(MediaAsset::from_file(AssetKind::Image, "reference.png"));
        let id = uuid::Uuid::new_v4();
        a.reference_stills.push(ReferenceStill {
            copied_from: None,
            id,
            name: "Hero".into(),
            image_asset: asset,
            image_hash: "0123456789abcdef0123456789abcdef".into(),
            native_balance: None,
            source_clip: None,
            source_time: Tick::ZERO,
            grade_revision: 3,
            color: a.color.clone(),
            format_index: 0,
        });
        let aid = a.id;
        let bid = b.id;
        p.insert_sequence(a);
        p.insert_sequence(b);
        d.timeline = Some(p);
        (d, aid, bid, id)
    }
    #[test]
    fn copied_reference_gallery_owns_identity_preserves_provenance_and_undo() {
        let (mut d, a, b, id) = fixture();
        let mut h = CommandHistory::new(16);
        let before = serde_json::to_value(&d).unwrap();
        let original = d.timeline.as_ref().unwrap().sequences[&a].reference_stills[0].clone();
        let cmd = copy_reference_still(
            d.timeline.as_ref().unwrap(),
            a,
            b,
            id,
            Some("  Reference copy  "),
        )
        .unwrap();
        h.execute_discrete(Command::Timeline(cmd), &mut d);
        let p = d.timeline.as_ref().unwrap();
        let copy = p.sequences[&b].reference_stills[0].clone();
        assert_eq!(p.sequences[&a].reference_stills, vec![original.clone()]);
        assert_ne!(copy.id, id);
        assert_eq!(copy.image_asset, original.image_asset);
        assert_eq!(copy.image_hash, original.image_hash);
        assert_eq!(copy.format_index, 1);
        assert_eq!(copy.name, "Reference copy");
        assert_eq!(copy.copied_from.as_ref().unwrap().sequence_id, a);
        assert_eq!(copy.copied_from.as_ref().unwrap().format_index, 0);
        let restored: Document = serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
        assert_eq!(
            serde_json::to_value(&restored).unwrap(),
            serde_json::to_value(&d).unwrap()
        );
        let remove = ops::remove_reference_still(p, a, id).unwrap();
        h.execute_discrete(Command::Timeline(remove), &mut d);
        assert!(!ops::unused_assets(d.timeline.as_ref().unwrap()).contains(&copy.image_asset));
        let cmd = copy_reference_still(d.timeline.as_ref().unwrap(), b, a, copy.id, None).unwrap();
        h.execute_discrete(Command::Timeline(cmd), &mut d);
        let recopy = &d.timeline.as_ref().unwrap().sequences[&a].reference_stills[0];
        assert_eq!(recopy.copied_from, copy.copied_from);
        assert_ne!(recopy.id, copy.id);
        h.undo(&mut d);
        h.undo(&mut d);
        h.undo(&mut d);
        assert_eq!(serde_json::to_value(&d).unwrap(), before);
    }
    #[test]
    fn copied_reference_gallery_rejects_incompatible_missing_and_capacity_atomically() {
        let (mut d, a, b, id) = fixture();
        let p = d.timeline.as_ref().unwrap();
        let before = serde_json::to_value(&d).unwrap();
        assert!(copy_reference_still(p, a, a, id, None).is_err());
        assert!(copy_reference_still(p, a, b, uuid::Uuid::new_v4(), None).is_err());
        assert!(copy_reference_still(p, a, b, id, Some(" ")).is_err());
        assert_eq!(serde_json::to_value(&d).unwrap(), before);
        let p = d.timeline.as_mut().unwrap();
        p.sequences.get_mut(&b).unwrap().active_format = 0;
        assert!(copy_reference_still(p, a, b, id, None).is_err());
        p.sequences.get_mut(&b).unwrap().active_format = 1;
        let still = p.sequences[&a].reference_stills[0].clone();
        p.sequences.get_mut(&b).unwrap().reference_stills = vec![still; 256];
        assert!(copy_reference_still(p, a, b, id, None).is_err());
    }
}
