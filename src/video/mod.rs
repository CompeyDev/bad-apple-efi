use alloc::vec::Vec;
use core::ffi::c_int;

mod codec;
mod container;
mod playback;

pub use playback::VideoPlayer;

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum VideoError {
    #[error("libvpx error {0}")]
    Vpx(c_int),
    #[error("invalid container: {0}")]
    InvalidFormat(&'static str),
}

/// Something which consumes the decoded frames, usually a renderer.
pub trait FrameSink {
    type Error: From<VideoError>;

    fn present(&mut self, frame: &VideoFrame) -> Result<(), Self::Error>;
}

/// A decoded YUV frame. Chroma is only optionally available when the `high_res`
/// feature is enabled.
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub y: Plane,
    #[cfg(feature = "high_res")]
    pub u: Plane,
    #[cfg(feature = "high_res")]
    pub v: Plane,
    /// Chroma subsampling represented as x and y shifts. Common values:
    ///
    /// - `(1, 1)` = 4:2:0
    /// - `(1, 0)` = 4:2:2
    /// - `(0, 0)` = 4:4:4
    ///
    /// A chroma sample covers `1 << x` by `1 << y` luma samples.
    #[cfg(feature = "high_res")]
    pub chroma_shift: (u32, u32),
    #[cfg(feature = "high_res")]
    pub color: Colorimetry,
}

cfg_if::cfg_if! {
    if #[cfg(feature = "high_res")] {
        /// Describes how a frame's YUV values can be mapped to RGB.
        #[derive(Debug, Clone, Copy)]
        pub struct Colorimetry {
            pub matrix: ColorMatrix,
            pub range: ColorRange,
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum ColorMatrix {
            /// BT.601 / SMPTE 170M, also a fallback when unknown.
            Bt601,
            Bt709,
            Smpte240,
            Bt2020,
            /// Raw RGB stored as as G, B and R in the Y, U and V planes.
            Identity,
        }

        #[derive(Debug, Clone, Copy)]
        pub enum ColorRange {
            /// Luma in 16..=235, chroma in 16..=240 ("TV" range).
            Limited,
            /// All components in 0..=255 ("PC" range).
            Full,
        }
    }
}

/// The plane of an image.
///
/// Rows are `stride` bytes apart. Only the first `width` bytes are sampled.
pub struct Plane {
    pub data: Vec<u8>,
    pub width: usize,
    pub stride: usize,
}

impl Plane {
    /// Rows of the plane, without the stride padding.
    pub fn rows(&self) -> impl Iterator<Item = &[u8]> {
        self.data.chunks(self.stride).map(|row| &row[..self.width])
    }
}
