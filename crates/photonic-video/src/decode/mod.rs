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

/// Output pixel layout requested from ffmpeg. Alpha sources use 4:4:4; known
/// non-alpha YUV sources keep their 4:2:0, 4:2:2 or 4:4:4 chroma sampling.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum PixFmt {
    Yuv420p,
    Yuv422p,
    Yuv444p,
    Yuv420p16le,
    Yuv422p16le,
    Yuva444p,
    Yuv444p16le,
    Yuva444p16le,
}

impl PixFmt {
    /// Whether the native managed source bridge can preserve this probed YUV
    /// layout without silently falling back to an 8-bit decode. Legacy SDR
    /// still uses `for_source` for formats outside this qualification set.
    pub fn supports_native_source(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        if matches!(
            name.as_str(),
            "nv12" | "nv21" | "nv16" | "nv61" | "nv24" | "nv42" | "yuyv422" | "uyvy422"
        ) {
            return true;
        }
        if [
            "p010", "p012", "p016", "p210", "p212", "p216", "p410", "p412", "p416",
        ]
        .iter()
        .any(|prefix| {
            name == *prefix || name == format!("{prefix}le") || name == format!("{prefix}be")
        }) {
            return true;
        }
        [
            "yuv420p", "yuv422p", "yuv444p", "yuva420p", "yuva422p", "yuva444p", "yuvj420p",
            "yuvj422p", "yuvj444p",
        ]
        .iter()
        .any(|prefix| {
            name == *prefix
                || ["9", "10", "12", "14", "16"].iter().any(|depth| {
                    name == format!("{prefix}{depth}le") || name == format!("{prefix}{depth}be")
                })
        })
    }

    /// The ffmpeg `-pix_fmt` token.
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            PixFmt::Yuv420p => "yuv420p",
            PixFmt::Yuv422p => "yuv422p",
            PixFmt::Yuv444p => "yuv444p",
            PixFmt::Yuv420p16le => "yuv420p16le",
            PixFmt::Yuv422p16le => "yuv422p16le",
            PixFmt::Yuva444p => "yuva444p",
            PixFmt::Yuv444p16le => "yuv444p16le",
            PixFmt::Yuva444p16le => "yuva444p16le",
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
            PixFmt::Yuv422p => w * h + 2 * w.div_ceil(2) * h,
            PixFmt::Yuv444p => 3 * w * h,
            PixFmt::Yuv420p16le => {
                let cw = w.div_ceil(2);
                let ch = h.div_ceil(2);
                2 * (w * h + 2 * cw * ch)
            }
            PixFmt::Yuv422p16le => 2 * (w * h + 2 * w.div_ceil(2) * h),
            PixFmt::Yuva444p => 4 * w * h,
            PixFmt::Yuv444p16le => 6 * w * h,
            PixFmt::Yuva444p16le => 8 * w * h,
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

    /// Preserve YUV source chroma sampling and samples above 8 bits through
    /// FFmpeg's planar 16-bit little-endian output, including big-endian source
    /// layouts. Unknown formats retain the historical 8-bit fallback until a
    /// native source interpretation can explicitly reject them.
    pub fn for_source(pixel_format: Option<&str>, has_alpha: bool) -> PixFmt {
        let high_depth = pixel_format.is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            (name.starts_with("yuv")
                || name.starts_with("yuva")
                || name.starts_with("p0")
                || name.starts_with("p2")
                || name.starts_with("p4"))
                && [
                    "9le", "9be", "10le", "10be", "12le", "12be", "14le", "14be", "16le", "16be",
                    "p010", "p012", "p016", "p210", "p212", "p216", "p410", "p412", "p416",
                ]
                .iter()
                .any(|marker| name.contains(marker))
        });
        let source_is_420 = pixel_format.is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.contains("420")
                || name.starts_with("p010")
                || name.starts_with("p012")
                || name.starts_with("p016")
        });
        let source_is_422 = pixel_format.is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.contains("422")
                || name.starts_with("p2")
                || name.starts_with("nv16")
                || name.starts_with("nv61")
        });
        let source_is_444 = pixel_format.is_some_and(|name| {
            let name = name.to_ascii_lowercase();
            name.contains("444")
                || name.starts_with("p4")
                || name.starts_with("nv24")
                || name.starts_with("nv42")
        });
        match (
            high_depth,
            has_alpha,
            source_is_420,
            source_is_422,
            source_is_444,
        ) {
            (true, true, _, _, _) => PixFmt::Yuva444p16le,
            (true, false, true, _, _) => PixFmt::Yuv420p16le,
            (true, false, false, true, _) => PixFmt::Yuv422p16le,
            (true, false, false, false, _) => PixFmt::Yuv444p16le,
            (false, false, false, true, _) => PixFmt::Yuv422p,
            (false, false, false, false, true) => PixFmt::Yuv444p,
            (false, _, _, _, _) => PixFmt::for_alpha(has_alpha),
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
    Yuv422 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    Yuv444 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    Yuv420P16 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    Yuv422P16 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    Yuva444 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    Yuv444P16 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
    Yuva444P16 {
        width: u32,
        height: u32,
        data: Vec<u8>,
    },
}

impl DecodedPlanes {
    /// Bytes owned by the contiguous decoded payload (including spare capacity).
    pub fn allocated_bytes(&self) -> u64 {
        match self {
            Self::Yuv420 { data, .. }
            | Self::Yuv422 { data, .. }
            | Self::Yuv444 { data, .. }
            | Self::Yuv420P16 { data, .. }
            | Self::Yuv422P16 { data, .. }
            | Self::Yuva444 { data, .. }
            | Self::Yuv444P16 { data, .. }
            | Self::Yuva444P16 { data, .. } => data.capacity() as u64,
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

    pub fn yuv422(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuv422p, width, height, data.len());
        Self::Yuv422 {
            width,
            height,
            data,
        }
    }

    pub fn yuv444(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuv444p, width, height, data.len());
        Self::Yuv444 {
            width,
            height,
            data,
        }
    }

    pub fn yuv420p16(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuv420p16le, width, height, data.len());
        Self::Yuv420P16 {
            width,
            height,
            data,
        }
    }

    pub fn yuv422p16(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuv422p16le, width, height, data.len());
        Self::Yuv422P16 {
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

    pub fn yuv444p16(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuv444p16le, width, height, data.len());
        Self::Yuv444P16 {
            width,
            height,
            data,
        }
    }

    pub fn yuva444p16(width: u32, height: u32, data: Vec<u8>) -> Self {
        assert_payload_len(PixFmt::Yuva444p16le, width, height, data.len());
        Self::Yuva444P16 {
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
            PixFmt::Yuv422p => Self::Yuv422 {
                width,
                height,
                data,
            },
            PixFmt::Yuv444p => Self::Yuv444 {
                width,
                height,
                data,
            },
            PixFmt::Yuv420p16le => Self::Yuv420P16 {
                width,
                height,
                data,
            },
            PixFmt::Yuv422p16le => Self::Yuv422P16 {
                width,
                height,
                data,
            },
            PixFmt::Yuva444p => Self::Yuva444 {
                width,
                height,
                data,
            },
            PixFmt::Yuv444p16le => Self::Yuv444P16 {
                width,
                height,
                data,
            },
            PixFmt::Yuva444p16le => Self::Yuva444P16 {
                width,
                height,
                data,
            },
        }
    }

    pub fn dims(&self) -> (u32, u32) {
        match *self {
            DecodedPlanes::Yuv420 { width, height, .. }
            | DecodedPlanes::Yuv422 { width, height, .. }
            | DecodedPlanes::Yuv444 { width, height, .. }
            | DecodedPlanes::Yuv420P16 { width, height, .. }
            | DecodedPlanes::Yuv422P16 { width, height, .. }
            | DecodedPlanes::Yuva444 { width, height, .. }
            | DecodedPlanes::Yuv444P16 { width, height, .. }
            | DecodedPlanes::Yuva444P16 { width, height, .. } => (width, height),
        }
    }

    /// Luma plane.
    pub fn y(&self) -> &[u8] {
        let (width, height, data) = self.storage();
        &data[..width as usize * height as usize * self.bytes_per_sample()]
    }

    fn bytes_per_sample(&self) -> usize {
        if matches!(
            self,
            Self::Yuv420P16 { .. }
                | Self::Yuv422P16 { .. }
                | Self::Yuv444P16 { .. }
                | Self::Yuva444P16 { .. }
        ) {
            2
        } else {
            1
        }
    }

    /// Cb chroma plane.
    pub fn cb(&self) -> &[u8] {
        let (width, height, data) = self.storage();
        let y_len = width as usize * height as usize * self.bytes_per_sample();
        let c_len = match self {
            Self::Yuv420 { .. } | Self::Yuv420P16 { .. } => {
                (width as usize).div_ceil(2)
                    * (height as usize).div_ceil(2)
                    * self.bytes_per_sample()
            }
            Self::Yuv422 { .. } | Self::Yuv422P16 { .. } => {
                (width as usize).div_ceil(2) * height as usize * self.bytes_per_sample()
            }
            Self::Yuv444 { .. }
            | Self::Yuva444 { .. }
            | Self::Yuv444P16 { .. }
            | Self::Yuva444P16 { .. } => y_len,
        };
        &data[y_len..y_len + c_len]
    }

    /// Cr chroma plane.
    pub fn cr(&self) -> &[u8] {
        let (width, height, data) = self.storage();
        let y_len = width as usize * height as usize * self.bytes_per_sample();
        let c_len = self.cb().len();
        &data[y_len + c_len..y_len + 2 * c_len]
    }

    /// Alpha plane, when this is a `yuva444p` frame.
    pub fn a(&self) -> Option<&[u8]> {
        match self {
            Self::Yuv420 { .. }
            | Self::Yuv422 { .. }
            | Self::Yuv444 { .. }
            | Self::Yuv420P16 { .. }
            | Self::Yuv422P16 { .. }
            | Self::Yuv444P16 { .. } => None,
            Self::Yuva444 {
                width,
                height,
                data,
            } => {
                let plane_len = *width as usize * *height as usize;
                Some(&data[3 * plane_len..4 * plane_len])
            }
            Self::Yuva444P16 {
                width,
                height,
                data,
            } => {
                let plane_len = *width as usize * *height as usize * 2;
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
            | Self::Yuv422 {
                width,
                height,
                data,
            }
            | Self::Yuv444 {
                width,
                height,
                data,
            }
            | Self::Yuv420P16 {
                width,
                height,
                data,
            }
            | Self::Yuv422P16 {
                width,
                height,
                data,
            }
            | Self::Yuva444 {
                width,
                height,
                data,
            }
            | Self::Yuv444P16 {
                width,
                height,
                data,
            }
            | Self::Yuva444P16 {
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
            DecodedPlanes::Yuv422 { width, height, .. } => YuvPlanes::Yuv422 {
                width: *width,
                height: *height,
                y: self.y(),
                cb: self.cb(),
                cr: self.cr(),
            },
            DecodedPlanes::Yuv444 { width, height, .. } => YuvPlanes::Yuv444 {
                width: *width,
                height: *height,
                y: self.y(),
                cb: self.cb(),
                cr: self.cr(),
            },
            DecodedPlanes::Yuv420P16 { width, height, .. } => YuvPlanes::Yuv420P16 {
                width: *width,
                height: *height,
                y: self.y(),
                cb: self.cb(),
                cr: self.cr(),
            },
            DecodedPlanes::Yuv422P16 { width, height, .. } => YuvPlanes::Yuv422P16 {
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
            DecodedPlanes::Yuv444P16 { width, height, .. }
            | DecodedPlanes::Yuva444P16 { width, height, .. } => YuvPlanes::Yuv444P16 {
                width: *width,
                height: *height,
                y: self.y(),
                cb: self.cb(),
                cr: self.cr(),
                a: self.a(),
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
        assert_eq!(PixFmt::Yuv422p.frame_bytes(3, 3), 21);
        assert_eq!(PixFmt::Yuv444p.frame_bytes(3, 3), 27);
        assert_eq!(PixFmt::Yuv420p16le.frame_bytes(3, 3), 34);
        assert_eq!(PixFmt::Yuv422p16le.frame_bytes(3, 3), 42);
    }

    #[test]
    fn pix_fmt_for_alpha() {
        assert_eq!(PixFmt::for_alpha(true), PixFmt::Yuva444p);
        assert_eq!(PixFmt::for_alpha(false), PixFmt::Yuv420p);
    }

    #[test]
    fn high_depth_source_keeps_sixteen_bit_planes() {
        for format in [
            "yuv420p",
            "yuv422p",
            "yuv444p",
            "yuva444p",
            "yuvj420p",
            "YUV422P10BE",
            "yuv444p16le",
            "nv12",
            "nv16",
            "nv24",
            "p010le",
            "p210le",
            "p416be",
            "yuyv422",
            "uyvy422",
        ] {
            assert!(PixFmt::supports_native_source(format), "{format}");
        }
        for format in [
            "gbrp16le",
            "rgb24",
            "yuv411p",
            "yuv420p18le",
            "yuv422p10xyz",
            "nv20",
            "p218le",
            "yuv420p10le-extra",
        ] {
            assert!(!PixFmt::supports_native_source(format), "{format}");
        }
        assert_eq!(
            PixFmt::for_source(Some("yuva444p12le"), true),
            PixFmt::Yuva444p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("yuv420p10le"), false),
            PixFmt::Yuv420p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("yuv422p10le"), false),
            PixFmt::Yuv422p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("p010le"), false),
            PixFmt::Yuv420p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("p012le"), false),
            PixFmt::Yuv420p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("P016LE"), false),
            PixFmt::Yuv420p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("YUV422P10BE"), false),
            PixFmt::Yuv422p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("yuv444p9le"), false),
            PixFmt::Yuv444p16le
        );
        assert_eq!(PixFmt::for_source(Some("yuv420p"), false), PixFmt::Yuv420p);
        assert_eq!(PixFmt::for_source(Some("yuv422p"), false), PixFmt::Yuv422p);
        assert_eq!(PixFmt::for_source(Some("yuv444p"), false), PixFmt::Yuv444p);
        assert_eq!(PixFmt::for_source(Some("nv16"), false), PixFmt::Yuv422p);
        assert_eq!(PixFmt::for_source(Some("nv24"), false), PixFmt::Yuv444p);
        assert_eq!(PixFmt::for_source(Some("yuyv422"), false), PixFmt::Yuv422p);
        assert_eq!(PixFmt::for_source(Some("uyvy422"), false), PixFmt::Yuv422p);
        assert_eq!(
            PixFmt::for_source(Some("p210le"), false),
            PixFmt::Yuv422p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("p212le"), false),
            PixFmt::Yuv422p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("p216be"), false),
            PixFmt::Yuv422p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("p410le"), false),
            PixFmt::Yuv444p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("p412le"), false),
            PixFmt::Yuv444p16le
        );
        assert_eq!(
            PixFmt::for_source(Some("p416be"), false),
            PixFmt::Yuv444p16le
        );
        assert_eq!(PixFmt::Yuva444p16le.frame_bytes(2, 1), 16);
        let samples = [4096u16, 60160, 32768, 32768, 32768, 32768, 0, 65535];
        let raw: Vec<u8> = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        let planes = DecodedPlanes::yuva444p16(2, 1, raw);
        assert_eq!(planes.y().len(), 4);
        assert_eq!(planes.cb().len(), 4);
        assert_eq!(planes.cr().len(), 4);
        assert_eq!(planes.a().unwrap().len(), 4);
        assert_eq!(u16::from_le_bytes([planes.y()[0], planes.y()[1]]), 4096);
        assert_eq!(u16::from_le_bytes([planes.y()[2], planes.y()[3]]), 60160);
        assert!(matches!(
            planes.as_yuv_planes(),
            YuvPlanes::Yuv444P16 { a: Some(_), .. }
        ));

        let codes: Vec<u16> = (0..17).collect();
        let raw: Vec<u8> = codes.iter().flat_map(|code| code.to_le_bytes()).collect();
        let subsampled = DecodedPlanes::yuv420p16(3, 3, raw);
        assert_eq!(subsampled.y().len(), 18);
        assert_eq!(subsampled.cb().len(), 8);
        assert_eq!(subsampled.cr().len(), 8);
        assert_eq!(
            u16::from_le_bytes(subsampled.cb()[..2].try_into().unwrap()),
            9
        );
        assert!(matches!(
            subsampled.as_yuv_planes(),
            YuvPlanes::Yuv420P16 { .. }
        ));
        let codes: Vec<u16> = (0..21).collect();
        let raw: Vec<u8> = codes.iter().flat_map(|code| code.to_le_bytes()).collect();
        let subsampled = DecodedPlanes::yuv422p16(3, 3, raw);
        assert_eq!(subsampled.y().len(), 18);
        assert_eq!(subsampled.cb().len(), 12);
        assert_eq!(subsampled.cr().len(), 12);
        assert_eq!(
            u16::from_le_bytes(subsampled.cb()[..2].try_into().unwrap()),
            9
        );
        assert!(matches!(
            subsampled.as_yuv_planes(),
            YuvPlanes::Yuv422P16 { .. }
        ));
        let subsampled = DecodedPlanes::yuv422(3, 3, (0..21).collect());
        assert_eq!(subsampled.y().len(), 9);
        assert_eq!(subsampled.cb().len(), 6);
        assert_eq!(subsampled.cr().len(), 6);
        assert!(matches!(
            subsampled.as_yuv_planes(),
            YuvPlanes::Yuv422 { .. }
        ));
        let full = DecodedPlanes::yuv444(3, 3, (0..27).collect());
        assert_eq!(full.y().len(), 9);
        assert_eq!(full.cb().len(), 9);
        assert_eq!(full.cr().len(), 9);
        assert!(matches!(full.as_yuv_planes(), YuvPlanes::Yuv444 { .. }));
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
