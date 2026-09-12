//! FFmpeg-sidecar decode (02 §3, D-03).
//!
//! One persistent `ffmpeg` process per `(asset, quality)` streams headerless
//! `rawvideo` on a pipe; a reader parses it into [`DecodedFrame`]s keyed by an
//! exact presentation [`Tick`]; a [`FrameRing`](ring::FrameRing) buffers them
//! around the playhead; a [`scheduler::DecodeSource`] drives seek/fill.
//!
//! ## Layout
//! - [`sidecar`] — spawn/kill the ffmpeg process (kill-on-drop), args, stderr drain.
//! - [`reader`]  — frame-boundary parser on the pipe → [`DecodedFrame`], PTS derivation.
//! - [`ring`]    — per-source decoded-frame ring, pts-keyed, thread-safe handoff.
//! - [`scheduler`] — minimal seek + sequential-fill driver.
//!
//! The decoded planes match [`photonic_render::video::YuvPlanes`] (the GPU-upload
//! consumer) exactly, so a `DecodedFrame` hands straight to the render crate with
//! zero copy via [`DecodedPlanes::as_yuv_planes`].

pub mod reader;
pub mod ring;
pub mod scheduler;
pub mod sidecar;
pub mod worker;

use photonic_core::timeline::Tick;
use photonic_render::video::YuvPlanes;

pub use reader::{FrameReader, PtsModel};
pub use ring::{FrameRing, SharedRing};
pub use scheduler::DecodeSource;
pub use sidecar::{Sidecar, SidecarConfig};
pub use worker::DecodeWorker;

/// Decode quality: the ring/cache and process are keyed by this so preview and
/// full-res streams don't collide (02 §3 "one process per (asset, quality)").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DecodeQuality {
    /// Preview (proxy allowed): 16 fwd / 4 back ring default.
    Preview,
    /// Full resolution (export / originals).
    Full,
}

/// Output pixel layout requested from ffmpeg. `Yuva444p` is chosen only when the
/// source carries alpha (02 §3); otherwise `Yuv420p` (half the chroma bytes).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PixFmt {
    Yuv420p,
    Yuva444p,
}

impl PixFmt {
    /// The ffmpeg `-pix_fmt` token.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            PixFmt::Yuv420p => "yuv420p",
            PixFmt::Yuva444p => "yuva444p",
        }
    }

    /// Bytes for one full frame at `width`×`height` in this layout. Chroma
    /// dimensions round up (`(w+1)/2`) so odd sizes are handled.
    pub fn frame_bytes(self, width: u32, height: u32) -> usize {
        let (w, h) = (width as usize, height as usize);
        match self {
            PixFmt::Yuv420p => {
                let cw = w.div_ceil(2);
                let ch = h.div_ceil(2);
                w * h + 2 * cw * ch
            }
            PixFmt::Yuva444p => 4 * w * h,
        }
    }

    /// Pick the layout for a source: `Yuva444p` iff it carries alpha.
    pub fn for_alpha(has_alpha: bool) -> PixFmt {
        if has_alpha {
            PixFmt::Yuva444p
        } else {
            PixFmt::Yuv420p
        }
    }
}

/// One decoded frame with its exact presentation tick and owned planes.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedFrame {
    pub pts: Tick,
    pub planes: DecodedPlanes,
}

/// Owned YUV(+A) frame data. Each variant keeps the FFmpeg `rawvideo` payload
/// in one contiguous allocation; plane views are derived on demand. This is
/// important for preview throughput: splitting a raw frame into independent
/// `Vec`s used to copy every decoded byte a second time before GPU upload.
/// Borrowed as [`YuvPlanes`] for GPU upload with no copy.
#[derive(Clone, Debug, PartialEq)]
pub enum DecodedPlanes {
    Yuv420 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    Yuva444 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
}

impl DecodedPlanes {
    /// Bytes owned by the contiguous decoded payload (including spare capacity).
    pub fn allocated_bytes(&self) -> u64 {
        match self {
            Self::Yuv420 { data, .. } | Self::Yuva444 { data, .. } => data.capacity() as u64,
        }
    }

    /// Construct a 4:2:0 frame from FFmpeg's tightly packed `yuv420p` payload.
    ///
    /// This validates the public input in every build. A malformed contiguous
    /// payload would otherwise panic later while deriving plane slices, far from
    /// the caller that supplied it.
    #[inline]
    pub fn yuv420(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuv420p, width, height, data.len());
        Self::Yuv420 {
            width,
            height,
            data,
        }
    }

    /// Construct a 4:4:4-with-alpha frame from FFmpeg's tightly packed
    /// `yuva444p` payload.
    ///
    /// This validates the public input in every build; see [`Self::yuv420`].
    #[inline]
    pub fn yuva444(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuva444p, width, height, data.len());
        Self::Yuva444 {
            width,
            height,
            data,
        }
    }

    /// Construct from a frame reader that has already consumed exactly one
    /// `rawvideo` frame. This keeps the per-frame reader path to a move plus a
    /// discriminant branch; public callers must use the always-checked
    /// constructors above.
    #[inline]
    pub(crate) fn from_rawvideo(pix_fmt: PixFmt, width: u32, height: u32, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), pix_fmt.frame_bytes(width, height));
        match pix_fmt {
            PixFmt::Yuv420p => Self::Yuv420 {
                width,
                height,
                data,
            },
            PixFmt::Yuva444p => Self::Yuva444 {
                width,
                height,
                data,
            },
        }
    }

    pub fn dims(&self) -> (u32, u32) {
        match *self {
            DecodedPlanes::Yuv420 { width, height, .. }
            | DecodedPlanes::Yuva444 { width, height, .. } => (width, height),
        }
    }

    /// Luma plane.
    pub fn y(&self) -> &[u8] {
        let (width, height, data) = self.storage();
        &data[..width as usize * height as usize]
    }

    /// Cb chroma plane.
    pub fn cb(&self) -> &[u8] {
        let (width, height, data) = self.storage();
        let y_len = width as usize * height as usize;
        let c_len = match self {
            Self::Yuv420 { .. } => (width as usize).div_ceil(2) * (height as usize).div_ceil(2),
            Self::Yuva444 { .. } => y_len,
        };
        &data[y_len..y_len + c_len]
    }

    /// Cr chroma plane.
    pub fn cr(&self) -> &[u8] {
        let (width, height, data) = self.storage();
        let y_len = width as usize * height as usize;
        let c_len = self.cb().len();
        &data[y_len + c_len..y_len + 2 * c_len]
    }

    /// Alpha plane, when this is a `yuva444p` frame.
    pub fn a(&self) -> Option<&[u8]> {
        match self {
            Self::Yuv420 { .. } => None,
            Self::Yuva444 {
                width,
                height,
                data,
            } => {
                let plane_len = *width as usize * *height as usize;
                Some(&data[3 * plane_len..4 * plane_len])
            }
        }
    }

    fn storage(&self) -> (u32, u32, &[u8]) {
        match self {
            Self::Yuv420 {
                width,
                height,
                data,
            }
            | Self::Yuva444 {
                width,
                height,
                data,
            } => (*width, *height, data),
        }
    }

    /// Borrow as the render crate's [`YuvPlanes`] for GPU upload (zero copy).
    pub fn as_yuv_planes(&self) -> YuvPlanes<'_> {
        match self {
            DecodedPlanes::Yuv420 { width, height, .. } => YuvPlanes::Yuv420 {
                width: *width,
                height: *height,
                y: self.y(),
                cb: self.cb(),
                cr: self.cr(),
            },
            DecodedPlanes::Yuva444 { width, height, .. } => YuvPlanes::Yuva444 {
                width: *width,
                height: *height,
                y: self.y(),
                cb: self.cb(),
                cr: self.cr(),
                a: self.a().expect("YUVA frame has an alpha plane"),
            },
        }
    }
}

#[inline]
fn assert_payload_len(pix_fmt: PixFmt, width: u32, height: u32, got: usize) {
    let expected = pix_fmt.frame_bytes(width, height);
    assert_eq!(
        got,
        expected,
        "invalid {} payload for {width}x{height}: expected {expected} bytes, got {got}",
        pix_fmt.ffmpeg_name(),
    );
}

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("decode cancelled")]
    Cancelled,
    #[error("failed to spawn ffmpeg: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("pipe read error: {0}")]
    Io(#[source] std::io::Error),
    #[error("ffmpeg stream ended mid-frame (got {got} of {expected} bytes)")]
    PartialFrame { got: usize, expected: usize },
    #[error("decode process exhausted its restart budget ({max}); last error: {last}")]
    RestartsExhausted { max: u32, last: String },
    #[error("no keyframe index / pts model available for pts-true decode")]
    NoPtsModel,
    #[error("decoder produced no frames for the requested seek")]
    EmptyDecode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_bytes_yuv420_and_yuva444() {
        // 320×180 4:2:0 = 320*180 + 2*160*90 = 57600 + 28800 = 86400.
        assert_eq!(PixFmt::Yuv420p.frame_bytes(320, 180), 86_400);
        // 4:4:4:A = 4*320*180 = 230400.
        assert_eq!(PixFmt::Yuva444p.frame_bytes(320, 180), 230_400);
    }

    #[test]
    fn frame_bytes_rounds_odd_dims() {
        // 3×3 4:2:0: 9 + 2*(2*2) = 9 + 8 = 17.
        assert_eq!(PixFmt::Yuv420p.frame_bytes(3, 3), 17);
    }

    #[test]
    fn pix_fmt_for_alpha() {
        assert_eq!(PixFmt::for_alpha(true), PixFmt::Yuva444p);
        assert_eq!(PixFmt::for_alpha(false), PixFmt::Yuv420p);
    }

    #[test]
    fn planes_borrow_as_yuv_planes() {
        let p = DecodedPlanes::yuv420(2, 2, vec![1, 2, 3, 4, 128, 128]);
        match p.as_yuv_planes() {
            YuvPlanes::Yuv420 { width, y, .. } => {
                assert_eq!(width, 2);
                assert_eq!(y, &[1, 2, 3, 4]);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    #[should_panic(expected = "invalid yuv420p payload")]
    fn malformed_public_yuv_payload_is_rejected_in_all_builds() {
        let _ = DecodedPlanes::yuv420(2, 2, vec![0; 5]);
    }
}
