//! Snapshot-isolated, bounded translation tracking and explicit undoable apply.
use crate::graph::{eval::read_texture_rgba16f, ir::FrameColorEncoding, ScopeTapPoint};
use crate::{EngineCmd, GpuContext, PreviewQuality, ProxyMode, VideoEngine};
use photonic_core::timeline::{
    self, ClipId, GradeMask, GradeOpId, Interp, Keyframe, PropPath, PropValue, SequenceId, Tick,
    TimelineCmd, TrackId, WindowShape,
};
use photonic_core::{CommandHistory, Document};
use photonic_render::window_tracking::{
    BidirectionalTranslationTracker, TrackingConfig, TrackingMethod, TrackingPlane, TrackingStop,
    TranslationTracker, MAX_INPUT_PIXELS,
};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant, UNIX_EPOCH};

pub const MAX_TRACKING_FRAMES: usize = 121;
#[derive(Clone, Debug)]
pub struct WindowTrackingRequest {
    pub clip: ClipId,
    pub op: GradeOpId,
    pub start: Tick,
    pub end: Tick,
    pub step_frames: u32,
    pub format_index: Option<usize>,
    pub config: TrackingConfig,
    pub method: TrackingMethod,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowTrackingSample {
    pub sequence_tick: Tick,
    pub local_tick: Tick,
    pub center: [f64; 2],
    pub score: f64,
    pub margin: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roundtrip_error_pixels: Option<f64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WindowTrackingProposal {
    #[serde(default)]
    pub method: TrackingMethod,
    pub document_id: uuid::Uuid,
    pub document_fingerprint: String,
    pub source_fingerprint: String,
    pub expected_revision: u64,
    pub sequence: SequenceId,
    pub track: TrackId,
    pub clip: ClipId,
    pub op: GradeOpId,
    pub requested_start: Tick,
    pub requested_end: Tick,
    pub analysis_size: [usize; 2],
    pub frame_size: [u32; 2],
    pub samples: Vec<WindowTrackingSample>,
    pub completed: bool,
    pub stop_reason: Option<String>,
}

pub fn document_fingerprint(doc: &Document) -> Result<String, String> {
    let value = serde_json::to_value(doc).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("tracking document snapshot exceeds 64 MiB".into());
    }
    Ok(format!("{:032x}", xxhash_rust::xxh3::xxh3_128(&bytes)))
}
fn source_fingerprint(doc: &Document) -> String {
    let mut entries = Vec::new();
    if let Some(project) = doc.timeline.as_ref() {
        for (id, asset) in &project.media.assets {
            if let timeline::AssetSource::File { path, rel_path } = &asset.source {
                for p in std::iter::once(path).chain(rel_path.iter()) {
                    let stamp = std::fs::metadata(p).ok().map(|m| {
                        (
                            m.len(),
                            m.modified()
                                .ok()
                                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                .map(|t| t.as_nanos().to_string()),
                        )
                    });
                    entries.push((id.to_string(), p.to_string_lossy().to_string(), stamp));
                }
            }
        }
    }
    entries.sort();
    format!(
        "{:032x}",
        xxhash_rust::xxh3::xxh3_128(&serde_json::to_vec(&entries).unwrap())
    )
}
struct Plan {
    sequence: SequenceId,
    track: TrackId,
    clip_start: Tick,
    times: Vec<Tick>,
    center: [f64; 2],
    size: [f64; 2],
    format: usize,
}
fn plan(doc: &Document, request: &WindowTrackingRequest) -> Result<Plan, String> {
    request.config.validate().map_err(str::to_owned)?;
    if !(1..=60).contains(&request.step_frames) {
        return Err("step_frames must be in 1..=60".into());
    }
    let project = doc.timeline.as_ref().ok_or("no timeline")?;
    let mut found = None;
    for (sid, seq) in &project.sequences {
        for track in &seq.video_tracks {
            for clip in &track.clips {
                if clip.id == request.clip {
                    found = Some((*sid, seq, track, clip));
                }
            }
        }
    }
    let (sid, seq, track, clip) = found.ok_or("tracking clip not found")?;
    if !clip.enabled
        || !track.enabled
        || clip.composition.is_some()
        || matches!(
            clip.source,
            timeline::ClipSource::NestedSequence { .. } | timeline::ClipSource::Adjustment
        )
    {
        return Err(
            "tracking requires an enabled ordinary clip without a source composition".into(),
        );
    }
    let grade = clip
        .grade
        .as_ref()
        .filter(|g| !g.bypass && g.graph.is_none())
        .ok_or("tracking requires an enabled ordered clip grade")?;
    let op = grade
        .ops
        .iter()
        .find(|op| op.id == request.op && op.enabled)
        .ok_or("tracking corrector not found or disabled")?;
    for path in ["mask.center[0]", "mask.center[1]"] {
        let lanes: Vec<_> = op
            .params
            .tracks
            .iter()
            .filter(|t| t.property.as_str() == path)
            .collect();
        if lanes.len() > 1 || lanes.iter().any(|l| l.orphaned) {
            return Err("tracking refuses duplicate or orphaned center lanes".into());
        }
    }
    let start = seq
        .frame_rate
        .frame_start(seq.frame_rate.frame_at(request.start));
    let end = seq
        .frame_rate
        .frame_start(seq.frame_rate.frame_at(request.end));
    let clip_end = clip
        .start
        .0
        .checked_add(clip.duration.0)
        .ok_or("clip time overflow")?;
    if start == end
        || [start, end]
            .iter()
            .any(|t| t.0 < clip.start.0 || t.0 >= clip_end || t.0 < 0)
    {
        return Err("tracking endpoints must be distinct frames inside the same clip".into());
    }
    let mask = op
        .mask_at(Tick(start.0 - clip.start.0))
        .map_err(str::to_owned)?;
    let Some(GradeMask::PowerWindow {
        shape: WindowShape::Ellipse | WindowShape::Rectangle,
        center,
        size,
        ..
    }) = mask
    else {
        return Err("tracking supports ellipse/rectangle center translation only".into());
    };
    let fi = request.format_index.unwrap_or(seq.active_format);
    let format = seq
        .formats
        .get(fi)
        .ok_or("tracking format index out of range")?;
    if u64::from(format.width) * u64::from(format.height) > MAX_INPUT_PIXELS as u64 {
        return Err("tracking frame exceeds the 16,777,216-pixel readback bound".into());
    }
    let delta = seq
        .frame_rate
        .ticks_per_frame()
        .0
        .checked_mul(i64::from(request.step_frames))
        .ok_or("tracking stride overflow")?;
    if delta <= 0 {
        return Err("invalid tracking frame rate".into());
    }
    let direction = if end > start { 1i64 } else { -1 };
    let mut times = vec![start];
    while *times.last().unwrap() != end {
        if times.len() >= MAX_TRACKING_FRAMES {
            return Err("tracking range exceeds 121 sampled frames".into());
        }
        let candidate = times
            .last()
            .unwrap()
            .0
            .checked_add(direction * delta)
            .ok_or("tracking time overflow")?;
        times.push(Tick(if direction > 0 {
            candidate.min(end.0)
        } else {
            candidate.max(end.0)
        }));
    }
    Ok(Plan {
        sequence: sid,
        track: track.id,
        clip_start: clip.start,
        times,
        center: center.map(f64::from),
        size: size.map(f64::from),
        format: fi,
    })
}
/// Validate before admitting an asynchronous analysis job. No document edit.
pub fn validate_request(doc: &Document, request: &WindowTrackingRequest) -> Result<(), String> {
    plan(doc, request).map(|_| ())
}

pub fn analyze_window(
    mut doc: Document,
    revision: u64,
    request: WindowTrackingRequest,
    gpu: GpuContext,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<WindowTrackingProposal, String> {
    let plan = plan(&doc, &request)?;
    let fingerprint = document_fingerprint(&doc)?;
    let sources = source_fingerprint(&doc);
    let mut result = WindowTrackingProposal {
        method: request.method,
        document_id: doc.id,
        document_fingerprint: fingerprint,
        source_fingerprint: sources.clone(),
        expected_revision: revision,
        sequence: plan.sequence,
        track: plan.track,
        clip: request.clip,
        op: request.op,
        requested_start: plan.times[0],
        requested_end: *plan.times.last().unwrap(),
        analysis_size: [0, 0],
        frame_size: [0, 0],
        samples: vec![],
        completed: false,
        stop_reason: None,
    };
    let project = doc.timeline.as_mut().unwrap();
    project.active_sequence = Some(plan.sequence);
    project
        .sequences
        .get_mut(&plan.sequence)
        .unwrap()
        .active_format = plan.format;
    let owned = Arc::new(Mutex::new(doc));
    let session = VideoEngine::new(gpu.clone())
        .open_session(owned.clone(), Arc::new(Mutex::new(CommandHistory::new(1))));
    let deadline = Instant::now() + Duration::from_secs(120);
    let work = (|| {
        let mut tracker: Option<TranslationTracker> = None;
        let mut adaptive: Option<BidirectionalTranslationTracker> = None;
        for (index, &tick) in plan.times.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("tracking cancelled".into());
            }
            if Instant::now() >= deadline {
                return Err("tracking exceeded its 120-second analysis bound".into());
            }
            let request_id = index as u64 + 1;
            if !session.send(EngineCmd::InspectFrame {
                request_id,
                sequence: plan.sequence,
                time: tick,
                proxy_mode: ProxyMode::ForceOriginal,
                quality: PreviewQuality::Full,
                scope_tap: ScopeTapPoint::ClipPreGrade(request.clip),
            }) {
                return Err("tracking engine queue unavailable".into());
            }
            let wait_until = (Instant::now() + Duration::from_secs(30)).min(deadline);
            let frame = loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("tracking cancelled".into());
                }
                if Instant::now() >= wait_until {
                    return Err("tracking input frame timed out".into());
                }
                if let Some(f) = session.latest_frame() {
                    if f.inspection_request_id == Some(request_id)
                        && f.time == tick
                        && f.sequence == plan.sequence
                    {
                        break f;
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            if frame.scope_tap_point != ScopeTapPoint::ClipPreGrade(request.clip)
                || !frame.color_errors.is_empty()
                || !frame.grading_errors.is_empty()
                || frame.cached_preview
                || frame.preview_asset.is_some()
                || frame.preview_quality != PreviewQuality::Full
                || frame.proxy_mode != ProxyMode::ForceOriginal
                || !matches!(
                    frame.scope_tap_encoding,
                    Some(
                        FrameColorEncoding::SceneLinearAcescg
                            | FrameColorEncoding::LegacyLinearRec709
                    )
                )
            {
                return Err("requested pre-grade tracking input is inactive or unqualified".into());
            }
            let tap = frame
                .scope_tap
                .as_ref()
                .ok_or("tracking input has no pixels")?;
            if u64::from(tap.width) * u64::from(tap.height) > MAX_INPUT_PIXELS as u64 {
                return Err("tracking readback exceeds pixel bound".into());
            }
            let pixels = read_texture_rgba16f(&gpu, &tap.texture, tap.width, tap.height);
            let plane =
                TrackingPlane::from_rgba(&pixels, tap.width, tap.height).map_err(str::to_owned)?;
            drop(pixels);
            if index == 0 {
                match request.method {
                    TrackingMethod::FixedReference => {
                        tracker = Some(
                            TranslationTracker::new(&plane, plan.center, plan.size, request.config)
                                .map_err(|e| format!("tracking reference rejected: {e:?}"))?,
                        )
                    }
                    TrackingMethod::AdaptiveBidirectional => {
                        adaptive = Some(
                            BidirectionalTranslationTracker::new(
                                &plane,
                                plan.center,
                                plan.size,
                                request.config,
                            )
                            .map_err(|e| format!("tracking reference rejected: {e:?}"))?,
                        )
                    }
                }
                result.analysis_size = plane.size();
                result.frame_size = [tap.width, tap.height];
                result.samples.push(WindowTrackingSample {
                    sequence_tick: tick,
                    local_tick: Tick(tick.0 - plan.clip_start.0),
                    center: plan.center,
                    score: 1.0,
                    margin: 1.0,
                    roundtrip_error_pixels: (request.method
                        == TrackingMethod::AdaptiveBidirectional)
                        .then_some(0.0),
                });
            } else {
                let found = match request.method {
                    TrackingMethod::FixedReference => tracker
                        .as_mut()
                        .unwrap()
                        .step(&plane, || cancel.load(Ordering::Relaxed))
                        .map(|f| (f.center, f.score, f.margin, None)),
                    TrackingMethod::AdaptiveBidirectional => adaptive
                        .as_mut()
                        .unwrap()
                        .step(&plane, || cancel.load(Ordering::Relaxed))
                        .map(|f| (f.center, f.score, f.margin, Some(f.roundtrip_error_pixels))),
                };
                match found {
                    Ok((center, score, margin, roundtrip_error_pixels)) => {
                        result.samples.push(WindowTrackingSample {
                            sequence_tick: tick,
                            local_tick: Tick(tick.0 - plan.clip_start.0),
                            center,
                            score,
                            margin,
                            roundtrip_error_pixels,
                        })
                    }
                    Err(TrackingStop::Cancelled) => return Err("tracking cancelled".into()),
                    Err(reason) => {
                        result.stop_reason =
                            Some(format!("{reason:?} at sequence tick {}", tick.0));
                        break;
                    }
                }
            }
            progress((index + 1) as f32 / plan.times.len() as f32);
        }
        result.completed = result.samples.len() == plan.times.len();
        if source_fingerprint(&*owned.lock().map_err(|_| "tracking snapshot unavailable")?)
            != sources
        {
            return Err("tracking source files changed during analysis".into());
        }
        Ok(result)
    })();
    session.shutdown();
    work
}

impl WindowTrackingProposal {
    /// Produces one normal grade command. Caller executes it as one discrete
    /// history edit only after the user explicitly applies this proposal.
    pub fn apply_command(
        &self,
        doc: &Document,
        current_revision: u64,
        replace_center_keys: bool,
        allow_partial: bool,
    ) -> Result<TimelineCmd, String> {
        if doc.id != self.document_id
            || current_revision != self.expected_revision
            || document_fingerprint(doc)? != self.document_fingerprint
            || source_fingerprint(doc) != self.source_fingerprint
        {
            return Err("tracking proposal is stale; analyze the current document again".into());
        }
        if !self.completed && !allow_partial {
            return Err(
                "tracking stopped early; explicit partial-result approval is required".into(),
            );
        }
        if !(2..=MAX_TRACKING_FRAMES).contains(&self.samples.len()) {
            return Err("tracking proposal needs at least two valid samples".into());
        }
        let project = doc.timeline.as_ref().ok_or("no timeline")?;
        let track = project
            .sequences
            .get(&self.sequence)
            .and_then(|s| s.track(self.track))
            .ok_or("tracking owner missing")?;
        if track.locked {
            return Err("tracking target track is locked".into());
        }
        let clip = track
            .clips
            .iter()
            .find(|c| c.id == self.clip)
            .ok_or("tracking clip missing")?;
        if !clip.enabled || !track.enabled || clip.composition.is_some() {
            return Err("tracking target is inactive or composited".into());
        }
        let mut grade = clip.grade.clone().ok_or("tracking grade missing")?;
        if grade.bypass || grade.graph.is_some() {
            return Err("tracking apply requires an enabled ordered grade".into());
        }
        let op = grade
            .ops
            .iter_mut()
            .find(|o| o.id == self.op)
            .ok_or("tracking corrector missing")?;
        if !op.enabled
            || !matches!(
                op.mask,
                Some(GradeMask::PowerWindow {
                    shape: WindowShape::Ellipse | WindowShape::Rectangle,
                    ..
                })
            )
        {
            return Err("tracking target window is inactive or unsupported".into());
        }
        let lo = self.samples.iter().map(|s| s.local_tick).min().unwrap();
        let hi = self.samples.iter().map(|s| s.local_tick).max().unwrap();
        let mut seen = std::collections::HashSet::new();
        for sample in &self.samples {
            let valid_consistency = match (self.method, sample.roundtrip_error_pixels) {
                (TrackingMethod::FixedReference, None) => true,
                (TrackingMethod::AdaptiveBidirectional, Some(e)) => {
                    e.is_finite() && (0.0..=1.0).contains(&e)
                }
                _ => false,
            };
            if !valid_consistency || !sample.score.is_finite() || !sample.margin.is_finite() {
                return Err("tracking sample has invalid method or consistency provenance".into());
            }

            if sample.local_tick.0 < 0
                || sample.local_tick.0 >= clip.duration.0
                || Some(sample.sequence_tick.0) != sample.local_tick.0.checked_add(clip.start.0)
                || !seen.insert(sample.local_tick.0)
                || sample
                    .center
                    .iter()
                    .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
            {
                return Err("invalid tracking proposal coordinates/times".into());
            }
        }
        for (channel, path) in ["mask.center[0]", "mask.center[1]"].iter().enumerate() {
            if op
                .params
                .tracks
                .iter()
                .filter(|t| t.property.as_str() == *path)
                .count()
                > 1
            {
                return Err("duplicate center lanes".into());
            }
            let lane = op.params.track_mut(&PropPath::new(*path));
            if lane.orphaned {
                return Err("orphaned center lane".into());
            }
            if !replace_center_keys && !lane.keyframes.is_empty() {
                return Err("existing center animation requires explicit edit permission; outside keys remain but adjoining interpolation can change".into());
            }
            lane.keyframes.retain(|k| k.at < lo || k.at > hi);
            for sample in &self.samples {
                let valid_consistency = match (self.method, sample.roundtrip_error_pixels) {
                    (TrackingMethod::FixedReference, None) => true,
                    (TrackingMethod::AdaptiveBidirectional, Some(e)) => {
                        e.is_finite() && (0.0..=1.0).contains(&e)
                    }
                    _ => false,
                };
                if !valid_consistency || !sample.score.is_finite() || !sample.margin.is_finite() {
                    return Err(
                        "tracking sample has invalid method or consistency provenance".into(),
                    );
                }

                lane.insert_keyframe(Keyframe::new(
                    sample.local_tick,
                    PropValue::Float(sample.center[channel]),
                    Interp::Linear,
                ));
            }
        }
        timeline::ops::set_grade(project, self.sequence, self.track, self.clip, Some(grade))
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::{Color, Command};
    use timeline::{
        Clip, ClipSource, FrameRate, Grade, GradeOp, GradeOpKind, GradeOpParams, Sequence,
        TimelineProject, Track, TrackKind,
    };

    fn fixture() -> (Document, WindowTrackingRequest) {
        let mut doc = Document::new("Tracking test", 96.0, 72.0);
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("Test", FrameRate::FPS_30, 96, 72);
        let mut track = Track::new(TrackKind::Video, "Video");
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: Color::rgb(0.2, 0.3, 0.4),
            },
            Tick::from_seconds(1),
            Tick::from_seconds(10),
        );
        let mut op = GradeOp::new(
            GradeOpKind::Exposure,
            GradeOpParams::Exposure { stops: 1.0 },
        );
        op.mask = Some(GradeMask::PowerWindow {
            shape: WindowShape::Ellipse,
            center: [0.5, 0.5],
            size: [0.15, 0.15],
            rotation: 0.0,
            softness: 0.1,
            invert: false,
        });
        let request = WindowTrackingRequest {
            clip: clip.id,
            op: op.id,
            start: clip.start,
            end: Tick(clip.start.0 + sequence.frame_rate.ticks_per_frame().0 * 3),
            step_frames: 1,
            format_index: None,
            config: TrackingConfig::default(),
            method: TrackingMethod::FixedReference,
        };
        clip.grade = Some(Grade {
            ops: vec![op],
            ..Grade::new()
        });
        track.clips.push(clip);
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        doc.timeline = Some(project);
        (doc, request)
    }
    fn proposal(doc: &Document, request: &WindowTrackingRequest) -> WindowTrackingProposal {
        let p = plan(doc, request).unwrap();
        WindowTrackingProposal {
            method: request.method,
            document_id: doc.id,
            document_fingerprint: document_fingerprint(doc).unwrap(),
            source_fingerprint: source_fingerprint(doc),
            expected_revision: 0,
            sequence: p.sequence,
            track: p.track,
            clip: request.clip,
            op: request.op,
            requested_start: request.start,
            requested_end: request.end,
            analysis_size: [96, 72],
            frame_size: [96, 72],
            samples: p
                .times
                .iter()
                .enumerate()
                .map(|(i, t)| WindowTrackingSample {
                    sequence_tick: *t,
                    local_tick: Tick(t.0 - p.clip_start.0),
                    center: [0.5 + i as f64 * 0.01, 0.5],
                    score: 0.99,
                    margin: 0.2,
                    roundtrip_error_pixels: (request.method
                        == TrackingMethod::AdaptiveBidirectional)
                        .then_some(0.0),
                })
                .collect(),
            completed: true,
            stop_reason: None,
        }
    }
    fn op_mut(doc: &mut Document) -> &mut GradeOp {
        &mut doc
            .timeline
            .as_mut()
            .unwrap()
            .sequences
            .values_mut()
            .next()
            .unwrap()
            .video_tracks[0]
            .clips[0]
            .grade
            .as_mut()
            .unwrap()
            .ops[0]
    }
    #[test]
    fn tracking_proposal_is_one_undoable_clip_local_edit_and_serializable() {
        let (mut doc, request) = fixture();
        let before = serde_json::to_value(&doc.timeline).unwrap();
        let p = proposal(&doc, &request);
        let p: WindowTrackingProposal =
            serde_json::from_value(serde_json::to_value(p).unwrap()).unwrap();
        let cmd = p.apply_command(&doc, 0, false, false).unwrap();
        assert_eq!(before, serde_json::to_value(&doc.timeline).unwrap());
        let mut history = CommandHistory::new(32);
        history.execute_discrete(Command::Timeline(cmd), &mut doc);
        assert_eq!(history.undo_depth(), 1);
        let op = op_mut(&mut doc);
        assert_eq!(op.params.tracks.len(), 2);
        assert!(op
            .params
            .tracks
            .iter()
            .all(|l| l.keyframes.len() == 4 && l.keyframes[0].at == Tick(0)));
        assert!(p
            .apply_command(&doc, 0, false, false)
            .unwrap_err()
            .contains("stale"));
        assert!(history.undo(&mut doc));
        assert_eq!(before, serde_json::to_value(&doc.timeline).unwrap());
    }
    #[test]
    fn tracking_apply_refuses_stale_partial_locked_and_invalid_samples() {
        let (doc, request) = fixture();
        let p = proposal(&doc, &request);
        assert!(p.apply_command(&doc, 1, false, false).is_err());
        let mut partial = p.clone();
        partial.completed = false;
        assert!(partial.apply_command(&doc, 0, false, false).is_err());
        assert!(partial.apply_command(&doc, 0, false, true).is_ok());
        for center in [[f64::NAN, 0.5], [-0.1, 0.5], [0.5, 1.1]] {
            let mut bad = p.clone();
            bad.samples[0].center = center;
            assert!(bad.apply_command(&doc, 0, false, false).is_err());
        }
        let mut bad = p.clone();
        bad.samples[1] = bad.samples[0].clone();
        assert!(bad.apply_command(&doc, 0, false, false).is_err());
        let mut locked = doc.clone();
        locked
            .timeline
            .as_mut()
            .unwrap()
            .sequences
            .values_mut()
            .next()
            .unwrap()
            .video_tracks[0]
            .locked = true;
        let p = proposal(&locked, &request);
        assert!(p
            .apply_command(&locked, 0, false, false)
            .unwrap_err()
            .contains("locked"));
    }
    #[test]
    fn tracking_requires_explicit_key_replacement_and_preserves_outside_keys() {
        let (mut doc, request) = fixture();
        let outside = Tick::from_seconds(3);
        for (at, v) in [(Tick(0), 0.2), (outside, 0.8)] {
            op_mut(&mut doc)
                .params
                .track_mut(&PropPath::new("mask.center[0]"))
                .insert_keyframe(Keyframe::new(at, PropValue::Float(v), Interp::Linear));
        }
        let p = proposal(&doc, &request);
        assert!(p
            .apply_command(&doc, 0, false, false)
            .unwrap_err()
            .contains("permission"));
        let cmd = p.apply_command(&doc, 0, true, false).unwrap();
        CommandHistory::new(32).execute_discrete(Command::Timeline(cmd), &mut doc);
        let lane = op_mut(&mut doc)
            .params
            .track_mut(&PropPath::new("mask.center[0]"));
        assert_eq!(lane.keyframes.len(), 5);
        assert!(lane
            .keyframes
            .iter()
            .any(|k| k.at == outside && k.value == PropValue::Float(0.8)));
    }
    #[test]
    fn tracking_plan_bounds_forward_reverse_and_unsupported_owners() {
        let (mut doc, mut request) = fixture();
        let forward = plan(&doc, &request).unwrap().times;
        std::mem::swap(&mut request.start, &mut request.end);
        assert_eq!(
            plan(&doc, &request).unwrap().times,
            forward.into_iter().rev().collect::<Vec<_>>()
        );
        request.end = request.start;
        assert!(validate_request(&doc, &request).is_err());
        request.end = Tick::from_seconds(9);
        assert!(validate_request(&doc, &request).is_err());
        request.step_frames = 60;
        assert!(validate_request(&doc, &request).is_ok());
        request.step_frames = 0;
        assert!(validate_request(&doc, &request).is_err());
        request.step_frames = 60;
        op_mut(&mut doc).mask = None;
        assert!(validate_request(&doc, &request).is_err());
    }
    #[test]
    fn tracking_method_legacy_serde_and_consistency_validation() {
        let (doc, mut request) = fixture();
        let fixed = proposal(&doc, &request);
        let mut legacy = serde_json::to_value(&fixed).unwrap();
        legacy.as_object_mut().unwrap().remove("method");
        for sample in legacy["samples"].as_array_mut().unwrap() {
            sample
                .as_object_mut()
                .unwrap()
                .remove("roundtrip_error_pixels");
        }
        let decoded: WindowTrackingProposal = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.method, TrackingMethod::FixedReference);
        assert!(decoded.apply_command(&doc, 0, false, false).is_ok());
        request.method = TrackingMethod::AdaptiveBidirectional;
        let adaptive = proposal(&doc, &request);
        let decoded: WindowTrackingProposal =
            serde_json::from_str(&serde_json::to_string(&adaptive).unwrap()).unwrap();
        assert_eq!(decoded.method, TrackingMethod::AdaptiveBidirectional);
        assert!(decoded.apply_command(&doc, 0, false, false).is_ok());
        let before = document_fingerprint(&doc).unwrap();
        for error in [
            None,
            Some(-0.1),
            Some(1.001),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            let mut bad = adaptive.clone();
            bad.samples[1].roundtrip_error_pixels = error;
            assert!(bad
                .apply_command(&doc, 0, false, false)
                .unwrap_err()
                .contains("provenance"));
            assert_eq!(document_fingerprint(&doc).unwrap(), before);
        }
        let mut bad = fixed;
        bad.samples[1].roundtrip_error_pixels = Some(0.0);
        assert!(bad.apply_command(&doc, 0, false, false).is_err());
    }

    #[test]
    fn tracking_actual_isolated_engine_tracks_legacy_and_native_translation() {
        use timeline::color::{
            InputMatrix, InputSignalRange, NativeInputColorInterpretation, NativeInputStandard,
            NativeManagedColorConfig, SequenceColorConfig,
        };
        use timeline::{AssetKind, MediaAsset};
        let gpu =
            GpuContext::request_blocking().expect("GPU required for actual tracking qualification");
        let root =
            std::env::temp_dir().join(format!("photonic-window-tracking-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("texture.png");
        let mut img = image::RgbaImage::new(96, 72);
        for (x, y, p) in img.enumerate_pixels_mut() {
            let v = ((x.wrapping_mul(73856093) ^ y.wrapping_mul(19349663)) % 191 + 32) as u8;
            *p = image::Rgba([v, v, v, 255]);
        }
        img.save(&path).unwrap();
        for native in [false, true] {
            for backward in [false, true] {
                for method in [
                    TrackingMethod::FixedReference,
                    TrackingMethod::AdaptiveBidirectional,
                ] {
                    let (mut doc, mut request) = fixture();
                    request.method = method;
                    let mut asset = MediaAsset::from_file(AssetKind::Image, path.clone());
                    asset.native_input_color = Some(NativeInputColorInterpretation {
                        version: 1,
                        standard: NativeInputStandard::SrgbDisplay,
                        range: InputSignalRange::Full,
                        matrix: InputMatrix::Rgb,
                        chroma_location: None,
                        hlg_peak_nits: None,
                        reference_white_nits: None,
                    });
                    let project = doc.timeline.as_mut().unwrap();
                    let aid = asset.id;
                    project.media.assets.insert(aid, asset);
                    let seq = project.sequences.values_mut().next().unwrap();
                    if native {
                        seq.color = SequenceColorConfig::NativeManaged(Box::new(
                            NativeManagedColorConfig::sdr_draft(),
                        ));
                    }
                    let clip = &mut seq.video_tracks[0].clips[0];
                    clip.source = ClipSource::Asset { asset: aid };
                    let duration = Tick(request.end.0 - request.start.0);
                    for (at, v) in [(Tick(0), 0.0), (duration, 6.0)] {
                        clip.transform
                            .track_mut(&PropPath::new("transform.x"))
                            .insert_keyframe(Keyframe::new(
                                at,
                                PropValue::Float(v),
                                Interp::Linear,
                            ));
                    }
                    if backward {
                        std::mem::swap(&mut request.start, &mut request.end);
                        if let Some(GradeMask::PowerWindow { center, .. }) =
                            &mut clip.grade.as_mut().unwrap().ops[0].mask
                        {
                            center[0] += 6.0 / 96.0;
                        }
                    }
                    let before = document_fingerprint(&doc).unwrap();
                    let mut progress = Vec::new();
                    let result = analyze_window(
                        doc.clone(),
                        0,
                        request,
                        gpu.clone(),
                        &AtomicBool::new(false),
                        |v| progress.push(v),
                    )
                    .unwrap();
                    assert!(
                        result.completed,
                        "native={native} backward={backward}: {:?}",
                        result.stop_reason
                    );
                    assert_eq!(result.method, method);
                    assert_eq!(result.samples.len(), 4);
                    assert_eq!(result.frame_size, [96, 72]);
                    assert_eq!(progress.last(), Some(&1.0));
                    for (i, sample) in result.samples.iter().enumerate() {
                        let frame = if backward { 3 - i } else { i };
                        let expected = 0.5 + frame as f64 * 2.0 / 96.0;
                        assert!(
                            (sample.center[0] - expected).abs() < 0.006,
                            "native={native} backward={backward}: {:?} vs {expected}",
                            sample.center
                        );
                        assert!((sample.center[1] - 0.5).abs() < 0.006);
                    }
                    assert_eq!(document_fingerprint(&doc).unwrap(), before);
                    assert!(result.apply_command(&doc, 0, false, false).is_ok());
                }
            }
        }
        let (doc, request) = fixture();
        assert!(
            analyze_window(doc, 0, request, gpu, &AtomicBool::new(true), |_| {})
                .unwrap_err()
                .contains("cancelled")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
