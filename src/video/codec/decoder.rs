use core::{mem, ptr, slice};

use super::bindings::*;
#[cfg(feature = "high_res")]
use crate::video::{ColorMatrix, ColorRange, Colorimetry};
use crate::video::{Plane, VideoError, VideoFrame};

/// A singlethreaded libvpx VP9 decoder.
pub struct Vp9Decoder {
    ctx: VpxCodecCtx,
}

impl Vp9Decoder {
    pub fn new() -> Result<Self, VideoError> {
        let cfg = VpxCodecDecCfg { threads: 1, w: 0, h: 0 }; // dimensions inferred
        let mut ctx = unsafe { mem::zeroed() };
        unsafe { vpx_codec_dec_init(&mut ctx, vpx_codec_vp9_dx(), &cfg, 0) }.ok()?;

        log::debug!("VP9 decoder initialized");
        Ok(Self { ctx })
    }

    /// Decodes one compressed packet.
    ///
    /// Use [`Self::receive_frame`] to get the frame after calling this method.
    pub fn send_packet(&mut self, packet: &[u8]) -> Result<(), VideoError> {
        let len = packet.len() as u32;
        unsafe { vpx_codec_decode(&mut self.ctx, packet.as_ptr(), len, ptr::null_mut(), 0) }.ok()
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
