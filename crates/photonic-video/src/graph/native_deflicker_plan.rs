//! Bounded native Deflicker analysis plans in authored owner clocks.
use super::deflicker::{AnalysisGrid, NativeDeflickerKey};
use photonic_core::timeline::{
    AnimProps, ClipEffect, ClipId, ClipSource, EffectKind, EffectParams, PropPath, PropValue,
    SequenceId, Tick, TimelineProject, VfxOwner,
};
use std::collections::HashSet;

pub(crate) const MAX_ANALYSIS_TURNS: usize = 1024;
pub(crate) const MAX_ANALYSIS_PLANS: usize = 32;
#[derive(Clone, Debug)]
pub(crate) struct NativeDeflickerPlan {
    pub key: NativeDeflickerKey,
    pub sequence_start: Tick,
    pub first: Tick,
    pub duration: Tick,
    pub frame_ticks: i64,
    pub fps: f32,
    pub grid: AnalysisGrid,
    pub authored: AnimProps<EffectParams>,
    pub animated: bool,
}
impl NativeDeflickerPlan {
    pub fn burst_ticks(&self) -> Vec<Tick> {
        let anchor = self.duration.0 / self.frame_ticks / 2;
        (0..12)
            .filter_map(|k| {
                let t = (anchor as i128 + k) * self.frame_ticks as i128;
                (t < self.duration.0 as i128).then_some(Tick(t as i64))
            })
            .collect()
    }
}

pub(crate) fn collect(
    project: &TimelineProject,
    root: SequenceId,
) -> Result<Vec<NativeDeflickerPlan>, String> {
    struct Collector<'a> {
        project: &'a TimelineProject,
        plans: Vec<NativeDeflickerPlan>,
        visited: HashSet<SequenceId>,
        path: HashSet<SequenceId>,
        turns: usize,
    }
    impl Collector<'_> {
        #[allow(clippy::too_many_arguments)]
        fn stack(
            &mut self,
            seq: SequenceId,
            owner: VfxOwner,
            clip: Option<ClipId>,
            effects: &[ClipEffect],
            start: i64,
            end: i64,
            offset: i64,
            frame: i64,
            fps: f32,
        ) -> Result<(), String> {
            for (index, fx) in effects
                .iter()
                .enumerate()
                .filter(|(_, f)| f.kind == EffectKind::Deflicker && f.enabled && !f.inert)
            {
                let amount_animated = fx
                    .params
                    .track(&PropPath::new("params.amount"))
                    .is_some_and(|t| !t.orphaned && !t.keyframes.is_empty());
                if fx.params.base.get("params.amount") == Some(&PropValue::Float(0.0))
                    && !amount_animated
                {
                    continue;
                }
                let (a, z) = fx
                    .zone
                    .map_or((start, end), |(a, z)| (start.max(a.0), end.min(z.0)));
                if z <= a {
                    continue;
                }
                let duration = z.checked_sub(a).ok_or("analysis range overflow")?;
                let sequence_start = a.checked_add(offset).ok_or("analysis clock overflow")?;
                if sequence_start < 0 {
                    return Err(
                        "native analysis requires a visible nonnegative sequence span".into(),
                    );
                }
                let grid = AnalysisGrid::new(Tick(duration), frame, 240)
                    .ok_or("native analysis grid is invalid")?;
                let animated = fx.params.tracks.iter().any(|t| {
                    !t.orphaned
                        && !t.keyframes.is_empty()
                        && matches!(t.property.as_str(), "params.window" | "params.max_change")
                });
                let plan = NativeDeflickerPlan {
                    key: NativeDeflickerKey {
                        sequence: seq,
                        owner,
                        clip_context: clip,
                        effect_index: index as u32,
                    },
                    sequence_start: Tick(sequence_start),
                    first: Tick(a),
                    duration: Tick(duration),
                    frame_ticks: frame,
                    fps,
                    grid,
                    authored: fx.params.clone(),
                    animated,
                };
                self.turns = self.turns.saturating_add(
                    plan.grid.ticks.len()
                        + plan.burst_ticks().len()
                        + if animated { plan.grid.ticks.len() } else { 1 },
                );
                if self.plans.len() >= MAX_ANALYSIS_PLANS || self.turns > MAX_ANALYSIS_TURNS {
                    return Err("native Deflicker analysis exceeds32plans or1024bounded sampling/solve turns; reduce enabled analysis scopes".into());
                }
                if self.plans.iter().any(|p| p.key == plan.key) {
                    return Err("native analysis has duplicate owner/context identities".into());
                }
                self.plans.push(plan);
            }
            Ok(())
        }
        fn sequence(&mut self, id: SequenceId) -> Result<(), String> {
            if self.path.contains(&id) {
                return Err("native analysis sequence cycle".into());
            }
            if self.visited.contains(&id) {
                return Ok(());
            }
            if self.path.len() >= 16 {
                return Err("native analysis nesting exceeds16sequences".into());
            }
            self.path.insert(id);
            let seq = self
                .project
                .sequences
                .get(&id)
                .ok_or("native analysis sequence missing")?;
            if seq.frame_rate.num == 0 || seq.frame_rate.den == 0 {
                return Err("native analysis frame rate invalid".into());
            }
            let frame = ((photonic_core::timeline::TICKS_PER_SECOND as i128
                * seq.frame_rate.den as i128)
                / seq.frame_rate.num as i128)
                .clamp(1, i64::MAX as i128) as i64;
            let fps = seq.frame_rate.num as f32 / seq.frame_rate.den as f32;
            for track in seq.video_tracks.iter().filter(|t| t.enabled) {
                for clip in track.clips.iter().filter(|c| c.enabled) {
                    if let ClipSource::NestedSequence { sequence } = clip.source {
                        self.sequence(sequence)?;
                    }
                }
            }
            let mut end = frame;
            for track in seq.video_tracks.iter().filter(|t| t.enabled) {
                for clip in track.clips.iter().filter(|c| c.enabled) {
                    let clip_end = clip
                        .start
                        .0
                        .checked_add(clip.duration.0)
                        .ok_or("native analysis clip span overflow")?;
                    end = end.max(clip_end);
                    let visible_start = clip.start.0.max(0);
                    if clip_end <= visible_start {
                        continue;
                    }
                    let local_start = visible_start
                        .checked_sub(clip.start.0)
                        .ok_or("native analysis clip clock overflow")?;
                    if let ClipSource::Asset { asset } | ClipSource::Vector { asset } = clip.source
                    {
                        let media = self
                            .project
                            .media
                            .assets
                            .get(&asset)
                            .ok_or("native analysis asset missing")?;
                        self.stack(
                            id,
                            VfxOwner::Asset(asset),
                            Some(clip.id),
                            &media.effects,
                            local_start,
                            clip.duration.0,
                            clip.start.0,
                            frame,
                            fps,
                        )?;
                    }
                    self.stack(
                        id,
                        VfxOwner::Clip(clip.id),
                        Some(clip.id),
                        &clip.effects,
                        local_start,
                        clip.duration.0,
                        clip.start.0,
                        frame,
                        fps,
                    )?;
                    if !matches!(clip.source, ClipSource::Adjustment) {
                        self.stack(
                            id,
                            VfxOwner::Track(track.id),
                            Some(clip.id),
                            &track.effects,
                            visible_start,
                            clip_end,
                            0,
                            frame,
                            fps,
                        )?;
                    }
                }
            }
            self.stack(
                id,
                VfxOwner::Master(id),
                None,
                &seq.master_effects,
                0,
                end,
                0,
                frame,
                fps,
            )?;
            self.path.remove(&id);
            self.visited.insert(id);
            Ok(())
        }
    }
    let mut c = Collector {
        project,
        plans: Vec::new(),
        visited: HashSet::new(),
        path: HashSet::new(),
        turns: 0,
    };
    c.sequence(root)?;
    Ok(c.plans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        AssetKind, Clip, FrameRate, Interp, Keyframe, MediaAsset, PropertyTrack, Sequence, Track,
        TrackKind,
    };
    fn fixture() -> (TimelineProject, SequenceId) {
        let mut p = TimelineProject::new();
        let mut a = MediaAsset::from_file(AssetKind::Video, "/tmp/plan-only.mkv");
        a.effects.push(ClipEffect::new(EffectKind::Deflicker));
        let aid = p.media.insert(a);
        let mut s = Sequence::new("plan", FrameRate::FPS_30, 16, 16);
        let f = s.frame_rate.ticks_per_frame().0;
        let mut t = Track::new(TrackKind::Video, "V1");
        t.effects.push(ClipEffect::new(EffectKind::Deflicker));
        for start in [10 * f, 30 * f] {
            let mut c = Clip::new(ClipSource::Asset { asset: aid }, Tick(start), Tick(12 * f));
            c.effects.push(ClipEffect::new(EffectKind::Deflicker));
            t.clips.push(c);
        }
        s.video_tracks.push(t);
        s.master_effects
            .push(ClipEffect::new(EffectKind::Deflicker));
        let id = s.id;
        p.insert_sequence(s);
        (p, id)
    }
    #[test]
    fn native_deflicker_plans_keep_owner_clocks_and_shared_asset_contexts() {
        let (p, id) = fixture();
        let plans = collect(&p, id).unwrap();
        assert_eq!(plans.len(), 7);
        let f = p.sequences[&id].frame_rate.ticks_per_frame().0;
        assert!(matches!(plans[0].key.owner, VfxOwner::Asset(_)));
        assert!(matches!(plans[1].key.owner, VfxOwner::Clip(_)));
        assert!(matches!(plans[2].key.owner, VfxOwner::Track(_)));
        assert!(matches!(plans[6].key.owner, VfxOwner::Master(_)));
        assert_eq!(plans[0].first, Tick(0));
        assert_eq!(plans[0].sequence_start, Tick(10 * f));
        assert_eq!(plans[2].first, Tick(10 * f));
        assert_eq!(plans[3].sequence_start, Tick(30 * f));
        assert_eq!(plans[6].duration, Tick(42 * f));
        assert_ne!(plans[0].key, plans[3].key);
        assert_eq!(plans[0].key.owner, plans[3].key.owner);
        assert!(plans
            .iter()
            .all(|p| p.grid.ticks.len() <= 240 && p.burst_ticks().len() <= 12));
    }
    #[test]
    fn native_deflicker_plans_intersect_zones_and_bypass_disabled_scopes() {
        let (mut p, id) = fixture();
        let f = p.sequences[&id].frame_rate.ticks_per_frame().0;
        let aid = *p.media.assets.keys().next().unwrap();
        p.media.assets.get_mut(&aid).unwrap().effects[0].zone = Some((Tick(3 * f), Tick(9 * f)));
        let plans = collect(&p, id).unwrap();
        assert_eq!(plans[0].first, Tick(3 * f));
        assert_eq!(plans[0].sequence_start, Tick(13 * f));
        assert_eq!(plans[0].duration, Tick(6 * f));
        assert_eq!(plans[3].sequence_start, Tick(33 * f));
        p.media.assets.get_mut(&aid).unwrap().effects[0].enabled = false;
        assert_eq!(collect(&p, id).unwrap().len(), 5);
        p.sequences.get_mut(&id).unwrap().video_tracks[0].enabled = false;
        assert_eq!(collect(&p, id).unwrap().len(), 1);
        p.sequences.get_mut(&id).unwrap().master_effects[0]
            .params
            .base
            .entries = vec![(PropPath::new("params.amount"), PropValue::Float(0.0))];
        assert!(collect(&p, id).unwrap().is_empty());
    }
    #[test]
    fn native_deflicker_plans_refuse_cycles_duplicate_identities_and_excess_work() {
        let (mut p, id) = fixture();
        let first = p.sequences[&id].video_tracks[0].clips[0].clone();
        p.sequences.get_mut(&id).unwrap().video_tracks[0]
            .clips
            .push(first);
        assert!(collect(&p, id).unwrap_err().contains("duplicate"));
        let (mut p, id) = fixture();
        p.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0].source =
            ClipSource::NestedSequence { sequence: id };
        assert!(collect(&p, id).unwrap_err().contains("cycle"));
        let (mut p, id) = fixture();
        let sequence = p.sequences.get_mut(&id).unwrap();
        let f = sequence.frame_rate.ticks_per_frame().0;
        for clip in &mut sequence.video_tracks[0].clips {
            clip.duration = Tick(240 * f);
            let mut lane = PropertyTrack::new(PropPath::new("params.window"));
            lane.keyframes.push(Keyframe::new(
                Tick(0),
                PropValue::Float(4.0),
                Interp::Linear,
            ));
            clip.effects[0].params.tracks.push(lane);
        }
        assert!(collect(&p, id).unwrap_err().contains("bounded sampling"));
        let (mut p, id) = fixture();
        p.sequences.get_mut(&id).unwrap().video_tracks[0].clips[0]
            .effects
            .push(ClipEffect::new(EffectKind::Deflicker));
        let plans = collect(&p, id).unwrap();
        let cid = p.sequences[&id].video_tracks[0].clips[0].id;
        let instances: Vec<_> = plans
            .iter()
            .filter(|p| p.key.owner == VfxOwner::Clip(cid))
            .map(|p| p.key.effect_index)
            .collect();
        assert_eq!(instances, vec![0, 1]);
    }
}
