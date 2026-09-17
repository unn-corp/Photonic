//! PCM audio source over an ffmpeg `-f f32le` sidecar pipe.
//!
//! **Seam note (02 §3):** the audio twin of `decode::sidecar`'s rawvideo pipe.
//! `decode/` ships only the video pipe in P3, so the playback layer owns this
//! minimal PCM sidecar for the mixer's [`PcmSource`] seam (09 §4). When
//! `decode/` grows a parallel PCM pipe per 02 §3 ("+ a parallel PCM pipe for
//! audio assets (`-f f32le`)"), this type moves there unchanged — the mixer
//! only ever sees the trait.
//!
//! Policy conformance (09 §4, `PcmSource` trait doc): output is always stereo
//! (`-ac 2` downmixes/duplicates) at the mixer's configured sample rate
//! (`-ar`), so the mixer never resamples or remaps beyond `ChannelMap`.

use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};

use photonic_core::timeline::{SpeedMap, Tick, TICKS_PER_SECOND};

use crate::audio::mixer::PcmSource;
use crate::media::ffmpeg_locate::FfmpegTools;

/// Interleaved-stereo f32 PCM decoded by a persistent ffmpeg process.
pub struct FfmpegPcmSource {
    child: Child,
    stdout: ChildStdout,
    sample_rate: u32,
    /// Byte staging buffer reused across reads (no per-read allocation).
    buf: Vec<u8>,
    /// End-of-stream reached; subsequent reads return 0 frames.
    finished: bool,
}

/// A seekable, in-memory PCM source evaluated through a clip [`SpeedMap`].
///
/// It is intentionally independent of ffmpeg so preview and export can share
/// identical interpolation. The owner decides how PCM is cached or decoded;
/// this type only turns output timeline samples into source positions.
pub struct TimeWarpPcmSource {
    pcm: Vec<f32>,
    sample_rate: u32,
    source_start: Tick,
    source_in: Tick,
    offset: Tick,
    speed: SpeedMap,
    elapsed_ticks: f64,
}

/// Decode at most `frames` from a sequential PCM source into a reusable
/// seekable window. Short sources simply return the frames available.
pub fn read_pcm_window(source: &mut dyn PcmSource, frames: usize) -> Vec<f32> {
    let channels = source.channels() as usize;
    let mut pcm = Vec::with_capacity(frames.saturating_mul(channels));
    let mut scratch = vec![0.0; 4096usize.saturating_mul(channels)];
    while pcm.len() / channels < frames {
        let remaining = frames - pcm.len() / channels;
        let requested = remaining.min(4096);
        let read = source.read(&mut scratch[..requested * channels], requested);
        if read == 0 {
            break;
        }
        pcm.extend_from_slice(&scratch[..read * channels]);
        if read < requested {
            break;
        }
    }
    pcm
}

impl TimeWarpPcmSource {
    /// Construct a retimed source from interleaved stereo PCM at `sample_rate`.
    pub fn new(
        pcm: Vec<f32>,
        sample_rate: u32,
        source_start: Tick,
        source_in: Tick,
        offset: Tick,
        speed: SpeedMap,
        elapsed: Tick,
    ) -> Self {
        Self {
            pcm,
            sample_rate: sample_rate.max(1),
            source_start,
            source_in,
            offset,
            speed,
            elapsed_ticks: elapsed.0 as f64,
        }
    }

    fn sample_at(&self, source_frame: f64, channel: usize) -> f32 {
        if source_frame < 0.0 {
            return 0.0;
        }
        let first = source_frame.floor() as usize;
        let last_frame = self.pcm.len() / 2;
        if first >= last_frame {
            return 0.0;
        }
        let next = (first + 1).min(last_frame.saturating_sub(1));
        let fraction = (source_frame - first as f64) as f32;
        let a = self.pcm[first * 2 + channel];
        let b = self.pcm[next * 2 + channel];
        a + (b - a) * fraction
    }
}

impl PcmSource for TimeWarpPcmSource {
    fn channels(&self) -> u16 {
        2
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
        let frames = frames.min(out.len() / 2);
        let ticks_per_sample = TICKS_PER_SECOND as f64 / self.sample_rate as f64;
        for frame in 0..frames {
            let elapsed = Tick(self.elapsed_ticks.round() as i64);
            if self.speed.ratio_at_f64(elapsed).abs() > f64::EPSILON {
                let source_tick = self.source_in.0 as f64 - self.offset.0 as f64
                    + self.speed.source_delta_f64(elapsed);
                let source_frame = (source_tick - self.source_start.0 as f64)
                    * self.sample_rate as f64
                    / TICKS_PER_SECOND as f64;
                out[frame * 2] = self.sample_at(source_frame, 0);
                out[frame * 2 + 1] = self.sample_at(source_frame, 1);
            } else {
                out[frame * 2] = 0.0;
                out[frame * 2 + 1] = 0.0;
            }
            self.elapsed_ticks += ticks_per_sample;
        }
        frames
    }
}

impl FfmpegPcmSource {
    /// Spawn a decoder for `input`'s audio, seeked to `start`, emitting
    /// interleaved stereo f32 at `sample_rate`.
    ///
    /// `stream` (K-D3) selects demuxed audio stream index `0:a:N`. `None` uses
    /// the first audio stream (ffmpeg default for a lone audio input).
    pub fn spawn(
        tools: &FfmpegTools,
        input: &Path,
        start: Tick,
        sample_rate: u32,
    ) -> std::io::Result<FfmpegPcmSource> {
        Self::spawn_stream(tools, input, start, sample_rate, None)
    }

    /// Like [`spawn`] with an explicit multi-stream selector (26 K-D3).
    pub fn spawn_stream(
        tools: &FfmpegTools,
        input: &Path,
        start: Tick,
        sample_rate: u32,
        stream: Option<u32>,
    ) -> std::io::Result<FfmpegPcmSource> {
        let mut command = Command::new(&tools.ffmpeg);
        for a in pcm_ffmpeg_argv(input, start, sample_rate, stream) {
            command.arg(a);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // 37 §2.2: SIGKILL this PCM reader if the editor process dies (Linux).
        crate::media::child_registry::arm_parent_death_signal(&mut command);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        Ok(FfmpegPcmSource {
            child,
            stdout,
            sample_rate,
            buf: Vec::new(),
            finished: false,
        })
    }
}

/// Pure argv for the PCM sidecar (K-D3). Tested without spawning ffmpeg.
pub fn pcm_ffmpeg_argv(
    input: &Path,
    start: Tick,
    sample_rate: u32,
    stream: Option<u32>,
) -> Vec<String> {
    // Input seeking alone may snap compressed audio to the next packet on
    // some FFmpeg builds (notably IMA ADPCM on Ubuntu), skipping a short sound
    // immediately after the requested mark. Seek at most one second early for
    // speed, then use an output seek to discard the exact residual interval.
    let lookback = Tick::from_seconds(1);
    let coarse = Tick((start.0 - lookback.0).max(0));
    let residual = Tick(start.0 - coarse.0);
    let mut args = vec![
        "-v".into(),
        "error".into(),
        "-accurate_seek".into(),
        "-ss".into(),
        format!("{:.6}", coarse.as_seconds_f64()),
        "-i".into(),
        input.display().to_string(),
        "-ss".into(),
        format!("{:.6}", residual.as_seconds_f64()),
    ];
    // K-D3: pick a specific demuxed audio stream when the clip asks for one.
    if let Some(n) = stream {
        args.push("-map".into());
        args.push(format!("0:a:{n}"));
    }
    args.extend([
        "-vn".into(),
        "-sn".into(),
        "-dn".into(),
        "-ac".into(),
        "2".into(),
        "-ar".into(),
        sample_rate.to_string(),
        "-f".into(),
        "f32le".into(),
        "pipe:1".into(),
    ]);
    args
}

/// K-D3: clip-relative timeline position → source seek tick given sync offset.
/// Positive `offset` delays audio (seek earlier in the source).
pub fn source_seek_with_offset(
    source_in: Tick,
    timeline_t: Tick,
    clip_start: Tick,
    offset: Tick,
) -> Tick {
    Tick((source_in.0 + (timeline_t.0 - clip_start.0) - offset.0).max(0))
}

impl PcmSource for FfmpegPcmSource {
    fn channels(&self) -> u16 {
        2 // -ac 2 (see module doc)
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn read(&mut self, out: &mut [f32], frames: usize) -> usize {
        if self.finished || frames == 0 {
            return 0;
        }
        let want_bytes = frames * 2 * 4; // stereo f32
        if self.buf.len() < want_bytes {
            self.buf.resize(want_bytes, 0);
        }
        let mut got = 0usize;
        while got < want_bytes {
            match self.stdout.read(&mut self.buf[got..want_bytes]) {
                Ok(0) => {
                    self.finished = true;
                    break; // EOF: emit the whole frames we have
                }
                Ok(n) => got += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.finished = true;
                    break;
                }
            }
        }
        let whole_frames = got / 8;
        for (sample, bytes) in out[..whole_frames * 2]
            .iter_mut()
            .zip(self.buf[..whole_frames * 8].chunks_exact(4))
        {
            *sample = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        whole_frames
    }
}

impl Drop for FfmpegPcmSource {
    fn drop(&mut self) {
        // Kill-on-drop, mirroring decode::sidecar: never leave an ffmpeg
        // writing into a dead pipe.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ffmpeg_locate::locate_for_test;
    use photonic_core::timeline::clip::SpeedKey;
    use photonic_core::timeline::{Ratio, SpeedMap};
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn pcm_argv_maps_stream_index_when_set() {
        let args = pcm_ffmpeg_argv(Path::new("/a.mp4"), Tick::ZERO, 48_000, Some(2));
        let joined = args.join(" ");
        assert!(
            joined.contains("-map 0:a:2"),
            "expected demux map for stream 2, got {joined}"
        );
        let no_map = pcm_ffmpeg_argv(Path::new("/a.mp4"), Tick::ZERO, 48_000, None);
        assert!(
            !no_map.iter().any(|a| a == "-map"),
            "default first stream must not force -map"
        );
    }

    #[test]
    fn source_seek_with_offset_delays_audio() {
        // Clip at timeline 100, source_in 0, t=100, offset=50 → seek 50 earlier.
        let seek = source_seek_with_offset(Tick(0), Tick(100), Tick(100), Tick(50));
        assert_eq!(seek, Tick(0)); // clamped: 0+0-50
        let seek2 = source_seek_with_offset(Tick(200), Tick(150), Tick(100), Tick(50));
        // source_in 200 + (150-100) - 50 = 200
        assert_eq!(seek2, Tick(200));
        let seek3 = source_seek_with_offset(Tick(0), Tick(200), Tick(100), Tick(0));
        assert_eq!(seek3, Tick(100));
    }

    #[test]
    fn decodes_beep_wav_as_stereo_f32() {
        let Some(tools) = locate_for_test() else {
            eprintln!("ffmpeg not found — skipping FfmpegPcmSource test");
            return;
        };
        let mut src =
            FfmpegPcmSource::spawn(&tools, &fixture("beep_flash.wav"), Tick::ZERO, 48_000)
                .expect("spawn pcm sidecar");
        assert_eq!(src.channels(), 2);
        assert_eq!(src.sample_rate(), 48_000);

        // The fixture has a 5ms 1kHz beep at t=1.0s and silence off-beep
        // (fixtures/README.md). Read up to just past 1.0s and check energy.
        let frames = 48_128; // slightly past 1.0s
        let mut pcm = vec![0f32; frames * 2];
        let mut done = 0usize;
        while done < frames {
            let n = src.read(&mut pcm[done * 2..], (frames - done).min(4096));
            if n == 0 {
                break;
            }
            done += n;
        }
        assert!(done >= 48_048, "read through the beep instant (got {done})");
        let rms = |range: std::ops::Range<usize>| {
            let s: f64 = pcm[range.start * 2..range.end * 2]
                .iter()
                .map(|v| (*v as f64) * (*v as f64))
                .sum();
            (s / ((range.len() * 2) as f64)).sqrt()
        };
        let off = rms(24_000..24_480); // t=0.5s: silence
        let on = rms(48_000..48_048); // t=1.0s..+1ms: beep
        assert!(off < 1e-3, "off-beep window ~silent (rms {off})");
        assert!(on > 0.05, "beep window has energy (rms {on})");
    }

    #[test]
    fn time_warp_reads_reverse_source_samples() {
        let pcm: Vec<f32> = (0..8)
            .flat_map(|sample| [sample as f32, sample as f32])
            .collect();
        let mut source = TimeWarpPcmSource::new(
            pcm,
            TICKS_PER_SECOND as u32,
            Tick::ZERO,
            Tick(5),
            Tick::ZERO,
            SpeedMap::Constant(Ratio::new(-1, 1)),
            Tick::ZERO,
        );
        let mut out = [0.0; 6];
        source.read(&mut out, 3);
        assert_eq!(out, [5.0, 5.0, 4.0, 4.0, 3.0, 3.0]);
    }

    #[test]
    fn time_warp_silences_zero_rate_interval() {
        let pcm = vec![1.0; 16];
        let mut source = TimeWarpPcmSource::new(
            pcm,
            TICKS_PER_SECOND as u32,
            Tick::ZERO,
            Tick::ZERO,
            Tick::ZERO,
            SpeedMap::Keyframed {
                keys: vec![SpeedKey::new(Tick::ZERO, Ratio::new(0, 1))],
            },
            Tick::ZERO,
        );
        let mut out = [1.0; 4];
        source.read(&mut out, 2);
        assert_eq!(out, [0.0; 4]);
    }
}
