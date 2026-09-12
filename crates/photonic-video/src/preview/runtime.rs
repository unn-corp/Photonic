//! Session-owned planning and optional cached-frame decoding. Planning compiles
//! at most a small slice after a present; no render is initiated by a cache miss.
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use photonic_core::timeline::{AssetSource, SequenceId, Tick};
use photonic_render::color::{Colorimetry, Matrix, Range};
use photonic_render::video::YuvConverter;
use xxhash_rust::xxh3::Xxh3;

use super::*;
use crate::decode::scheduler::{DecodeSource, PtsKind, SourceParams};
use crate::decode::worker::DecodeWorker;
use crate::decode::{FrameRing, PixFmt, SharedRing};
use crate::graph::eval::{GpuContext, GpuFrame};
use crate::graph::ir::{ContentHash, FrameGraph, IrOp};
use crate::media::ffmpeg_locate::FfmpegTools;
use crate::media::keyframe_index::KeyframeIndex;
use crate::session::{PreviewQuality, ProxyMode, RenderSnapshot};

type Slot = (SequenceId, usize, Tick);
const MAX_RANGES: usize = 128;
const PLAN_SLICE: Duration = Duration::from_millis(2);

struct RangePlan {
    context: PreviewContext,
    spans: ChunkSpans,
    render: bool,
}
struct Building {
    context: PreviewContext,
    span: ChunkSpan,
    frames: Vec<ContentHash>,
    sources: Xxh3,
    source_hashes: Vec<ContentHash>,
    dependencies: Vec<SourceDependency>,
    render: bool,
}

/// One runtime per interactive session. Export shadow sessions never construct
/// it. The single preview worker and single cached decoder remain bounded even
/// when callers mark many ranges or issue many export requests.
struct ValidatedChunk {
    signature: ChunkSignature,
    source_hashes: Vec<ContentHash>,
    dependencies: Vec<SourceDependency>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceDependency {
    path: std::path::PathBuf,
    bytes: u64,
    modified: Option<std::time::SystemTime>,
}
impl SourceDependency {
    pub(crate) fn unchanged(&self) -> bool {
        std::fs::metadata(&self.path).is_ok_and(|metadata| {
            metadata.len() == self.bytes && metadata.modified().ok() == self.modified
        })
    }
}

pub struct PreviewRuntime {
    worker: PreviewWorker,
    plans: VecDeque<RangePlan>,
    building: Option<Building>,
    validated: HashMap<Slot, ValidatedChunk>,
    decoder: Option<PreviewDecoder>,
    gpu: GpuContext,
    tools: FfmpegTools,
    snapshot: Arc<RenderSnapshot>,
    snapshot_generation: u64,
    vector_hash: Option<ContentHash>,
    last_error: Option<String>,
    profile: PreviewProfile,
}
impl PreviewRuntime {
    pub fn new(
        gpu: GpuContext,
        tools: FfmpegTools,
        sidecar_root: std::path::PathBuf,
        snapshot: Arc<RenderSnapshot>,
        generation: u64,
    ) -> Result<Self, PreviewError> {
        let limit = snapshot
            .project
            .as_ref()
            .and_then(|p| p.settings.cache_limit_mb);
        let cache = Arc::new(PreviewCache::open(
            sidecar_root,
            CacheConfig::for_project(limit),
        )?);
        let worker = PreviewWorker::new(
            gpu.clone(),
            tools.clone(),
            cache,
            PlaybackPriorityGate::default(),
        )?;
        let mut runtime = Self {
            worker,
            plans: VecDeque::new(),
            building: None,
            validated: HashMap::new(),
            decoder: None,
            gpu,
            tools,
            snapshot,
            snapshot_generation: generation,
            vector_hash: None,
            last_error: None,
            profile: PreviewProfile::default(),
        };
        runtime.update_zones();
        Ok(runtime)
    }
    pub fn status(&self) -> PreviewStatusSnapshot {
        let mut status = self.worker.status();
        status.doc_revision = self.snapshot.revision;
        status.snapshot_generation = self.snapshot_generation;
        status.error = self.last_error.clone();
        status.profile = self.profile;
        status.planning_ranges = self.plans.len() + usize::from(self.building.is_some());
        status
    }
    pub fn set_profile(&mut self, profile: PreviewProfile) {
        if profile == self.profile {
            return;
        }
        self.profile = profile;
        let snapshot = self.snapshot.clone();
        let generation = self.snapshot_generation;
        self.set_snapshot(snapshot, generation.wrapping_add(1));
        self.snapshot_generation = generation;
    }
    pub fn pending(&self) -> bool {
        !self.plans.is_empty() || self.building.is_some() || self.worker.pending() > 0
    }
    pub fn set_playing(&self, playing: bool) {
        self.worker.gate().set_playing(playing);
    }
    pub fn set_snapshot(&mut self, snapshot: Arc<RenderSnapshot>, generation: u64) {
        if self.snapshot_generation == generation && self.snapshot.revision == snapshot.revision {
            return;
        }
        if let Some(project) = &self.snapshot.project {
            for sequence in project.sequences.keys() {
                self.worker.cancel(*sequence);
                self.worker.cache().invalidate_sequence(*sequence);
            }
        }
        self.snapshot = snapshot;
        self.snapshot_generation = generation;
        self.vector_hash = None;
        self.validated.clear();
        self.decoder = None;
        self.building = None;
        self.plans.clear();
        self.update_zones();
        // Revalidation is lazy and CPU-only. Retained content variants can
        // become green again after undo without scheduling another encode.
        let zones: Vec<_> = self
            .snapshot
            .project
            .as_ref()
            .into_iter()
            .flat_map(|p| p.sequences.values())
            .flat_map(|s| {
                s.preview_zones
                    .iter()
                    .map(move |z| (s.id, (z.start, z.end)))
            })
            .take(MAX_RANGES)
            .collect();
        for (sequence, range) in zones {
            let _ = self.request_range(sequence, range, false);
        }
    }
    fn update_zones(&mut self) {
        if let Some(project) = &self.snapshot.project {
            for sequence in project.sequences.values() {
                let zones: Vec<_> = sequence
                    .preview_zones
                    .iter()
                    .map(|z| (z.start, z.end))
                    .collect();
                self.worker.cache().set_zones(sequence.id, &zones);
            }
        }
    }
    pub fn request(
        &mut self,
        sequence: SequenceId,
        range: Option<(Tick, Tick)>,
    ) -> Result<(), PreviewError> {
        self.last_error = None;
        if let Some(range) = range {
            return self.request_range(sequence, range, true);
        }
        let project = self
            .snapshot
            .project
            .as_ref()
            .ok_or_else(|| PreviewError::Invalid("preview has no timeline".into()))?;
        let seq = project
            .sequences
            .get(&sequence)
            .ok_or_else(|| PreviewError::Invalid("preview sequence not found".into()))?;
        let zones: Vec<_> = seq.preview_zones.iter().map(|z| (z.start, z.end)).collect();
        if zones.is_empty() {
            return Err(PreviewError::Invalid("no preview zones are marked".into()));
        }
        for range in zones {
            self.request_range(sequence, range, true)?;
        }
        Ok(())
    }
    fn request_range(
        &mut self,
        sequence: SequenceId,
        range: (Tick, Tick),
        render: bool,
    ) -> Result<(), PreviewError> {
        if self.plans.len() >= MAX_RANGES {
            return Err(PreviewError::QueueFull);
        }
        let project = self
            .snapshot
            .project
            .as_ref()
            .ok_or_else(|| PreviewError::Invalid("preview has no timeline".into()))?;
        let seq = project
            .sequences
            .get(&sequence)
            .ok_or_else(|| PreviewError::Invalid("preview sequence not found".into()))?;
        let format_index = seq.active_format.min(seq.formats.len().saturating_sub(1));
        let format = seq
            .formats
            .get(format_index)
            .ok_or_else(|| PreviewError::Invalid("preview format not found".into()))?;
        let context = PreviewContext {
            sequence,
            format_index,
            width: format.width,
            height: format.height,
            frame_rate: seq.frame_rate,
            quality: PreviewQuality::Full,
            proxy_mode: ProxyMode::ForceOriginal,
            use_proxy: false,
            source_signature: ContentHash(0),
            profile: self.profile,
        };
        self.plans.push_back(RangePlan {
            context,
            spans: chunks_for_range(seq.frame_rate, range)?,
            render,
        });
        Ok(())
    }
    pub fn cancel(&mut self, sequence: SequenceId) {
        self.worker.cancel(sequence);
        self.plans.retain(|plan| plan.context.sequence != sequence);
        if self
            .building
            .as_ref()
            .is_some_and(|b| b.context.sequence == sequence)
        {
            self.building = None;
        }
    }
    pub fn clear(&mut self, sequence: SequenceId, range: Option<(Tick, Tick)>) {
        self.cancel(sequence);
        self.decoder = None;
        self.worker.clear(sequence, range);
        self.validated.retain(|_, chunk| {
            chunk.signature.context.sequence != sequence
                || range.is_some_and(|(start, end)| {
                    chunk.signature.span.start >= end || chunk.signature.span.end <= start
                })
        });
    }
    /// Compile uses the interactive session's warmed LUT, stabilization and
    /// deflicker providers. Work is sliced between engine ticks; a cache miss
    /// only asks for validation, never an expensive render.
    pub fn poll(&mut self, mut compile: impl FnMut(&PreviewContext, Tick) -> Option<FrameGraph>) {
        let started = Instant::now();
        while started.elapsed() < PLAN_SLICE {
            if self.worker.pending() >= 2 {
                break;
            }
            if self.building.is_none() {
                let Some(plan) = self.plans.front_mut() else {
                    break;
                };
                if let Some(span) = plan.spans.next() {
                    self.building = Some(Building {
                        context: plan.context.clone(),
                        span,
                        frames: Vec::with_capacity(span.frame_count as usize),
                        sources: Xxh3::new(),
                        source_hashes: Vec::new(),
                        dependencies: Vec::new(),
                        render: plan.render,
                    });
                } else {
                    self.plans.pop_front();
                    continue;
                }
            }
            let mut building = self.building.take().expect("building initialized");
            let tick = building
                .context
                .frame_rate
                .frame_start(building.span.first_frame + building.frames.len() as i64);
            let Some(graph) = compile(&building.context, tick) else {
                self.last_error = Some("preview frame could not be compiled".into());
                continue;
            };
            let Some(output) = graph.output else {
                continue;
            };
            let hash = graph.nodes[output.0 as usize].content_hash;
            match dependency_hash(&self.snapshot, &graph, &mut self.vector_hash) {
                Ok((source_hash, dependencies)) => {
                    building.frames.push(hash);
                    building.sources.update(&source_hash.0.to_le_bytes());
                    building.source_hashes.push(source_hash);
                    for dependency in dependencies {
                        if !building.dependencies.contains(&dependency) {
                            building.dependencies.push(dependency);
                        }
                    }
                    if building.dependencies.len() > 256 {
                        self.last_error = Some("preview chunk exceeds 256 source revisions".into());
                        continue;
                    }
                }
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    continue;
                }
            }
            if building.frames.len() as u64 == building.span.frame_count {
                building.context.source_signature = ContentHash(building.sources.digest128());
                let result = ChunkSignature::new(building.context, building.span, building.frames);
                match result {
                    Ok(signature) => {
                        let hit = self.worker.cache().lookup(&signature).is_some();
                        if building.render && !hit {
                            if let Err(error) = self.worker.submit(PreviewChunkRequest {
                                snapshot: self.snapshot.clone(),
                                signature: signature.clone(),
                                dependencies: building.dependencies.clone(),
                            }) {
                                self.last_error = Some(error.to_string());
                            }
                        }
                        if self.validated.len() >= self.worker.cache().config().max_entries {
                            self.validated.clear();
                        }
                        self.validated.insert(
                            (
                                signature.context.sequence,
                                signature.context.format_index,
                                signature.span.start,
                            ),
                            ValidatedChunk {
                                signature,
                                source_hashes: building.source_hashes,
                                dependencies: building.dependencies,
                            },
                        );
                    }
                    Err(error) => self.last_error = Some(error.to_string()),
                }
            } else {
                self.building = Some(building);
            }
        }
    }
    /// Full/original program playback only. Inspections, alternate scope taps,
    /// compares, source peeks and exports bypass this API entirely.
    pub fn frame(
        &mut self,
        sequence: SequenceId,
        format: usize,
        time: Tick,
        graph: &FrameGraph,
    ) -> Option<GpuFrame> {
        let project = self.snapshot.project.as_ref()?;
        let rate = project.sequences.get(&sequence)?.frame_rate;
        let span = ChunkSpan::covering(rate, time).ok()?;
        let slot = (sequence, format, span.start);
        let Some(validated) = self.validated.get(&slot) else {
            if self
                .building
                .as_ref()
                .is_none_or(|b| b.context.sequence != sequence || b.span != span)
                && self.plans.is_empty()
            {
                let _ = self.request_range(sequence, (span.start, span.end), false);
            }
            return None;
        };
        let ordinal = usize::try_from(rate.frame_at(time) - span.first_frame).ok()?;
        let signature = &validated.signature;
        let current_hash = graph.nodes[graph.output?.0 as usize].content_hash;
        let (sources, _) = dependency_hash(&self.snapshot, graph, &mut self.vector_hash).ok()?;
        if signature.frame_hashes.get(ordinal) != Some(&current_hash)
            || validated.source_hashes.get(ordinal) != Some(&sources)
            || !validated
                .dependencies
                .iter()
                .all(SourceDependency::unchanged)
        {
            self.worker
                .cache()
                .set_status(signature, ChunkState::NotRendered, 0, None);
            self.validated.remove(&slot);
            self.decoder = None;
            let _ = self.request_range(sequence, (span.start, span.end), false);
            return None;
        }
        let chunk = self.worker.cache().lookup(signature)?;
        if self
            .decoder
            .as_ref()
            .is_none_or(|decoder| decoder.chunk.signature.key != chunk.signature.key)
        {
            self.decoder = Some(PreviewDecoder::new(self.tools.clone(), chunk));
        }
        self.decoder.as_mut()?.frame(&self.gpu, time)
    }
}

/// File revision facts only enter chunks that actually reference that source.
/// The graph already contains resolved LUT/analysis payloads. Whole-document
/// vector rasterization additionally depends on its immutable drawing state.
fn dependency_hash(
    snapshot: &RenderSnapshot,
    graph: &FrameGraph,
    vector_hash: &mut Option<ContentHash>,
) -> Result<(ContentHash, Vec<SourceDependency>), PreviewError> {
    let mut hash = Xxh3::new();
    let mut dependencies = Vec::new();
    let project = snapshot
        .project
        .as_ref()
        .ok_or_else(|| PreviewError::Invalid("preview has no timeline".into()))?;
    let mut reachable = vec![false; graph.nodes.len()];
    let mut pending = graph.output.into_iter().collect::<Vec<_>>();
    while let Some(id) = pending.pop() {
        let index = id.0 as usize;
        if reachable[index] {
            continue;
        }
        reachable[index] = true;
        pending.extend(graph.nodes[index].inputs.iter().map(|(id, _)| *id));
    }
    for node in graph
        .nodes
        .iter()
        .zip(reachable)
        .filter_map(|(node, used)| used.then_some(node))
    {
        match node.op {
            IrOp::DecodeVideo { asset, .. } | IrOp::DecodeStill { asset } => {
                let proxy = matches!(node.op, IrOp::DecodeVideo { proxy: true, .. });
                let media = project
                    .media
                    .assets
                    .get(&asset)
                    .ok_or_else(|| PreviewError::Invalid("preview source missing".into()))?;
                let AssetSource::File { path, .. } = &media.source else {
                    continue;
                };
                let selected =
                    crate::media::proxy::resolve_decode_input(path, media.proxy.as_ref(), proxy);
                let metadata = std::fs::metadata(&selected)?;
                dependencies.push(SourceDependency {
                    path: selected.clone(),
                    bytes: metadata.len(),
                    modified: metadata.modified().ok(),
                });
                hash.update(asset.0.as_bytes());
                hash.update(&[u8::from(selected != *path)]);
                if let Some(content) = &media.content_hash {
                    hash.update(content.as_bytes());
                } else {
                    hash.update(selected.as_os_str().as_encoded_bytes());
                }
                hash.update(&metadata.len().to_le_bytes());
                if let Ok(modified) = metadata.modified() {
                    if let Ok(elapsed) = modified.duration_since(std::time::UNIX_EPOCH) {
                        hash.update(&elapsed.as_nanos().to_le_bytes());
                    }
                }
            }
            IrOp::RasterVector { .. } => {
                if vector_hash.is_none() {
                    let document = snapshot.document.as_ref().ok_or_else(|| {
                        PreviewError::Invalid("preview vector document missing".into())
                    })?;
                    let mut value = serde_json::to_value(document.as_ref())?;
                    if let Some(object) = value.as_object_mut() {
                        for key in [
                            "timeline",
                            "name",
                            "annotations",
                            "guides",
                            "recent_colors",
                            "workspaces",
                            "export_profiles",
                            "history_max_mb",
                        ] {
                            object.remove(key);
                        }
                    }
                    let bytes = serde_json::to_vec(&value)?;
                    *vector_hash = Some(ContentHash(xxhash_rust::xxh3::xxh3_128(&bytes)));
                }
                hash.update(&vector_hash.expect("computed vector hash").0.to_le_bytes());
            }
            _ => {}
        }
    }
    Ok((ContentHash(hash.digest128()), dependencies))
}

struct PreviewDecoder {
    // Worker drops before its immutable media lease.
    worker: DecodeWorker,
    ring: SharedRing,
    chunk: Arc<PreviewChunk>,
    converter: Option<YuvConverter>,
    uploaded: Option<(Tick, GpuFrame)>,
}
impl PreviewDecoder {
    fn new(tools: FfmpegTools, chunk: Arc<PreviewChunk>) -> Self {
        let context = &chunk.signature.context;
        let rate = context.frame_rate;
        let (width, height) = context.profile.output_size(context.width, context.height);
        let ring = SharedRing::new(FrameRing::new(6, 1).with_byte_budget(32 * 1024 * 1024));
        let params = SourceParams {
            input: chunk.path().to_owned(),
            width,
            height,
            pix_fmt: PixFmt::for_alpha(context.profile.codec != PreviewCodec::IntraH264),
            pts_kind: PtsKind::Cfr(rate),
            keyframes: KeyframeIndex {
                keyframes: (0..chunk.signature.span.frame_count)
                    .map(|frame| rate.frame_start(frame as i64))
                    .collect(),
            },
        };
        let source = DecodeSource::new(tools, params, ring.clone());
        let source = if context.profile.codec == PreviewCodec::Lossless {
            source.with_software_decoder("libvpx-vp9")
        } else {
            source
        };
        let source = Arc::new(Mutex::new(source));
        Self {
            worker: DecodeWorker::spawn(source, rate),
            ring,
            chunk,
            converter: None,
            uploaded: None,
        }
    }
    fn frame(&mut self, gpu: &GpuContext, time: Tick) -> Option<GpuFrame> {
        let local = self.chunk.local_time(time)?;
        self.worker.steer(local);
        let frame = self.ring.try_frame_covering(local)?;
        if local.0 - frame.pts.0 >= self.chunk.signature.context.frame_rate.ticks_per_frame().0 {
            return None;
        }
        if let Some((pts, texture)) = &self.uploaded {
            if *pts == frame.pts {
                return Some(texture.clone());
            }
        }
        let converter = self
            .converter
            .get_or_insert_with(|| YuvConverter::new(gpu.device()));
        let (width, height) = frame.planes.dims();
        let bucket = crate::graph::ir::TextureDesc { width, height }.bucket();
        let texture = converter.convert_to_size(
            gpu.device(),
            gpu.queue(),
            &frame.planes.as_yuv_planes(),
            Colorimetry {
                matrix: Matrix::Bt709,
                range: Range::Limited,
            },
            bucket,
        );
        let frame_gpu = GpuFrame::new(Arc::new(texture), width, height);
        self.uploaded = Some((frame.pts, frame_gpu.clone()));
        Some(frame_gpu)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::{
        Clip, ClipSource, FrameRate, Sequence, TimelineProject, Track, TrackKind, TICKS_PER_SECOND,
    };
    use photonic_core::Color;
    #[test]
    fn lossless_alpha_profile_decodes_side_channel_and_half_dimensions() {
        let Some(gpu) = GpuContext::request_blocking() else {
            assert!(
                std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
                "required preview GPU unavailable"
            );
            eprintln!("no GPU; alpha preview skipped");
            return;
        };
        let Some(tools) = crate::media::ffmpeg_locate::locate_for_test() else {
            assert!(
                std::env::var_os("PHOTONIC_REQUIRE_GPU").is_none(),
                "required preview FFmpeg unavailable"
            );
            eprintln!("no FFmpeg; alpha preview skipped");
            return;
        };
        let root =
            std::env::temp_dir().join(format!("photonic-preview-alpha-{}", uuid::Uuid::new_v4()));
        let mut project = TimelineProject::new();
        let mut sequence = Sequence::new("alpha", FrameRate::FPS_30, 128, 128);
        let id = sequence.id;
        let mut track = Track::new(TrackKind::Video, "V1");
        track.clips.push(Clip::new(
            ClipSource::SolidColor {
                color: Color {
                    r: 0.25,
                    g: 0.75,
                    b: 0.5,
                    a: 0.5,
                },
            },
            Tick(0),
            Tick(TICKS_PER_SECOND),
        ));
        sequence.video_tracks.push(track);
        project.insert_sequence(sequence);
        project.active_sequence = Some(id);
        let context = PreviewContext {
            sequence: id,
            format_index: 0,
            width: 128,
            height: 128,
            frame_rate: FrameRate::FPS_30,
            quality: PreviewQuality::Full,
            proxy_mode: ProxyMode::ForceOriginal,
            use_proxy: false,
            source_signature: ContentHash(0),
            profile: PreviewProfile {
                codec: PreviewCodec::Lossless,
                quality: 18,
                scale: PreviewScale::Half,
            },
        };
        let span = ChunkSpan::covering(context.frame_rate, Tick(0)).unwrap();
        let hashes = span
            .ticks(context.frame_rate)
            .map(|tick| {
                let compiled = crate::graph::compile::compile_full(
                    &project,
                    id,
                    0,
                    tick,
                    crate::graph::compile::Quality::FULL,
                    None,
                    None,
                    false,
                    None,
                    None,
                );
                compiled.graph.nodes[compiled.graph.output.unwrap().0 as usize].content_hash
            })
            .collect();
        let signature = ChunkSignature::new(context, span, hashes).unwrap();
        let cache = Arc::new(PreviewCache::open(root.clone(), CacheConfig::default()).unwrap());
        let worker = PreviewWorker::new(
            gpu.clone(),
            tools.clone(),
            cache.clone(),
            PlaybackPriorityGate::default(),
        )
        .unwrap();
        worker
            .submit(PreviewChunkRequest {
                snapshot: Arc::new(RenderSnapshot {
                    revision: 0,
                    project: Some(Arc::new(project)),
                    document: None,
                }),
                signature: signature.clone(),
                dependencies: Vec::new(),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let chunk = loop {
            if let Some(chunk) = cache.lookup(&signature) {
                break chunk;
            }
            let status = worker.status();
            assert!(
                !status.chunks.iter().any(|c| c.state == ChunkState::Failed),
                "alpha render failed: {:?}",
                status
            );
            assert!(
                Instant::now() < deadline,
                "alpha render timed out: {:?}",
                status
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut decoder = PreviewDecoder::new(tools, chunk);
        let deadline = Instant::now() + Duration::from_secs(10);
        let decoded = loop {
            if let Some(frame) = decoder.frame(&gpu, Tick(0)) {
                break frame;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((decoded.width, decoded.height), (64, 64));
        let pixels = crate::graph::eval::read_texture_rgba16f(&gpu, &decoded.texture, 64, 64);
        assert!(
            pixels.iter().all(|pixel| (pixel[3] - 0.5).abs() < 0.01),
            "VP9 alpha must survive cached decode, not become opaque"
        );
        drop(decoder);
        worker.shutdown();
        drop(worker);
        drop(cache);
        let _ = std::fs::remove_dir_all(root);
    }
}
