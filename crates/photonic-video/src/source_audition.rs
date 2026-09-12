//! Explicit source audition on the existing monitor, with its own audio clock.
//! The caller pauses program playback first. This helper never changes the
//! document or its sequence controller and releases its output on stop.

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use photonic_core::timeline::{
    AssetId, AssetKind, AssetSource, Clip, ClipAudio, ClipSource, FrameRate, MediaAsset, Sequence,
    SequenceId, Tick, TimelineProject, Track, TrackKind,
};

use crate::audio::{audio_ring, AudioEngine, AudioEngineError, StereoMeter};
use crate::media::ffmpeg_locate::FfmpegTools;
use crate::playback::PlaybackController;
use crate::session::{spawn_source_audio_feeder, AudioFeeder};

#[derive(Debug, thiserror::Error)]
pub enum SourceAuditionError {
    #[error("source marks must define a positive range within this asset")]
    InvalidRange,
    #[error("source metadata is still loading; audition is available after probing")]
    NotReady,
    #[error("audition requires a video or audio file")]
    UnsupportedSource,
    #[error("source file is offline")]
    Offline,
    #[error("FFmpeg is unavailable for source audio")]
    MissingFfmpeg,
    #[error("source audio could not be decoded")]
    Decode,
    #[error("source audio did not become ready in time")]
    PrefillTimeout,
    #[error("source audio output is unavailable: {0}")]
    Audio(#[from] AudioEngineError),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SourceAuditionStatus {
    pub asset: AssetId,
    pub range: (Tick, Tick),
    pub playhead: Tick,
    pub playing: bool,
    pub has_audio: bool,
}

/// Pure source-to-mixer mapping, reusable by headless tests and the service.
pub struct SourceAuditionPlan {
    pub project: Arc<TimelineProject>,
    pub sequence: SequenceId,
    pub range: (Tick, Tick),
    pub frame_rate: FrameRate,
    pub has_audio: bool,
}

pub fn plan_source_audition(
    asset: &MediaAsset,
    requested: (Tick, Tick),
) -> Result<SourceAuditionPlan, SourceAuditionError> {
    if !matches!(asset.kind, AssetKind::Video | AssetKind::Audio)
        || !matches!(asset.source, AssetSource::File { .. })
    {
        return Err(SourceAuditionError::UnsupportedSource);
    }
    let probe = asset.probe.as_ref().ok_or(SourceAuditionError::NotReady)?;
    let source_bounds = asset.subclip_range.unwrap_or((Tick::ZERO, probe.duration));
    let start = requested.0.max(source_bounds.0).max(Tick::ZERO);
    let end = requested.1.min(source_bounds.1).min(probe.duration);
    if end <= start {
        return Err(SourceAuditionError::InvalidRange);
    }
    let frame_rate = probe
        .video
        .as_ref()
        .map(|video| video.frame_rate)
        .filter(|rate| rate.num > 0 && rate.den > 0)
        .unwrap_or(FrameRate::FPS_30);
    let mut sequence = Sequence::new("Source audition", frame_rate, 1920, 1080);
    // Source audition is raw media, without the program's default limiter or
    // its lookahead delay. Output remains stereo through the same mixer.
    sequence.audio_master.fx_chain.clear();
    let has_audio = asset.kind == AssetKind::Audio || probe.audio.is_some();
    if has_audio {
        let mut track = Track::new(TrackKind::Audio, "Source audio");
        let mut clip = Clip::new(ClipSource::Asset { asset: asset.id }, start, end - start);
        clip.source_in = start;
        clip.audio = Some(ClipAudio::new());
        track.clips.push(clip);
        sequence.audio_tracks.push(track);
    }
    let mut project = TimelineProject::new();
    project.media.assets.insert(asset.id, asset.clone());
    let sequence = project.insert_sequence(sequence);
    Ok(SourceAuditionPlan {
        project: Arc::new(project),
        sequence,
        range: (start, end),
        frame_rate,
        has_audio,
    })
}

/// Gate the final interleaved stereo block on the source half-open range.
/// Runs after all mixing so no effect tail can leak past Out.
pub(crate) fn bound_source_output(block: &mut [f32], start: Tick, end: Tick, sample_rate: u32) {
    let remaining = (end.0 as i128 - start.0 as i128).max(0);
    let ticks = photonic_core::timeline::TICKS_PER_SECOND as i128;
    let frames = (remaining * sample_rate as i128 + ticks - 1) / ticks;
    let samples = (frames * 2).min(block.len() as i128) as usize;
    block[samples..].fill(0.0);
}

/// A bounded source playback session. Normal program play/seek cancels this
/// helper before using the program audio output again.
pub struct SourceAudition {
    asset: AssetId,
    range: (Tick, Tick),
    controller: PlaybackController,
    audio: AudioEngine,
    feeder: Option<AudioFeeder>,
    meter: Arc<ArcSwapOption<StereoMeter>>,
    has_audio: bool,
}

impl SourceAudition {
    /// Start source-clock playback; audio-bearing sources require real output.
    /// Silent videos use a software clock and report has_audio=false.
    pub fn start(
        asset: &MediaAsset,
        range: (Tick, Tick),
        tools: Option<FfmpegTools>,
    ) -> Result<Self, SourceAuditionError> {
        let plan = plan_source_audition(asset, range)?;
        let AssetSource::File { path, .. } = &asset.source else {
            return Err(SourceAuditionError::UnsupportedSource);
        };
        if !path.is_file() {
            return Err(SourceAuditionError::Offline);
        }
        if plan.has_audio && tools.is_none() {
            return Err(SourceAuditionError::MissingFfmpeg);
        }
        let mut audio = AudioEngine::new();
        let mut controller = PlaybackController::new(plan.frame_rate);
        controller.seek(plan.range.0);
        let meter = Arc::new(ArcSwapOption::empty());
        let feeder = if plan.has_audio {
            let (producer, consumer, _) = audio_ring();
            let sample_rate = audio.prepare(consumer)?;
            let feeder = spawn_source_audio_feeder(
                plan.project,
                plan.sequence,
                plan.range.0,
                sample_rate,
                producer,
                tools,
                meter.clone(),
                Arc::new(ArcSwapOption::empty()),
                Arc::new(AtomicU32::new(0)),
                plan.range.1,
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while feeder.prefill_state() == 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            match feeder.prefill_state() {
                1 => {}
                2 => return Err(SourceAuditionError::Decode),
                _ => return Err(SourceAuditionError::PrefillTimeout),
            }
            controller.play_audio(audio.clock());
            audio.play_prepared()?;
            Some(feeder)
        } else {
            controller.play_soft();
            None
        };
        Ok(Self {
            asset: asset.id,
            range: plan.range,
            controller,
            audio,
            feeder,
            meter,
            has_audio: plan.has_audio,
        })
    }

    /// Advance/status the independent source clock and stop exactly at Out.
    pub fn poll(&mut self) -> SourceAuditionStatus {
        if self.controller.playhead() >= self.range.1 {
            self.stop();
            self.controller.seek(self.range.1);
        }
        self.status()
    }

    pub fn status(&self) -> SourceAuditionStatus {
        SourceAuditionStatus {
            asset: self.asset,
            range: self.range,
            playhead: self.controller.playhead().clamp(self.range.0, self.range.1),
            playing: self.controller.is_playing(),
            has_audio: self.has_audio,
        }
    }

    pub fn meter(&self) -> Option<Arc<StereoMeter>> {
        self.meter.load_full()
    }

    pub fn stop(&mut self) {
        self.controller.pause();
        self.audio.stop();
        self.feeder = None;
        self.meter.store(None);
    }
}

impl Drop for SourceAudition {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photonic_core::timeline::MediaProbe;

    #[test]
    fn source_plan_maps_audio_at_source_in_and_respects_subclip_bounds() {
        let mut asset = MediaAsset::from_file(AssetKind::Audio, "speech.wav");
        asset.probe = Some(MediaProbe::basic(Tick::from_seconds(20), "wav", "pcm"));
        asset.subclip_range = Some((Tick::from_seconds(3), Tick::from_seconds(9)));
        let original = asset.clone();
        let plan =
            plan_source_audition(&asset, (Tick::from_seconds(1), Tick::from_seconds(12))).unwrap();
        assert_eq!(plan.range, (Tick::from_seconds(3), Tick::from_seconds(9)));
        let clip = &plan.project.sequences[&plan.sequence].audio_tracks[0].clips[0];
        assert_eq!(clip.start, Tick::from_seconds(3));
        assert_eq!(clip.source_in, clip.start);
        assert_eq!(clip.end(), Tick::from_seconds(9));
        assert!(clip.audio.is_some());
        assert_eq!(asset, original);
    }

    #[test]
    fn source_audition_audio_uses_marked_offset_and_stops_at_out() {
        use crate::audio::{
            mixer::{ClipVoice, Mixer, TrackVoice},
            BLOCK_FRAMES, CHANNELS,
        };
        use crate::playback::pcm::FfmpegPcmSource;
        use photonic_core::timeline::TICKS_PER_SECOND;
        let Some(tools) = crate::media::ffmpeg_locate::locate_for_test() else {
            eprintln!("FFmpeg unavailable; source audio fixture skipped");
            return;
        };
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beep_flash.wav");
        let mut asset = MediaAsset::from_file(AssetKind::Audio, path.clone());
        asset.probe = Some(MediaProbe::basic(Tick::from_seconds(2), "wav", "pcm"));
        // Fixture has a 5ms beep at 1.0s. Mark 0.99..1.002s: beep must be
        // heard 480 samples into this audition, never a second after it.
        let plan = plan_source_audition(
            &asset,
            (
                Tick(TICKS_PER_SECOND * 99 / 100),
                Tick(TICKS_PER_SECOND * 1002 / 1000),
            ),
        )
        .unwrap();
        let seq = &plan.project.sequences[&plan.sequence];
        let track = &seq.audio_tracks[0];
        let clip = &track.clips[0];
        let mut source = FfmpegPcmSource::spawn(&tools, &path, clip.source_in, 48_000).unwrap();
        let mut mixer = Mixer::new(48_000);
        mixer.set_declick(crate::audio::mixer::DeclickConfig {
            enabled: false,
            ..Default::default()
        });
        let mut samples = Vec::new();
        for index in 0..(2048usize.div_ceil(BLOCK_FRAMES)) {
            let elapsed = Tick((index * BLOCK_FRAMES) as i64 * (TICKS_PER_SECOND / 48_000));
            let mut voices = [TrackVoice {
                id: track.id,
                audio: track.audio.as_ref().unwrap(),
                clips: vec![ClipVoice {
                    audio: clip.audio.as_ref().unwrap(),
                    elapsed,
                    remaining: clip.duration.saturating_sub(elapsed).max(Tick::ZERO),
                    source: &mut source,
                }],
            }];
            let mut block = vec![0.; BLOCK_FRAMES * CHANNELS];
            mixer.render_block(
                clip.start + elapsed,
                &mut voices,
                &seq.audio_master,
                &mut block,
            );
            bound_source_output(&mut block, clip.start + elapsed, plan.range.1, 48_000);
            samples.extend(block);
        }
        let peak = |range: std::ops::Range<usize>| {
            samples[range.start * 2..range.end * 2]
                .iter()
                .fold(0f32, |peak, sample| peak.max(sample.abs()))
        };
        assert!(
            peak(0..400) < 0.001,
            "pre-beep source region must be silent"
        );
        assert!(
            peak(480..570) > 0.05,
            "marked source beep must reach output"
        );
        assert!(peak(576..1900) < 0.001, "audio must stop at marked Out");
    }

    #[test]
    fn source_feeder_prefills_real_pcm_and_cuts_beep_at_out_without_device() {
        let Some(tools) = crate::media::ffmpeg_locate::locate_for_test() else {
            eprintln!("FFmpeg unavailable; source feeder fixture skipped");
            return;
        };
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/beep_flash.wav");
        let mut asset = MediaAsset::from_file(AssetKind::Audio, path);
        asset.probe = Some(MediaProbe::basic(Tick::from_seconds(2), "wav", "pcm"));
        let ticks = photonic_core::timeline::TICKS_PER_SECOND;
        let plan =
            plan_source_audition(&asset, (Tick(ticks * 99 / 100), Tick(ticks * 1002 / 1000)))
                .unwrap();
        let (producer, mut consumer, _) = audio_ring();
        let feeder = spawn_source_audio_feeder(
            plan.project,
            plan.sequence,
            plan.range.0,
            48_000,
            producer,
            Some(tools),
            Arc::new(ArcSwapOption::empty()),
            Arc::new(ArcSwapOption::empty()),
            Arc::new(AtomicU32::new(0)),
            plan.range.1,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !feeder.has_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            feeder.has_finished(),
            "bounded source feeder should finish after Out"
        );
        assert_eq!(feeder.prefill_state(), 1);
        let mut samples = vec![0.; 2048];
        consumer.fill(&mut samples);
        assert!(
            samples[480 * 2..570 * 2]
                .iter()
                .any(|sample| sample.abs() > 0.05),
            "ready means real marked PCM was queued"
        );
        assert!(
            samples[576 * 2..].iter().all(|sample| sample.abs() < 0.001),
            "final partial block must be silent from exact Out"
        );
    }

    #[test]
    fn unprobed_or_empty_source_ranges_refuse_before_opening_audio() {
        let mut asset = MediaAsset::from_file(AssetKind::Video, "movie.mp4");
        assert!(matches!(
            plan_source_audition(&asset, (Tick::ZERO, Tick::from_seconds(2))),
            Err(SourceAuditionError::NotReady)
        ));
        asset.probe = Some(MediaProbe::basic(Tick::from_seconds(1), "mp4", "h264"));
        assert!(matches!(
            plan_source_audition(&asset, (Tick::from_seconds(2), Tick::from_seconds(3))),
            Err(SourceAuditionError::InvalidRange)
        ));
        let plan = plan_source_audition(&asset, (Tick::ZERO, Tick::from_seconds(1))).unwrap();
        assert!(!plan.has_audio);
        assert!(plan.project.sequences[&plan.sequence]
            .audio_tracks
            .is_empty());
    }
}
