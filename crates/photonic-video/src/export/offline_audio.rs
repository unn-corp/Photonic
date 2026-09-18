//! Offline sequence-audio mix for export (K-0.7 / 09 §7 / 31 §6).
//!
//! Renders the same wall-clock-free [`Mixer::render_block`] path used by
//! interactive playback (PA-10), then optionally applies a constant
//! [`LoudnessTarget`] gain (two-pass: measure integrated LUFS + true peak,
//! then scale — never a time-varying compressor).
//!
//! The measurement half is **E-2** (32 §2 / 31 §5): it runs as
//! [`analysis::analyze_loudness`], is keyed by the PCM content hash and cached,
//! so the "analyse → cache → apply" shape 31 §6.2 mandates is the actual code
//! shape here, and a re-render of unchanged audio skips the measurement pass.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use photonic_core::timeline::{
    AssetSource, Clip, ClipAudio, ClipId, ClipSource, Ratio, SequenceId, SpeedMap, Tick,
    TimelineProject, TICKS_PER_SECOND,
};

use super::presets::LoudnessTarget;
use super::render_loop::ExportError;
use crate::audio::mixer::{ClipVoice, Mixer, PcmSource, TrackVoice};
use crate::audio::{BLOCK_FRAMES, CHANNELS};
use crate::graph::analysis::{self, AnalysisCache, AnalysisResult};
use crate::media::ffmpeg_locate::FfmpegTools;
use crate::playback::pcm::{read_pcm_window, TimeWarpPcmSource};
use crate::playback::FfmpegPcmSource;

/// Default export mix rate when a sequence does not declare one (48 kHz).
pub const DEFAULT_EXPORT_SAMPLE_RATE: u32 = 48_000;

/// Render interleaved stereo f32le PCM for `[start, end)` on `sequence`, at
/// the sequence's `audio_sample_rate` (falling back to
/// [`DEFAULT_EXPORT_SAMPLE_RATE`]). When `tools` is `None`, every block is
/// silence (no decode sidecars) — still a valid length-correct buffer so a
/// video-only machine can mux silent audio if the preset asks for it.
///
/// When `loudness` is `Some`, a constant gain is computed and applied so the
/// integrated LUFS hits the target without breaching `true_peak_dbtp` (31 §6.2).
pub fn render_export_audio(
    project: &TimelineProject,
    sequence: SequenceId,
    start: Tick,
    end: Tick,
    tools: Option<&FfmpegTools>,
    loudness: Option<&LoudnessTarget>,
) -> Result<Vec<f32>, ExportError> {
    render_export_audio_filtered(project, sequence, start, end, tools, loudness, None)
}

/// K-D4: render one audio track as a stem (`only_track = Some`). `None` mixes
/// every enabled audio track (same as [`render_export_audio`]).
pub fn render_export_audio_filtered(
    project: &TimelineProject,
    sequence: SequenceId,
    start: Tick,
    end: Tick,
    tools: Option<&FfmpegTools>,
    loudness: Option<&LoudnessTarget>,
    only_track: Option<photonic_core::timeline::TrackId>,
) -> Result<Vec<f32>, ExportError> {
    if end <= start {
        return Ok(Vec::new());
    }
    let seq = project.sequences.get(&sequence).ok_or_else(|| {
        ExportError::Resolve(format!("sequence {sequence} not found for audio export"))
    })?;
    let sample_rate = if project.settings.audio_sample_rate > 0 {
        project.settings.audio_sample_rate
    } else {
        DEFAULT_EXPORT_SAMPLE_RATE
    };

    let total_frames = ticks_to_frames(end - start, sample_rate);
    if total_frames == 0 {
        return Ok(Vec::new());
    }

    let block_ticks =
        Tick(((BLOCK_FRAMES as i128 * TICKS_PER_SECOND as i128) / sample_rate as i128) as i64)
            .max(Tick(1));

    let mut out_pcm = Vec::with_capacity(total_frames * CHANNELS);
    let mut block = vec![0f32; BLOCK_FRAMES * CHANNELS];
    let mut mixer = Mixer::new(sample_rate);
    let default_clip_audio = ClipAudio::new();
    let mut pcm: HashMap<ClipId, Box<dyn PcmSource>> = HashMap::new();
    let mut t = start;
    let mut frames_left = total_frames;

    while frames_left > 0 {
        let frames_this = frames_left.min(BLOCK_FRAMES);
        // Which clips sound at t? Audio tracks only (matches interactive feeder).
        let active: Vec<(&photonic_core::timeline::Track, &Clip)> = seq
            .audio_tracks
            .iter()
            .filter(|track| track.enabled && track.audio.is_some())
            .filter(|track| only_track.map(|id| track.id == id).unwrap_or(true))
            .flat_map(|track| {
                track
                    .clips
                    .iter()
                    .filter(|clip| {
                        clip.enabled
                            && clip.start <= t
                            && t < clip.end()
                            && matches!(clip.source, ClipSource::Asset { .. })
                    })
                    .map(move |clip| (track, clip))
            })
            .collect();

        if let Some(tools) = tools {
            for (_, clip) in &active {
                if pcm.contains_key(&clip.id) {
                    continue;
                }
                let ClipSource::Asset { asset } = clip.source else {
                    continue;
                };
                let Some(AssetSource::File { path, .. }) =
                    project.media.assets.get(&asset).map(|a| &a.source)
                else {
                    continue;
                };
                let (offset, stream) = clip
                    .audio
                    .as_ref()
                    .map(|a| (a.offset, a.stream))
                    .unwrap_or((Tick::ZERO, None));
                let identity = matches!(clip.speed, SpeedMap::Constant(r) if r == Ratio::ONE);
                if identity {
                    let src_pos = crate::playback::pcm::source_seek_with_offset(
                        clip.source_in,
                        t,
                        clip.start,
                        offset,
                    );
                    if let Ok(source) =
                        FfmpegPcmSource::spawn_stream(tools, path, src_pos, sample_rate, stream)
                    {
                        pcm.insert(clip.id, Box::new(source));
                    }
                } else {
                    let (lo, hi) = clip.speed.source_delta_range(clip.duration);
                    let source_start =
                        Tick((clip.source_in.0 - offset.0 + lo.floor() as i64).max(0));
                    let source_end = (clip.source_in.0 - offset.0) as f64 + hi.ceil();
                    let frames = ((source_end - source_start.0 as f64).max(0.0)
                        * sample_rate as f64
                        / TICKS_PER_SECOND as f64)
                        .ceil() as usize
                        + 2;
                    if let Ok(mut decoder) = FfmpegPcmSource::spawn_stream(
                        tools,
                        path,
                        source_start,
                        sample_rate,
                        stream,
                    ) {
                        let window = read_pcm_window(&mut decoder, frames);
                        pcm.insert(
                            clip.id,
                            Box::new(TimeWarpPcmSource::new(
                                window,
                                sample_rate,
                                source_start,
                                clip.source_in,
                                offset,
                                clip.speed.clone(),
                                t - clip.start,
                            )),
                        );
                    }
                }
            }
        }
        let active_ids: HashSet<ClipId> = active.iter().map(|(_, c)| c.id).collect();
        pcm.retain(|id, _| active_ids.contains(id));

        let mut refs: HashMap<ClipId, &mut Box<dyn PcmSource>> =
            pcm.iter_mut().map(|(id, src)| (*id, src)).collect();
        let mut voices: Vec<TrackVoice<'_>> = Vec::new();
        for track in seq
            .audio_tracks
            .iter()
            .filter(|track| track.enabled && track.audio.is_some())
            .filter(|track| only_track.map(|id| track.id == id).unwrap_or(true))
        {
            let track_audio = track.audio.as_ref().expect("filtered to Some");
            let mut clips: Vec<ClipVoice<'_>> = Vec::new();
            for (_, clip) in active.iter().filter(|(tr, _)| tr.id == track.id) {
                if let Some(source) = refs.remove(&clip.id) {
                    clips.push(ClipVoice {
                        audio: clip.audio.as_ref().unwrap_or(&default_clip_audio),
                        elapsed: t - clip.start,
                        remaining: clip.end() - t,
                        source: source.as_mut(),
                    });
                }
            }
            voices.push(TrackVoice {
                id: track.id,
                audio: track_audio,
                clips,
            });
        }

        block.fill(0.0);
        mixer.render_block(t, &mut voices, &seq.audio_master, &mut block);
        out_pcm.extend_from_slice(&block[..frames_this * CHANNELS]);
        frames_left -= frames_this;
        t = t + block_ticks;
    }

    if let Some(target) = loudness {
        // Analyse (cached, E-2) → apply. Two statements, in that order, because
        // 31 §6.2 says the measurement is a job and the gain is what the render
        // consumes.
        let measured = measure_loudness_cached(&out_pcm, sample_rate);
        apply_loudness_gain(&mut out_pcm, &measured, target);
    }
    Ok(out_pcm)
}

/// K-D4: sidecar path for a stem export next to the main output.
/// Stems are always written as little-endian IEEE float WAV (`.wav`) so they
/// need no second encode pass and stay independent of the main container.
pub fn stem_output_path(main: &std::path::Path, track_name: &str) -> std::path::PathBuf {
    let stem = main
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export");
    let safe: String = track_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "track".into()
    } else {
        safe
    };
    let parent = main.parent().unwrap_or_else(|| std::path::Path::new("."));
    parent.join(format!("{stem}_stem_{safe}.wav"))
}

/// Write interleaved stereo f32 PCM as a WAV file (IEEE float format 3).
pub fn write_stem_wav(
    path: &std::path::Path,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), ExportError> {
    let channels: u16 = 2;
    let bits: u16 = 32;
    let data_bytes = samples.len() * 4;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits) / 8;
    let block_align = channels * bits / 8;
    let mut out = Vec::with_capacity(44 + data_bytes);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM/float chunk size
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out).map_err(|e| ExportError::Encode(super::encoder::EncodeError::Io(e)))
}

/// K-D4: render one stem WAV per enabled audio track beside `main_out`.
/// Returns the written paths (empty when the sequence has no audio tracks).
/// Tools may be `None` (silent PCM of correct length — still a real file).
pub fn write_stems_for_export(
    project: &TimelineProject,
    sequence: SequenceId,
    start: Tick,
    end: Tick,
    main_out: &std::path::Path,
    tools: Option<&FfmpegTools>,
    loudness: Option<&LoudnessTarget>,
) -> Result<Vec<std::path::PathBuf>, ExportError> {
    let seq = project.sequences.get(&sequence).ok_or_else(|| {
        ExportError::Resolve(format!("sequence {sequence} not found for stem export"))
    })?;
    let sample_rate = if project.settings.audio_sample_rate > 0 {
        project.settings.audio_sample_rate
    } else {
        DEFAULT_EXPORT_SAMPLE_RATE
    };
    let mut written = Vec::new();
    for track in seq
        .audio_tracks
        .iter()
        .filter(|t| t.enabled && t.audio.is_some())
    {
        let pcm = render_export_audio_filtered(
            project,
            sequence,
            start,
            end,
            tools,
            loudness,
            Some(track.id),
        )?;
        let path = stem_output_path(main_out, &track.name);
        write_stem_wav(&path, &pcm, sample_rate)?;
        written.push(path);
    }
    Ok(written)
}

/// Convert a tick duration to a sample-frame count at `sample_rate` (exact
/// integer arithmetic for rates that divide `TICKS_PER_SECOND` cleanly).
fn ticks_to_frames(duration: Tick, sample_rate: u32) -> usize {
    let d = duration.0.max(0) as i128;
    let n = (d * sample_rate as i128) / TICKS_PER_SECOND as i128;
    n.max(0) as usize
}

/// Bound on the shared measurement cache. Each entry is a handful of bytes and
/// one export contributes one entry, so this is a leak guard rather than a
/// working-set policy; on overflow the whole map is dropped (a re-measure, not
/// a wrong answer). A byte-budgeted LRU belongs with 32 §5.2's budget work.
const MAX_CACHED_MEASUREMENTS: usize = 64;

/// Process-wide loudness measurement cache (31 §6.2 step 4: "cache the
/// measurement by content hash so a re-render is free"). Export jobs are
/// separate invocations, so a job-scoped cache could never hit; the reuse this
/// exists for is *the same range exported again* — a preset change, a retry, a
/// second container — which must not re-run K-weighting over the whole mix.
fn shared_loudness_cache() -> &'static Mutex<AnalysisCache> {
    static CACHE: OnceLock<Mutex<AnalysisCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(AnalysisCache::new()))
}

/// Measure `pcm` through the E-2 substrate, reusing a cached verdict when the
/// content hash hits.
///
/// The lock is taken twice and never held across the measurement, so two
/// concurrent exports of *different* audio do not serialise on each other (the
/// worst case is a duplicated measurement of identical audio, which is correct,
/// just not free). A poisoned mutex degrades to measuring uncached rather than
/// failing the export.
fn measure_loudness_cached(pcm: &[f32], sample_rate: u32) -> AnalysisResult {
    let key = analysis::loudness_key(pcm, sample_rate);
    if let Some(hit) = shared_loudness_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(key).cloned())
    {
        return hit;
    }
    let measured = analysis::analyze_loudness(pcm, sample_rate);
    if let Ok(mut cache) = shared_loudness_cache().lock() {
        if cache.len() >= MAX_CACHED_MEASUREMENTS {
            cache.clear();
        }
        cache.insert(key, measured.clone());
    }
    measured
}

/// Second pass of 31 §6.2: turn an [`AnalysisResult::Loudness`] into a constant
/// gain that hits `target.integrated_lufs`, reduced if it would breach
/// `true_peak_dbtp`. Pure in the measurement — it never touches the DSP itself,
/// which is what makes the cached and uncached paths provably identical.
/// Silence (non-finite LUFS) is left untouched.
fn apply_loudness_gain(pcm: &mut [f32], measured: &AnalysisResult, target: &LoudnessTarget) {
    if pcm.is_empty() {
        return;
    }
    let AnalysisResult::Loudness {
        integrated_lufs: measured,
        true_peak_dbtp: peak,
        ..
    } = *measured
    else {
        debug_assert!(false, "loudness gain needs an AnalysisResult::Loudness");
        return;
    };
    if !measured.is_finite() {
        return;
    }
    let mut gain_db = f64::from(target.integrated_lufs) - measured;
    let peak_after = peak as f64 + gain_db;
    if peak_after > f64::from(target.true_peak_dbtp) {
        // Pull the gain down so true peak lands on the ceiling; report via
        // tracing (callers can surface EngineStatus diagnostics later).
        let reduced = f64::from(target.true_peak_dbtp) - peak as f64;
        tracing::info!(
            target: "photonic_video::export",
            measured_lufs = measured,
            wanted_lufs = target.integrated_lufs,
            true_peak_dbtp = peak,
            ceiling_dbtp = target.true_peak_dbtp,
            gain_db_before = gain_db,
            gain_db_after = reduced,
            "loudness gain reduced to honour true-peak ceiling"
        );
        gain_db = reduced;
    }
    let g = 10f64.powf(gain_db / 20.0) as f32;
    if (g - 1.0).abs() < 1e-6 {
        return;
    }
    for s in pcm.iter_mut() {
        *s *= g;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::dsp::loudness::{integrated_lufs, true_peak_dbtp};
    use photonic_core::timeline::{FrameRate, MasterBus, Sequence, Track, TrackAudio, TrackKind};

    fn empty_audio_seq() -> (TimelineProject, SequenceId) {
        let mut project = TimelineProject::new();
        project.settings.audio_sample_rate = 48_000;
        let mut seq = Sequence::new("seq", FrameRate::FPS_30, 640, 360);
        let mut t = Track::new(TrackKind::Audio, "A1");
        t.audio = Some(TrackAudio::new());
        seq.audio_tracks.push(t);
        seq.audio_master = MasterBus::default();
        let id = seq.id;
        project.insert_sequence(seq);
        (project, id)
    }

    #[test]
    fn stem_output_path_sanitizes_track_name() {
        let p = stem_output_path(std::path::Path::new("/tmp/out.mp4"), "A1 Dialogue!");
        assert_eq!(
            p,
            std::path::PathBuf::from("/tmp/out_stem_A1_Dialogue_.wav")
        );
    }

    #[test]
    fn write_stems_for_export_writes_one_wav_per_audio_track() {
        let (project, sid) = empty_audio_seq();
        // Replace with two named audio tracks so the count is exact.
        let mut project = project;
        {
            let seq = project.sequences.get_mut(&sid).unwrap();
            seq.audio_tracks.clear();
            let mut a = Track::new(TrackKind::Audio, "A1 Dialogue");
            a.audio = Some(TrackAudio::new());
            let mut b = Track::new(TrackKind::Audio, "A2 Music");
            b.audio = Some(TrackAudio::new());
            seq.audio_tracks.push(a);
            seq.audio_tracks.push(b);
        }
        let dir = std::env::temp_dir().join(format!("photonic-stems-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let main = dir.join("show.mp4");
        let start = Tick::ZERO;
        let end = Tick::from_seconds(1);
        // tools=None → silent PCM of correct length (real shipped path).
        let paths = write_stems_for_export(&project, sid, start, end, &main, None, None)
            .expect("write stems");
        assert_eq!(paths.len(), 2, "one stem per audio track");
        for p in &paths {
            assert!(p.exists(), "stem missing: {}", p.display());
            assert!(
                p.extension().and_then(|e| e.to_str()) == Some("wav"),
                "stems must be wav: {}",
                p.display()
            );
            let bytes = std::fs::read(p).unwrap();
            assert!(bytes.starts_with(b"RIFF"), "WAV header");
            assert!(bytes.len() > 44, "payload present");
        }
        // Cleanup.
        for p in paths {
            let _ = std::fs::remove_file(p);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_export_audio_silence_has_correct_length() {
        let (project, sid) = empty_audio_seq();
        // 1 second at 48 kHz → 48000 frames × 2 channels.
        let start = Tick(0);
        let end = Tick(TICKS_PER_SECOND);
        let pcm = render_export_audio(&project, sid, start, end, None, None).unwrap();
        assert_eq!(pcm.len(), 48_000 * CHANNELS);
        assert!(pcm.iter().all(|&s| s == 0.0));
    }

    /// Stereo 997 Hz tone at `amp`, the buffer both loudness tests measure.
    fn tone(amp: f32, seconds: f64, fs: u32) -> Vec<f32> {
        let frames = (fs as f64 * seconds) as usize;
        let mut pcm = vec![0.0f32; frames * CHANNELS];
        for f in 0..frames {
            let s = (2.0 * std::f64::consts::PI * 997.0 * f as f64 / fs as f64).sin() as f32 * amp;
            for c in 0..CHANNELS {
                pcm[f * CHANNELS + c] = s;
            }
        }
        pcm
    }

    /// Measure-then-apply, as one call, the way `render_export_audio` does it.
    fn normalize(pcm: &mut [f32], fs: u32, target: &LoudnessTarget) {
        let measured = measure_loudness_cached(pcm, fs);
        apply_loudness_gain(pcm, &measured, target);
    }

    /// The pre-E-2 implementation, transcribed verbatim from the two-pass
    /// `apply_loudness_gain` this change replaced (git 8b6e572, offline_audio.rs
    /// :165-199) — the oracle for "re-plumbing, not a retune".
    fn legacy_apply_loudness_gain(pcm: &mut [f32], sample_rate: u32, target: &LoudnessTarget) {
        use crate::audio::dsp::loudness::{integrated_lufs, true_peak_dbtp};
        if pcm.is_empty() {
            return;
        }
        let measured = integrated_lufs(pcm, sample_rate);
        if !measured.is_finite() {
            return;
        }
        let mut gain_db = f64::from(target.integrated_lufs) - measured;
        let peak = true_peak_dbtp(pcm, sample_rate);
        let peak_after = peak as f64 + gain_db;
        if peak_after > f64::from(target.true_peak_dbtp) {
            gain_db = f64::from(target.true_peak_dbtp) - peak as f64;
        }
        let g = 10f64.powf(gain_db / 20.0) as f32;
        if (g - 1.0).abs() < 1e-6 {
            return;
        }
        for s in pcm.iter_mut() {
            *s *= g;
        }
    }

    #[test]
    fn apply_loudness_on_silence_is_noop() {
        let mut pcm = vec![0.0f32; 48_000 * CHANNELS];
        let target = LoudnessTarget {
            integrated_lufs: -14.0,
            true_peak_dbtp: -1.0,
        };
        normalize(&mut pcm, 48_000, &target);
        assert!(pcm.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn apply_loudness_raises_a_quiet_tone_toward_target() {
        // 1 kHz stereo sine at low amplitude for 1 s @ 48 kHz.
        let fs = 48_000u32;
        let mut pcm = tone(0.05, 1.0, fs);
        let before = integrated_lufs(&pcm, fs);
        assert!(before.is_finite() && before < -20.0, "before={before}");
        let target = LoudnessTarget {
            integrated_lufs: -23.0,
            true_peak_dbtp: -1.0,
        };
        normalize(&mut pcm, fs, &target);
        let after = integrated_lufs(&pcm, fs);
        // Should land near −23 LUFS (within ~1.5 LU of measurement noise).
        assert!(
            (after - (-23.0)).abs() < 1.5,
            "after={after}, before={before}"
        );
    }

    /// E-2 re-plumbing must not move a single sample: the substrate-routed path
    /// and the pre-E-2 two-pass must agree **bit for bit**, on a gain-limited
    /// case, a ceiling-limited case, and silence.
    #[test]
    fn e2_routed_loudness_is_bit_identical_to_the_legacy_two_pass() {
        /// Which arm of 31 §6.2 a case is meant to exercise.
        #[derive(Debug, PartialEq)]
        enum Branch {
            /// Gain set purely by the LUFS delta.
            Lufs,
            /// Gain pulled back to honour the true-peak ceiling.
            Ceiling,
            /// Non-finite measurement — buffer left untouched.
            Silent,
        }
        let fs = 48_000u32;
        let broadcast = LoudnessTarget {
            integrated_lufs: -23.0,
            true_peak_dbtp: -1.0,
        };
        let streaming = LoudnessTarget {
            integrated_lufs: -14.0,
            true_peak_dbtp: -1.0,
        };
        // High crest factor: quiet enough to want a big boost, peaky enough
        // that the boost would breach the ceiling.
        let mut clicky = tone(0.02, 1.0, fs);
        for f in (0..fs as usize).step_by(4_800) {
            for c in 0..CHANNELS {
                clicky[f * CHANNELS + c] = 0.95;
            }
        }
        let cases: [(&str, Vec<f32>, LoudnessTarget, Branch); 3] = [
            ("quiet tone", tone(0.05, 1.0, fs), broadcast, Branch::Lufs),
            ("peaky clicks", clicky, streaming, Branch::Ceiling),
            (
                "silence",
                vec![0.0f32; fs as usize * CHANNELS],
                streaming,
                Branch::Silent,
            ),
        ];
        for (name, source, target, expected) in cases {
            let mut legacy = source.clone();
            legacy_apply_loudness_gain(&mut legacy, fs, &target);
            let mut routed = source.clone();
            normalize(&mut routed, fs, &target);
            assert_eq!(routed.len(), legacy.len(), "{name}: length");
            assert!(
                routed
                    .iter()
                    .zip(&legacy)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "{name}: E-2 path diverged from the legacy two-pass"
            );
            // …and the case really did exercise the branch it claims to, with
            // the branch condition derived from the measurement, not asserted
            // as a literal.
            let peak = true_peak_dbtp(&source, fs);
            let lufs = integrated_lufs(&source, fs);
            let branch = if !lufs.is_finite() {
                Branch::Silent
            } else if peak as f64 + (f64::from(target.integrated_lufs) - lufs)
                > f64::from(target.true_peak_dbtp)
            {
                Branch::Ceiling
            } else {
                Branch::Lufs
            };
            assert_eq!(branch, expected, "{name}: lufs={lufs} peak={peak}");
            if expected == Branch::Silent {
                assert_eq!(routed, source, "{name}: silence must be untouched");
            } else {
                assert_ne!(routed, source, "{name}: gain should have moved samples");
            }
        }
    }

    /// The measurement is cached on the **real export path**: poisoning the
    /// shared cache under the buffer's content key changes what the export
    /// consumes, which is only possible if the second call never re-measured.
    #[test]
    fn export_measurement_hits_the_shared_analysis_cache() {
        let fs = 48_000u32;
        // A buffer no other test in this process produces (marker sample).
        let mut pcm = tone(0.037_913_1, 0.6, fs);
        pcm[0] = f32::from_bits(0x3ecc_cccd);
        let key = analysis::loudness_key(&pcm, fs);

        let first = measure_loudness_cached(&pcm, fs);
        assert_eq!(first, analysis::analyze_loudness(&pcm, fs));
        assert_eq!(
            shared_loudness_cache().lock().unwrap().get(key).cloned(),
            Some(first.clone()),
            "first measurement should have been cached under its content key"
        );

        let poison = AnalysisResult::Loudness {
            integrated_lufs: -60.0,
            true_peak_dbtp: -30.0,
            frames: 1,
            sample_rate: fs,
        };
        shared_loudness_cache()
            .lock()
            .unwrap()
            .insert(key, poison.clone());
        assert_eq!(
            measure_loudness_cached(&pcm, fs),
            poison,
            "second export of the same audio re-ran the measurement pass"
        );

        // The poisoned verdict is what the gain is computed from, proving the
        // export consumes the cache rather than the DSP.
        let target = LoudnessTarget {
            integrated_lufs: -14.0,
            true_peak_dbtp: -1.0,
        };
        let mut routed = pcm.clone();
        normalize(&mut routed, fs, &target);
        let mut expected = pcm.clone();
        apply_loudness_gain(&mut expected, &poison, &target);
        assert!(routed
            .iter()
            .zip(&expected)
            .all(|(a, b)| a.to_bits() == b.to_bits()));

        shared_loudness_cache().lock().unwrap().insert(key, first);
    }

    /// End-to-end through the export entry point: a loudness target on a silent
    /// sequence stays silent, and the range's measurement lands in the cache.
    #[test]
    fn render_export_audio_with_loudness_target_caches_its_measurement() {
        let (project, sid) = empty_audio_seq();
        let target = LoudnessTarget {
            integrated_lufs: -14.0,
            true_peak_dbtp: -1.0,
        };
        let pcm = render_export_audio(
            &project,
            sid,
            Tick(0),
            Tick(TICKS_PER_SECOND),
            None,
            Some(&target),
        )
        .unwrap();
        assert_eq!(pcm.len(), 48_000 * CHANNELS);
        assert!(pcm.iter().all(|&s| s == 0.0), "silence must stay silent");
        let key = analysis::loudness_key(&pcm, 48_000);
        assert!(shared_loudness_cache().lock().unwrap().get(key).is_some());
    }

    #[test]
    fn ticks_to_frames_is_exact_for_one_second_at_48k() {
        assert_eq!(ticks_to_frames(Tick(TICKS_PER_SECOND), 48_000), 48_000);
    }
}
