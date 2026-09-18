//! Frame-boundary parser over the `rawvideo` pipe → [`DecodedFrame`] (02 §3/§4).
//!
//! `rawvideo` is headerless: frames are back-to-back plane bytes with no
//! timestamps, so the reader (a) slices the stream into fixed
//! [`PixFmt::frame_bytes`]-sized frames and (b) derives each frame's exact
//! presentation [`Tick`] itself.
//!
//! ## PTS derivation — why two models (02 §4 "pts-true")
//!
//! The pipe carries no PTS, so we reconstruct it. For a **constant-frame-rate**
//! source this is exact arithmetic at zero extra cost: the first output frame is
//! the seek keyframe (input `-ss` lands exactly on it), so
//! `pts(i) = (start_frame + i) × ticks_per_frame`. This keeps the cold-seek
//! budget (02 §8) — no second probe pass.
//!
//! For a **variable-frame-rate** source that arithmetic is wrong (the per-frame
//! interval changes — e.g. `vfr_sample.mp4` steps 24→30 fps mid-file). There is
//! no way to recover true PTS from the headerless pipe, so decode falls back to
//! a ground-truth [`PtsIndex`](crate::media::PtsIndex) built once via ffprobe and
//! cached in the sidecar dir; the reader maps output ordinal → absolute source
//! frame → `table[frame]`. This is the pts-true path 02 §4 requires; it costs one
//! extra probe pass, paid only for genuinely variable sources.

use std::io::Read;
use std::sync::Arc;

use photonic_core::timeline::{FrameRate, Tick};

use super::{DecodeError, DecodedFrame, DecodedPlanes, PixFmt};
use crate::media::keyframe_index::PtsIndex;

/// How the reader assigns a presentation tick to output frame `i`.
#[derive(Clone, Debug)]
pub enum PtsModel {
    /// Constant-frame-rate: `pts(i) = (start_frame + i) × ticks_per_frame`.
    /// `start_frame` is the seek keyframe's source frame ordinal.
    Cfr { rate: FrameRate, start_frame: i64 },
    /// Variable-frame-rate: look up the absolute source frame in the pts table.
    /// `pts(i) = table[start_frame + i]`.
    Table {
        start_frame: i64,
        table: Arc<PtsIndex>,
    },
}

impl PtsModel {
    /// Presentation tick for the `i`-th output frame since the seek origin.
    fn pts(&self, i: i64) -> Option<Tick> {
        match self {
            PtsModel::Cfr { rate, start_frame } => {
                let frame = start_frame + i;
                Some(Tick(frame * rate.ticks_per_frame().0))
            }
            PtsModel::Table { start_frame, table } => table.pts_at(start_frame + i),
        }
    }
}

/// Splits the rawvideo pipe into presentation-timed frames.
pub struct FrameReader<R: Read> {
    reader: R,
    width: u32,
    height: u32,
    pix_fmt: PixFmt,
    frame_bytes: usize,
    /// Output ordinal since the seek origin (0-based).
    ordinal: i64,
    pts_model: PtsModel,
    /// Reused only while a keyframe seek discards pre-target frames. Frames
    /// retained by the ring must own their bytes, but skipped GOP frames do not.
    discard_buf: Vec<u8>,
}

impl<R: Read> FrameReader<R> {
    pub fn new(reader: R, width: u32, height: u32, pix_fmt: PixFmt, pts_model: PtsModel) -> Self {
        FrameReader {
            reader,
            width,
            height,
            pix_fmt,
            frame_bytes: pix_fmt.frame_bytes(width, height),
            ordinal: 0,
            pts_model,
            discard_buf: Vec::new(),
        }
    }

    /// Read the next frame. `Ok(None)` on a clean end-of-stream (frame boundary);
    /// `Err(PartialFrame)` if the stream ends mid-frame (a crash mid-decode).
    pub fn next_frame(&mut self) -> Result<Option<DecodedFrame>, DecodeError> {
        let mut buf = vec![0u8; self.frame_bytes];
        if !Self::read_frame(&mut self.reader, &mut buf)? {
            return Ok(None);
        }

        let pts = self.advance_pts()?;
        let planes = self.split_planes(buf);
        Ok(Some(DecodedFrame { pts, planes }))
    }

    /// Read and discard one full raw frame while preserving its presentation
    /// ordinal. This is the seek fast path for frames before the target: reuse
    /// one buffer instead of allocating a full YUV payload per discarded GOP
    /// frame. Returns its tick, or `None` on clean end-of-stream.
    pub fn discard_next_frame(&mut self) -> Result<Option<Tick>, DecodeError> {
        let mut buf = std::mem::take(&mut self.discard_buf);
        buf.resize(self.frame_bytes, 0);
        let result = Self::read_frame(&mut self.reader, &mut buf);
        self.discard_buf = buf;
        if !result? {
            return Ok(None);
        }
        self.advance_pts().map(Some)
    }

    fn read_frame(reader: &mut R, buf: &mut [u8]) -> Result<bool, DecodeError> {
        let mut filled = 0;
        while filled < buf.len() {
            match reader.read(&mut buf[filled..]) {
                Ok(0) => {
                    if filled == 0 {
                        return Ok(false); // clean EOF at a frame boundary
                    }
                    return Err(DecodeError::PartialFrame {
                        got: filled,
                        expected: buf.len(),
                    });
                }
                Ok(n) => filled += n,
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(DecodeError::Io(e)),
            }
        }
        Ok(true)
    }

    fn advance_pts(&mut self) -> Result<Tick, DecodeError> {
        let pts = self
            .pts_model
            .pts(self.ordinal)
            .ok_or(DecodeError::NoPtsModel)?;
        self.ordinal += 1;
        Ok(pts)
    }

    /// The presentation tick the *next* frame will carry (without reading it).
    pub fn next_pts(&self) -> Option<Tick> {
        self.pts_model.pts(self.ordinal)
    }

    fn split_planes(&self, buf: Vec<u8>) -> DecodedPlanes {
        DecodedPlanes::from_rawvideo(self.pix_fmt, self.width, self.height, buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::keyframe_index::PtsIndex;

    /// A CFR reader over an in-memory two-frame yuv420p 2×2 stream derives the
    /// right ticks: frame N (start_frame=60, 30fps) then N+1.
    #[test]
    fn cfr_pts_and_plane_split() {
        let fb = PixFmt::Yuv420p.frame_bytes(2, 2); // 2*2 + 2*1 = 6
        assert_eq!(fb, 6);
        let mut bytes = Vec::new();
        // frame 0 planes: Y=[1,2,3,4] Cb=[100] Cr=[200]
        bytes.extend_from_slice(&[1, 2, 3, 4, 100, 200]);
        // frame 1 planes: Y=[5,6,7,8] Cb=[101] Cr=[201]
        bytes.extend_from_slice(&[5, 6, 7, 8, 101, 201]);

        let rate = FrameRate::FPS_30;
        let tpf = rate.ticks_per_frame().0;
        let mut r = FrameReader::new(
            std::io::Cursor::new(bytes),
            2,
            2,
            PixFmt::Yuv420p,
            PtsModel::Cfr {
                rate,
                start_frame: 60,
            },
        );

        let f0 = r.next_frame().unwrap().unwrap();
        assert_eq!(f0.pts, Tick(60 * tpf));
        assert_eq!(f0.planes.y(), &[1, 2, 3, 4]);
        assert_eq!(f0.planes.cb(), &[100]);
        assert_eq!(f0.planes.cr(), &[200]);

        let f1 = r.next_frame().unwrap().unwrap();
        assert_eq!(f1.pts, Tick(61 * tpf));

        assert!(r.next_frame().unwrap().is_none(), "clean EOF");
    }

    #[test]
    fn discarded_frame_advances_pts_without_affecting_next_payload() {
        let rate = FrameRate::FPS_30;
        let tpf = rate.ticks_per_frame().0;
        // Two yuv420p 2×2 frames. The first represents a pre-target GOP frame
        // and must be read into the reusable discard buffer; the second still
        // has to arrive with its own pixels and the correct ordinal/PTS.
        let bytes = vec![1, 2, 3, 4, 100, 200, 5, 6, 7, 8, 101, 201];
        let mut r = FrameReader::new(
            std::io::Cursor::new(bytes),
            2,
            2,
            PixFmt::Yuv420p,
            PtsModel::Cfr {
                rate,
                start_frame: 10,
            },
        );

        assert_eq!(r.discard_next_frame().unwrap(), Some(Tick(10 * tpf)));
        let kept = r.next_frame().unwrap().expect("second frame");
        assert_eq!(kept.pts, Tick(11 * tpf));
        assert_eq!(kept.planes.y(), &[5, 6, 7, 8]);
        assert_eq!(kept.planes.cb(), &[101]);
        assert_eq!(kept.planes.cr(), &[201]);
    }

    #[test]
    fn table_pts_model_uses_source_frame() {
        let table = Arc::new(PtsIndex {
            pts: vec![Tick(0), Tick(10), Tick(20), Tick(30), Tick(45)],
        });
        // Seek origin at source frame 2 → first output frame = table[2] = 20.
        let fb = PixFmt::Yuv420p.frame_bytes(2, 2);
        let bytes = vec![0u8; fb * 2];
        let mut r = FrameReader::new(
            std::io::Cursor::new(bytes),
            2,
            2,
            PixFmt::Yuv420p,
            PtsModel::Table {
                start_frame: 2,
                table,
            },
        );
        assert_eq!(r.next_frame().unwrap().unwrap().pts, Tick(20));
        assert_eq!(r.next_frame().unwrap().unwrap().pts, Tick(30));
    }

    #[test]
    fn partial_frame_is_an_error() {
        // 5 bytes for a 6-byte frame → crash mid-frame.
        let mut r = FrameReader::new(
            std::io::Cursor::new(vec![1, 2, 3, 4, 5]),
            2,
            2,
            PixFmt::Yuv420p,
            PtsModel::Cfr {
                rate: FrameRate::FPS_30,
                start_frame: 0,
            },
        );
        assert!(matches!(
            r.next_frame(),
            Err(DecodeError::PartialFrame {
                got: 5,
                expected: 6
            })
        ));
    }
}
