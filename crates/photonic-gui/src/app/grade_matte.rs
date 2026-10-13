//! Read-only grading graph output inspection, independent of qualifier input sampling.
use super::*;
use photonic_video::graph::{ir::FrameColorEncoding, ScopeTapPoint};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub sequence: SequenceId,
    pub track: TrackId,
    pub clip: ClipId,
    pub node: u32,
    pub stage: photonic_core::timeline::GradeInspectionStage,
    pub image: bool,
    pub input: Option<(GradeOpId, Option<u32>)>,
}
impl Target {
    pub fn input_label(self, doc: &Document) -> String {
        let Some((op, node)) = self.input else {
            return format!("node {}", self.node);
        };
        let operator = doc
            .timeline
            .as_ref()
            .and_then(|p| {
                let clip = p
                    .sequences
                    .get(&self.sequence)?
                    .track(self.track)?
                    .clips
                    .iter()
                    .find(|c| c.id == self.clip)?;
                self.stage
                    .resolve_grade(p, clip)?
                    .ops
                    .iter()
                    .enumerate()
                    .find(|(_, candidate)| candidate.id == op)
            })
            .map(|(index, candidate)| format!("{} · {:?}", index + 1, candidate.kind))
            .unwrap_or_else(|| "Unavailable corrector".into());
        match node {
            Some(node) => format!("{operator} · node {node}"),
            None => format!("{operator} · automatic"),
        }
    }
    pub fn tap(self) -> ScopeTapPoint {
        if let Some((op, node)) = self.input {
            return ScopeTapPoint::GradeStageInput {
                clip: self.clip,
                stage: self.stage,
                node,
                op,
            };
        }
        if self.stage != photonic_core::timeline::GradeInspectionStage::Clip {
            return if self.image {
                ScopeTapPoint::GradeStageGraphImageOutput {
                    clip: self.clip,
                    stage: self.stage,
                    node: self.node,
                }
            } else {
                ScopeTapPoint::GradeStageGraphMatteOutput {
                    clip: self.clip,
                    stage: self.stage,
                    node: self.node,
                }
            };
        }
        if self.image {
            ScopeTapPoint::GradeGraphImageOutput {
                clip: self.clip,
                node: self.node,
            }
        } else {
            ScopeTapPoint::GradeGraphMatteOutput {
                clip: self.clip,
                node: self.node,
            }
        }
    }
    fn valid(self, doc: &Document) -> bool {
        doc.timeline
            .as_ref()
            .filter(|p| p.active_sequence == Some(self.sequence))
            .and_then(|p| p.sequences.get(&self.sequence))
            .filter(|s| {
                matches!(
                    s.color,
                    photonic_core::timeline::color::SequenceColorConfig::NativeManaged(_)
                )
            })
            .and_then(|s| s.track(self.track))
            .and_then(|t| t.clips.iter().find(|c| c.id == self.clip))
            .and_then(|c| self.stage.resolve_grade(doc.timeline.as_ref()?, c))
            .is_some_and(|grade| {
                if let Some((op, node)) = self.input {
                    return !grade.bypass
                        && grade.has_grade_input(op, node)
                        && grade.ops.iter().any(|candidate| {
                            candidate.id == op
                                && candidate.enabled
                                && (candidate.kind
                                    == photonic_core::timeline::GradeOpKind::HslQualifier
                                    || (self.image
                                        && candidate.kind
                                            == photonic_core::timeline::GradeOpKind::Curves))
                        });
                }
                if self.image {
                    grade.has_image_output(self.node)
                } else {
                    grade.has_matte_output(self.node)
                }
            })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub target: Target,
    pub time: Tick,
    pub revision: u64,
    pub generation: u64,
    pub hash: u128,
    pub quality: photonic_video::PreviewQuality,
    pub proxy: photonic_video::ProxyMode,
}
pub(super) struct Job {
    key: Key,
    receiver: std::sync::mpsc::Receiver<Result<(Vec<u8>, u32, u32), String>>,
}
pub(super) struct Preview {
    pub key: Key,
    pub texture: egui::TextureHandle,
}
impl PhotonicApp {
    pub(super) fn toggle_grade_matte(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        target: Target,
    ) {
        self.grade_matte_target = if target.valid(doc) && self.grade_matte_target != Some(target) {
            Some(target)
        } else {
            None
        };
        // Retain the one in-flight job until it drains; switching targets must
        // not create an unbounded queue of readback/tone-map workers.
        self.grade_matte_preview = None;
        self.qualifier_matte_target = None;
        self.qualifier_matte_job = None;
        self.qualifier_matte_preview = None;
        self.eyedropper.cancel();
        if let Some(job) = self.video_eyedropper_job.as_mut() {
            job.canceled = true;
        }
        ctx.data_mut(|data| {
            data.insert_temp(
                egui::Id::new("qualifier_matte_target"),
                None::<(SequenceId, ClipId, GradeOpId, Option<u32>)>,
            )
        });
    }
    pub(super) fn update_grade_matte(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        history: &CommandHistory,
    ) {
        let Some(target) = self.grade_matte_target else {
            if let Some(job) = self.grade_matte_job.as_ref() {
                match job.receiver.try_recv() {
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(50))
                    }
                    _ => self.grade_matte_job = None,
                }
            }
            return;
        };
        if !target.valid(doc) || !self.timeline_selection.contains(&target.clip) {
            self.grade_matte_target = None;
            self.grade_matte_preview = None;
            return;
        }
        if self.monitor_playing {
            return;
        }
        let Some(bridge) = self.engine.as_ref() else {
            return;
        };
        let Some(frame) = bridge.session.latest_frame() else {
            return;
        };
        if frame.sequence != target.sequence
            || frame.doc_revision != history.revision()
            || bridge.presented_frame != Some((frame.time, frame.sequence))
            || frame.scope_tap_point != target.tap()
            || frame.scope_tap_encoding
                != Some(if target.image || target.input.is_some() {
                    FrameColorEncoding::SceneLinearAcescg
                } else {
                    FrameColorEncoding::MatteWeight
                })
            || !frame.color_errors.is_empty()
            || !frame.grading_errors.is_empty()
            || !if let Some((op, node)) = target.input {
                photonic_video::graph::compile::NativeStageGradeInput::find(
                    &frame.native_stage_grade_inputs,
                    target.clip,
                    target.stage,
                    node,
                    op,
                )
                .is_some_and(|input| target.image || input.qualifier.is_some())
            } else {
                photonic_video::graph::compile::GradeGraphStageOutput::find(
                    &frame.native_stage_graph_outputs,
                    target.clip,
                    target.stage,
                    target.node,
                    target.image,
                )
                .is_some()
            }
        {
            self.grade_matte_preview = None;
            return;
        }
        let Some(tap) = frame.scope_tap.as_ref() else {
            return;
        };
        let key = Key {
            target,
            time: frame.time,
            revision: frame.doc_revision,
            generation: frame.snapshot_generation,
            hash: frame.content_hash.0,
            quality: frame.preview_quality,
            proxy: frame.proxy_mode,
        };
        if self
            .grade_matte_preview
            .as_ref()
            .is_some_and(|p| p.key == key)
        {
            return;
        }
        if self
            .grade_matte_job
            .as_ref()
            .is_some_and(|job| job.key != key)
        {
            self.grade_matte_preview = None;
            match self.grade_matte_job.as_ref().unwrap().receiver.try_recv() {
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(std::time::Duration::from_millis(50));
                    return;
                }
                _ => self.grade_matte_job = None,
            }
        }
        if let Some(job) = self.grade_matte_job.as_ref() {
            if job.key == key {
                match job.receiver.try_recv() {
                    Ok(Ok((rgba, w, h))) => {
                        self.grade_matte_preview = Some(Preview {
                            key,
                            texture: ctx.load_texture(
                                format!("grade-matte-{}-{}", target.node, key.time.0),
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [w as usize, h as usize],
                                    &rgba,
                                ),
                                egui::TextureOptions::LINEAR,
                            ),
                        });
                        self.grade_matte_job = None;
                    }
                    Ok(Err(error)) => {
                        self.reference_still_status =
                            Some(format!("Graph output inspection failed: {error}"));
                        self.grade_matte_target = None;
                        self.grade_matte_job = None;
                        self.grade_matte_preview = None;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(50))
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.reference_still_status = Some("Graph output worker stopped".into());
                        self.grade_matte_target = None;
                        self.grade_matte_job = None;
                        self.grade_matte_preview = None;
                    }
                }
                return;
            }
        }
        self.grade_matte_preview = None;
        let qualifier = target.input.and_then(|(op, node)| {
            (!target.image)
                .then(|| {
                    photonic_video::graph::compile::NativeStageGradeInput::find(
                        &frame.native_stage_grade_inputs,
                        target.clip,
                        target.stage,
                        node,
                        op,
                    )
                    .and_then(|input| input.qualifier.clone())
                })
                .flatten()
        });
        let texture = Arc::clone(&tap.texture);
        let (width, height) = (tap.width, tap.height);
        let gpu = photonic_video::GpuContext::new(
            Arc::clone(bridge.gpu().device()),
            Arc::clone(bridge.gpu().queue()),
        );
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = (|| -> Result<(Vec<u8>, u32, u32), String> {
                let pixels = if let Some(key) = qualifier {
                    let matte =
                        photonic_render::grade_gpu::native_qualifier_matte_with_spatial_gpu(
                            gpu.device(),
                            gpu.queue(),
                            &texture,
                            &key.qualifier,
                            key.mask.as_ref(),
                            (width, height),
                            key.spatial,
                        )
                        .map_err(str::to_owned)?;
                    photonic_video::graph::eval::read_texture_rgba16f(&gpu, &matte, width, height)
                } else {
                    photonic_video::graph::eval::read_texture_rgba16f(&gpu, &texture, width, height)
                };
                if target.image {
                    photonic_video::color::native_output::inspection::inspect_scene_image(
                        &pixels, width, height,
                    )
                    .map(|inspection| (inspection.rgba, width, height))
                    .map_err(str::to_owned)
                } else {
                    photonic_render::matte_inspection::inspect_weights(&pixels, width, height)
                        .map(|inspection| (inspection.rgba, width, height))
                        .map_err(str::to_owned)
                }
            })();
            let _ = sender.send(result);
        });
        self.grade_matte_job = Some(Job { key, receiver });
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::color::{NativeManagedColorConfig, SequenceColorConfig};
    use photonic_core::timeline::{
        CdlParams, Clip, ClipSource, FrameRate, Grade, GradeGraphNode, GradeOp, GradeOpKind,
        GradeOpParams, Sequence, TimelineProject, Track, TrackKind,
    };

    fn fixture() -> (Document, Target) {
        let mut doc = Document::new("matte inspection", 16.0, 16.0);
        let mut sequence = Sequence::new("matte", FrameRate::FPS_30, 16, 16);
        sequence.color =
            SequenceColorConfig::NativeManaged(Box::new(NativeManagedColorConfig::sdr_draft()));
        let mut track = Track::new(TrackKind::Video, "V1");
        let mut clip = Clip::new(
            ClipSource::SolidColor {
                color: photonic_core::Color::WHITE,
            },
            Tick::ZERO,
            Tick::from_seconds(1),
        );
        let mut grade = Grade::new();
        let op = GradeOp::new(
            GradeOpKind::HslQualifier,
            GradeOpParams::HslQualifier {
                hue: [0.0, 1.0],
                sat: [0.0, 1.0],
                lum: [0.0, 1.0],
                softness: 0.0,
                correction: CdlParams::identity(),
                keys: vec![],
                matte_levels: [0.0, 0.0],
            },
        );
        let op_id = op.id;
        grade.ops.push(op);
        grade.convert_to_graph();
        let input = *grade
            .graph
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, n)| matches!(n, GradeGraphNode::Input))
            .unwrap()
            .0;
        let node = grade
            .add_graph_utility(GradeGraphNode::QualifierMatte {
                input,
                op: op_id,
                label: String::new(),
            })
            .unwrap();
        let corrected =
            match grade.graph.as_ref().unwrap().nodes[&grade.graph.as_ref().unwrap().output] {
                GradeGraphNode::Output { input } => input,
                _ => unreachable!(),
            };
        grade
            .add_graph_utility(GradeGraphNode::MatteApply {
                original: input,
                corrected,
                matte: node,
                label: String::new(),
            })
            .unwrap();
        let target = Target {
            input: None,
            stage: photonic_core::timeline::GradeInspectionStage::Clip,
            sequence: sequence.id,
            track: track.id,
            clip: clip.id,
            node,
            image: false,
        };
        clip.grade = Some(grade);
        track.clips.push(clip);
        sequence.video_tracks.push(track);
        let mut project = TimelineProject::new();
        project.insert_sequence(sequence);
        doc.timeline = Some(project);
        (doc, target)
    }

    #[test]
    fn look_input_targets_are_typed_stage_exact_and_readonly() {
        use photonic_core::timeline::{ClipLook, GradeInspectionStage as Stage};
        let (mut doc, output) = fixture();
        let clip = &mut doc
            .timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&output.sequence)
            .unwrap()
            .video_tracks[0]
            .clips[0];
        let grade = clip.grade.as_ref().unwrap().clone();
        let op = grade.ops[0].id;
        let non_input = grade.graph.as_ref().unwrap().output;
        clip.look = Some(ClipLook::Local(Box::new(grade)));
        let before = doc.to_json().unwrap();
        let input = Target {
            stage: Stage::LocalLook,
            input: Some((op, None)),
            image: true,
            ..output
        };
        let key = Target {
            image: false,
            ..input
        };
        assert!(input.valid(&doc));
        assert!(key.valid(&doc));
        assert_ne!(input, key);
        assert_ne!(input, output);
        assert_eq!(
            input.tap(),
            ScopeTapPoint::GradeStageInput {
                clip: output.clip,
                stage: Stage::LocalLook,
                node: None,
                op
            }
        );
        assert!(
            !Target {
                input: Some((op, Some(non_input))),
                ..input
            }
            .valid(&doc),
            "grade output is not a correction input"
        );
        assert!(!Target {
            input: Some((GradeOpId::new(), None)),
            ..key
        }
        .valid(&doc));
        assert!(
            Target {
                input: Some((op, Some(output.node))),
                ..input
            }
            .valid(&doc),
            "explicit qualifier-key source is a valid input address"
        );
        assert_eq!(doc.to_json().unwrap(), before);
        doc.timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&output.sequence)
            .unwrap()
            .video_tracks[0]
            .clips[0]
            .look = None;
        assert!(!input.valid(&doc));
        assert!(!key.valid(&doc));
        assert!(output.valid(&doc));
    }

    #[test]
    fn look_targets_require_current_link_and_cache_identity() {
        use photonic_core::timeline::{
            ClipLook, GradeInspectionStage as Stage, SharedLook, SharedLookId,
        };
        let (mut doc, target) = fixture();
        let p = doc.timeline.as_mut().unwrap();
        let clip = &mut p.sequences.get_mut(&target.sequence).unwrap().video_tracks[0].clips[0];
        let grade = clip.grade.as_ref().unwrap().clone();
        clip.look = Some(ClipLook::Local(Box::new(grade.clone())));
        let local = Target {
            stage: Stage::LocalLook,
            ..target
        };
        assert!(local.valid(&doc));
        assert_ne!(local, target);
        assert_ne!(local.tap(), target.tap());
        let id = SharedLookId::new();
        let p = doc.timeline.as_mut().unwrap();
        p.shared_looks.insert(
            id,
            SharedLook {
                versions: Vec::new(),
                active_version: None,
                id,
                name: "Linked".into(),
                grade,
            },
        );
        p.sequences.get_mut(&target.sequence).unwrap().video_tracks[0].clips[0].look =
            Some(ClipLook::Shared(id));
        let shared = Target {
            stage: Stage::SharedLook { id },
            ..target
        };
        assert!(shared.valid(&doc));
        assert!(!local.valid(&doc));
        assert!(!Target {
            stage: Stage::SharedLook {
                id: SharedLookId::new()
            },
            ..target
        }
        .valid(&doc));
        doc.timeline.as_mut().unwrap().shared_looks.remove(&id);
        assert!(!shared.valid(&doc));
        assert!(target.valid(&doc));
    }

    #[test]
    fn matte_target_rejects_wrong_scope_missing_nodes_and_bypassed_grade() {
        let (mut doc, target) = fixture();
        assert!(target.valid(&doc));
        assert!(!Target {
            node: u32::MAX,
            ..target
        }
        .valid(&doc));
        let seq = doc
            .timeline
            .as_mut()
            .unwrap()
            .sequences
            .get_mut(&target.sequence)
            .unwrap();
        seq.video_tracks[0].clips[0].grade.as_mut().unwrap().bypass = true;
        assert!(!target.valid(&doc));
    }

    #[test]
    fn changing_inspection_target_keeps_only_one_pending_worker() {
        let (doc, matte) = fixture();
        let mut image = matte;
        image.image = true;
        image.node = doc.timeline.as_ref().unwrap().sequences[&image.sequence].video_tracks[0]
            .clips[0]
            .grade
            .as_ref()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
            .output;
        let mut app = PhotonicApp::default();
        let ctx = egui::Context::default();
        let (sender, receiver) = std::sync::mpsc::channel();
        app.grade_matte_job = Some(Job {
            key: Key {
                target: matte,
                time: Tick::ZERO,
                revision: 0,
                generation: 0,
                hash: 0,
                quality: photonic_video::PreviewQuality::Full,
                proxy: photonic_video::ProxyMode::ForceOriginal,
            },
            receiver,
        });
        app.toggle_grade_matte(&ctx, &doc, image);
        assert_eq!(app.grade_matte_job.as_ref().unwrap().key.target, matte);
        app.toggle_grade_matte(&ctx, &doc, image);
        assert!(app.grade_matte_target.is_none());
        assert!(app.grade_matte_job.is_some());
        drop(sender);
        app.update_grade_matte(&ctx, &doc, &CommandHistory::new(4));
        assert!(app.grade_matte_job.is_none());
    }

    #[test]
    fn image_and_matte_selection_are_typed_and_readonly() {
        let (doc, matte) = fixture();
        let mut image = matte;
        image.image = true;
        assert!(!image.valid(&doc));
        image.node = doc.timeline.as_ref().unwrap().sequences[&image.sequence].video_tracks[0]
            .clips[0]
            .grade
            .as_ref()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
            .output;
        assert!(image.valid(&doc));
        assert!(!Target {
            image: false,
            ..image
        }
        .valid(&doc));
        let before = serde_json::to_value(&doc).unwrap();
        let mut app = PhotonicApp::default();
        let ctx = egui::Context::default();
        app.toggle_grade_matte(&ctx, &doc, matte);
        app.toggle_grade_matte(&ctx, &doc, image);
        assert_eq!(app.grade_matte_target, Some(image));
        app.toggle_grade_matte(&ctx, &doc, image);
        assert!(app.grade_matte_target.is_none());
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }

    #[test]
    fn toggling_matte_output_does_not_edit_document_and_can_exit() {
        let (doc, target) = fixture();
        let before = serde_json::to_value(&doc).unwrap();
        let mut app = PhotonicApp::default();
        app.toggle_grade_matte(&egui::Context::default(), &doc, target);
        assert_eq!(app.grade_matte_target, Some(target));
        app.toggle_grade_matte(&egui::Context::default(), &doc, target);
        assert!(app.grade_matte_target.is_none());
        assert_eq!(serde_json::to_value(&doc).unwrap(), before);
    }
    #[test]
    fn held_graph_matte_preview_rejects_stale_revision_and_selection() {
        use photonic_core::timeline::{AssetKind, MediaAsset};
        let engine = photonic_video::VideoEngine::headless()
            .expect("GPU required for matte output qualification");
        let (mut doc, target) = fixture();
        let path =
            std::env::temp_dir().join(format!("photonic-gui-matte-{}.png", uuid::Uuid::new_v4()));
        let image = photonic_core::raster::RasterImage::filled(16, 16, [255; 4]);
        std::fs::write(&path, image.to_png()).unwrap();
        let project = doc.timeline.as_mut().unwrap();
        let mut asset = MediaAsset::from_file(AssetKind::Image, &path);
        asset.native_input_color=Some(serde_json::from_value(serde_json::json!({"version":1,"standard":"srgb_display","range":"full","matrix":"rgb"})).unwrap());
        let asset = project.media.insert(asset);
        project
            .sequences
            .get_mut(&target.sequence)
            .unwrap()
            .video_tracks[0]
            .clips[0]
            .source = ClipSource::Asset { asset };
        let original = serde_json::to_value(&doc).unwrap();
        let mut history = CommandHistory::new(20);
        let mut bridge = engine::EngineBridge::new(engine);
        bridge.sync_document(&doc, &history);
        bridge.peek_sequence(target.sequence);
        assert!(bridge.prepare_full_preview());
        bridge
            .session
            .send(photonic_video::EngineCmd::SetScopeTap(target.tap()));
        bridge.seek(Tick::ZERO);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let frame = loop {
            if let Some(frame) = bridge.session.latest_frame() {
                if frame.scope_tap_point == target.tap() && frame.scope_tap.is_some() {
                    break frame;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "matte output never rendered"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        bridge.presented_frame = Some((frame.time, frame.sequence));
        let mut app = PhotonicApp::default();
        app.engine = Some(bridge);
        app.timeline_selection.push(target.clip);
        app.toggle_grade_matte(&egui::Context::default(), &doc, target);
        let ctx = egui::Context::default();
        while app.grade_matte_preview.is_none() && std::time::Instant::now() < deadline {
            app.update_grade_matte(&ctx, &doc, &history);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            app.grade_matte_preview
                .as_ref()
                .expect("preview")
                .texture
                .size(),
            [16, 16]
        );
        assert_eq!(serde_json::to_value(&doc).unwrap(), original);
        assert_eq!(history.revision(), 0);
        let project = doc.timeline.as_ref().unwrap();
        let mut changed_grade = project.sequences[&target.sequence].video_tracks[0].clips[0]
            .grade
            .clone()
            .unwrap();
        changed_grade.ops[0].enabled = false;
        let edit = photonic_core::timeline::ops::set_grade(
            project,
            target.sequence,
            target.track,
            target.clip,
            Some(changed_grade),
        )
        .unwrap();
        history.execute_discrete(Command::Timeline(edit), &mut doc);
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(
            app.grade_matte_preview.is_none(),
            "old readback survived a document revision"
        );
        assert!(history.undo(&mut doc));
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(
            app.grade_matte_preview.is_none(),
            "undo reused an old snapshot readback"
        );
        // A frame no longer presented must never leave its matte displayed.
        app.engine.as_mut().unwrap().presented_frame = None;
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(app.grade_matte_preview.is_none());
        app.timeline_selection.clear();
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(app.grade_matte_target.is_none());
        drop(app);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn held_graph_image_preview_rejects_stale_revision_and_selection() {
        use photonic_core::timeline::{AssetKind, MediaAsset};
        let engine = photonic_video::VideoEngine::headless()
            .expect("GPU required for matte output qualification");
        let (mut doc, mut target) = fixture();
        target.image = true;
        target.node = doc.timeline.as_ref().unwrap().sequences[&target.sequence].video_tracks[0]
            .clips[0]
            .grade
            .as_ref()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
            .output;
        let path =
            std::env::temp_dir().join(format!("photonic-gui-image-{}.png", uuid::Uuid::new_v4()));
        let image = photonic_core::raster::RasterImage::filled(16, 16, [255; 4]);
        std::fs::write(&path, image.to_png()).unwrap();
        let project = doc.timeline.as_mut().unwrap();
        let mut asset = MediaAsset::from_file(AssetKind::Image, &path);
        asset.native_input_color=Some(serde_json::from_value(serde_json::json!({"version":1,"standard":"srgb_display","range":"full","matrix":"rgb"})).unwrap());
        let asset = project.media.insert(asset);
        project
            .sequences
            .get_mut(&target.sequence)
            .unwrap()
            .video_tracks[0]
            .clips[0]
            .source = ClipSource::Asset { asset };
        let original = serde_json::to_value(&doc).unwrap();
        let mut history = CommandHistory::new(20);
        let mut bridge = engine::EngineBridge::new(engine);
        bridge.sync_document(&doc, &history);
        bridge.peek_sequence(target.sequence);
        assert!(bridge.prepare_full_preview());
        bridge
            .session
            .send(photonic_video::EngineCmd::SetScopeTap(target.tap()));
        bridge.seek(Tick::ZERO);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let frame = loop {
            if let Some(frame) = bridge.session.latest_frame() {
                if frame.scope_tap_point == target.tap() && frame.scope_tap.is_some() {
                    break frame;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "matte output never rendered"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        bridge.presented_frame = Some((frame.time, frame.sequence));
        let mut app = PhotonicApp::default();
        app.engine = Some(bridge);
        app.timeline_selection.push(target.clip);
        app.toggle_grade_matte(&egui::Context::default(), &doc, target);
        let ctx = egui::Context::default();
        while app.grade_matte_preview.is_none() && std::time::Instant::now() < deadline {
            app.update_grade_matte(&ctx, &doc, &history);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            app.grade_matte_preview
                .as_ref()
                .expect("preview")
                .texture
                .size(),
            [16, 16]
        );
        assert_eq!(serde_json::to_value(&doc).unwrap(), original);
        assert_eq!(history.revision(), 0);
        let project = doc.timeline.as_ref().unwrap();
        let mut changed_grade = project.sequences[&target.sequence].video_tracks[0].clips[0]
            .grade
            .clone()
            .unwrap();
        changed_grade.ops[0].enabled = false;
        let edit = photonic_core::timeline::ops::set_grade(
            project,
            target.sequence,
            target.track,
            target.clip,
            Some(changed_grade),
        )
        .unwrap();
        history.execute_discrete(Command::Timeline(edit), &mut doc);
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(
            app.grade_matte_preview.is_none(),
            "old readback survived a document revision"
        );
        assert!(history.undo(&mut doc));
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(
            app.grade_matte_preview.is_none(),
            "undo reused an old snapshot readback"
        );
        // A frame no longer presented must never leave its matte displayed.
        app.engine.as_mut().unwrap().presented_frame = None;
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(app.grade_matte_preview.is_none());
        app.timeline_selection.clear();
        app.update_grade_matte(&ctx, &doc, &history);
        assert!(app.grade_matte_target.is_none());
        drop(app);
        std::fs::remove_file(path).unwrap();
    }
}
