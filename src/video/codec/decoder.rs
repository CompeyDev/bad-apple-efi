use core::ffi::{c_int, CStr};
use core::ptr::{self, NonNull};
use core::time::Duration;
use core::{mem, slice};

use super::bindings::opus::*;
use super::bindings::vpx::*;
use crate::video::container::Packet;
#[cfg(feature = "high_res")]
use crate::video::{ColorMatrix, ColorRange, Colorimetry};
use crate::video::{Plane, VideoError, VideoFrame};

/// A singlethreaded libvpx VP9 decoder.
pub struct Vp9Decoder {
    ctx: VpxCodecCtx,
    pts: Duration, // timestamp of the last packet sent
}

impl Vp9Decoder {
    pub fn new() -> Result<Self, VideoError> {
        let cfg = VpxCodecDecCfg { threads: 1, w: 0, h: 0 }; // dimensions inferred
        let mut ctx = unsafe { mem::zeroed() };
        unsafe { vpx_codec_dec_init(&mut ctx, vpx_codec_vp9_dx(), &cfg, 0) }.ok()?;

        log::debug!("VP9 decoder initialized");
        Ok(Self { ctx, pts: Duration::ZERO })
    }

    /// Decodes one compressed packet.
    ///
    /// Use [`Self::receive_frame`] to get the frame after calling this method.
    pub fn send_packet(&mut self, packet: &Packet) -> Result<(), VideoError> {
        let (data, len) = (packet.data.as_ptr(), packet.data.len() as u32);
        self.pts = packet.pts;
        unsafe { vpx_codec_decode(&mut self.ctx, data, len, ptr::null_mut(), 0) }
            .ok()
            .inspect_err(|_| self.log_error_detail())
    }

    /// Logs the last libvpx error's details, if any.
    fn log_error_detail(&self) {
        let detail = unsafe { vpx_codec_error_detail(&self.ctx) };
        if !detail.is_null() {
            let detail = unsafe { CStr::from_ptr(detail) };
            log::debug!(target: "libvpx", "{}", detail.to_string_lossy());
        }
    }

    /// Copies out the most recently decoded frame, if successfully produced.
    pub fn receive_frame(&mut self) -> Option<VideoFrame> {
        let img = unsafe { vpx_codec_get_frame(&mut self.ctx, &mut ptr::null()).as_ref()? };
        let (width, height) = (img.d_w as usize, img.d_h as usize);

        let plane = |i: usize, width: usize, rows: usize| {
            let stride = img.stride[i] as usize;
            let data = unsafe { slice::from_raw_parts(img.planes[i], stride * rows) }.to_vec();
            Plane { data, width, stride }
        };

        // Chroma planes are the luma size divided by the subsampling factor, rounded up
        #[cfg(feature = "high_res")]
        let (xs, ys) = (img.x_chroma_shift, img.y_chroma_shift);
        #[cfg(feature = "high_res")]
        let (chroma_w, chroma_h) = ((width + (1 << xs) - 1) >> xs, (height + (1 << ys) - 1) >> ys);

        Some(VideoFrame {
            pts: self.pts,
            width,
            height,
            y: plane(0, width, height),
            #[cfg(feature = "high_res")]
            u: plane(1, chroma_w, chroma_h),
            #[cfg(feature = "high_res")]
            v: plane(2, chroma_w, chroma_h),
            #[cfg(feature = "high_res")]
            chroma_shift: (xs, ys),
            #[cfg(feature = "high_res")]
            color: Colorimetry { matrix: img.cs.into(), range: img.range.into() },
        })
    }
}

#[cfg(feature = "high_res")]
impl From<VpxColorSpace> for ColorMatrix {
    fn from(cs: VpxColorSpace) -> Self {
        match cs {
            VpxColorSpace::BT_709 => Self::Bt709,
            VpxColorSpace::SMPTE_240 => Self::Smpte240,
            VpxColorSpace::BT_2020 => Self::Bt2020,
            VpxColorSpace::SRGB => Self::Identity,
            _ => Self::Bt601,
        }
    }
}

#[cfg(feature = "high_res")]
impl From<VpxColorRange> for ColorRange {
    fn from(range: VpxColorRange) -> Self {
        if range == VpxColorRange::FULL {
            Self::Full
        } else {
            Self::Limited
        }
    }
}

impl Drop for Vp9Decoder {
    fn drop(&mut self) {
        let _ = unsafe { vpx_codec_destroy(&mut self.ctx) };
    }
}

/// A libopus decoder, producing interleaved 16-bit samples at 48 kHz.
pub struct OpusDecoder {
    state: NonNull<OpusDecoderState>,
    channels: usize,
}

impl OpusDecoder {
    pub const SAMPLE_RATE: u32 = 48_000;
    pub const MAX_FRAME_SIZE: usize = 5760;

    /// Creates a decoder which mixes the track to `channels`.
    pub fn new(channels: u8) -> Result<Self, VideoError> {
        let mut error = 0;
        let rate = Self::SAMPLE_RATE as i32;
        let state = unsafe { opus_decoder_create(rate, channels.into(), &mut error) };
        let state = NonNull::new(state).ok_or(VideoError::Opus(error))?;

        log::debug!("Opus decoder initialized");
        Ok(Self { state, channels: channels.into() })
    }

    /// Decodes one compressed packet into `pcm`, returning how many samples per channel
    /// were written.
    ///
    /// Errors when `pcm` is larger than [`Self::MAX_FRAME_SIZE`].
    pub fn decode(&mut self, packet: &[u8], pcm: &mut [i16]) -> Result<usize, VideoError> {
        let (data, len) = (packet.as_ptr(), packet.len() as i32);
        let frame_size = (pcm.len() / self.channels) as c_int;

        let decoded =
            unsafe { opus_decode(self.state.as_ptr(), data, len, pcm.as_mut_ptr(), frame_size, 0) };

        usize::try_from(decoded).map_err(|_| VideoError::Opus(decoded))
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        unsafe { opus_decoder_destroy(self.state.as_ptr()) };
    }
}
