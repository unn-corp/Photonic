//! Minimal canonical PCM WAV read/write — no crate dependency.
//!
//! Two callers need this: the hosted adapter probes a WAV's duration to
//! compute its per-request timeout budget (06 §2.2's `2 × source duration +
//! 30s`) without shelling out to `ffprobe` a second time, and tests need a
//! tiny synthetic WAV fixture writer that doesn't pull in a crate like
//! `hound` just to produce a few hundred bytes of silence.

/// The fields decode needs out of a WAV's `fmt `/`data` chunks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub data_len: u32,
}

impl WavInfo {
    pub fn duration_secs(&self) -> f64 {
        let bytes_per_sample = (self.bits_per_sample / 8).max(1) as f64;
        let frame_bytes = bytes_per_sample * self.channels.max(1) as f64;
        if frame_bytes <= 0.0 || self.sample_rate == 0 {
            return 0.0;
        }
        (self.data_len as f64 / frame_bytes) / self.sample_rate as f64
    }
}

/// Parse a RIFF/WAVE header, walking chunks generically (so `fmt `/`data`
/// can appear in either order, with arbitrary chunks — e.g. `LIST` — between
/// them, as real-world WAV files do). Returns `None` on anything malformed.
pub fn read_wav_info(bytes: &[u8]) -> Option<WavInfo> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u32, u16)> = None; // (channels, sample_rate, bits_per_sample)
    let mut data_len: Option<u32> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let body_start = pos + 8;
        let body_end = body_start.checked_add(size)?;
        if body_end > bytes.len() {
            break;
        }

        if id == b"fmt " && size >= 16 {
            let channels =
                u16::from_le_bytes(bytes[body_start + 2..body_start + 4].try_into().ok()?);
            let sample_rate =
                u32::from_le_bytes(bytes[body_start + 4..body_start + 8].try_into().ok()?);
            let bits_per_sample =
                u16::from_le_bytes(bytes[body_start + 14..body_start + 16].try_into().ok()?);
            fmt = Some((channels, sample_rate, bits_per_sample));
        } else if id == b"data" {
            data_len = Some(size as u32);
        }

        // Chunks are word-aligned: a size with the low bit set has one pad byte.
        pos = body_end + (size & 1);
    }

    let (channels, sample_rate, bits_per_sample) = fmt?;
    Some(WavInfo {
        sample_rate,
        channels,
        bits_per_sample,
        data_len: data_len?,
    })
}

/// Encode 16-bit PCM samples as a canonical 44-byte-header WAV file.
pub fn write_pcm16_wav(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let block_align = channels * (bits_per_sample / 8);
    let byte_rate = sample_rate * block_align as u32;
    let data_bytes = samples.len() * 2;

    let mut buf = Vec::with_capacity(44 + data_bytes);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_bytes) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits_per_sample.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    buf
}

/// `duration_secs` of digital silence at 48 kHz mono — the shape every
/// caption-adjacent WAV in this module uses (06 §3.2).
pub fn silent_48k_mono_wav(duration_secs: f64) -> Vec<u8> {
    let sample_rate = 48_000u32;
    let n = (duration_secs.max(0.0) * sample_rate as f64) as usize;
    write_pcm16_wav(sample_rate, 1, &vec![0i16; n])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_sample_rate_channels_and_duration() {
        let bytes = write_pcm16_wav(48_000, 1, &vec![0i16; 48_000 * 2]); // 2 seconds
        let info = read_wav_info(&bytes).expect("valid wav");
        assert_eq!(info.sample_rate, 48_000);
        assert_eq!(info.channels, 1);
        assert_eq!(info.bits_per_sample, 16);
        assert!((info.duration_secs() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn silent_wav_helper_matches_requested_duration() {
        let bytes = silent_48k_mono_wav(1.5);
        let info = read_wav_info(&bytes).unwrap();
        assert!((info.duration_secs() - 1.5).abs() < 1e-3);
        assert_eq!(info.channels, 1);
        assert_eq!(info.sample_rate, 48_000);
    }

    #[test]
    fn rejects_non_riff_data() {
        assert!(read_wav_info(b"not a wav file at all").is_none());
        assert!(read_wav_info(&[]).is_none());
    }

    #[test]
    fn skips_unknown_chunks_between_fmt_and_data() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&0u32.to_le_bytes()); // patched below
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // mono
        buf.extend_from_slice(&48_000u32.to_le_bytes());
        buf.extend_from_slice(&96_000u32.to_le_bytes());
        buf.extend_from_slice(&2u16.to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        // An odd-sized unknown "JUNK" chunk to exercise word-alignment padding.
        buf.extend_from_slice(b"JUNK");
        buf.extend_from_slice(&3u32.to_le_bytes());
        buf.extend_from_slice(&[0u8, 0, 0]);
        buf.push(0); // pad byte
        buf.extend_from_slice(b"data");
        let samples = [1i16, -1, 2, -2];
        buf.extend_from_slice(&((samples.len() * 2) as u32).to_le_bytes());
        for s in samples {
            buf.extend_from_slice(&s.to_le_bytes());
        }
        let riff_len = (buf.len() - 8) as u32;
        buf[4..8].copy_from_slice(&riff_len.to_le_bytes());

        let info = read_wav_info(&buf).expect("valid wav with a JUNK chunk");
        assert_eq!(info.data_len, 8);
        assert_eq!(info.sample_rate, 48_000);
    }
}
