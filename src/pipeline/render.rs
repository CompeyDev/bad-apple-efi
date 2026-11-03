use alloc::vec;
use alloc::vec::Vec;

use fast_image_resize::images::Image;
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use super::PipelineError;
use crate::video::VideoFrame;

cfg_if::cfg_if! {
    if #[cfg(feature = "high_res")] {
        use core::iter;
        use crate::video::{ColorMatrix, ColorRange, Colorimetry};

        const PIXEL_TYPE: PixelType = PixelType::U8x3;
        const FILTER: FilterType = FilterType::Lanczos3;

        /// Packs a row of RGB samples into display pixels.
        #[inline(always)]
        fn shade_row(dst: &mut [u32], src: &[u8]) {
            for (out, px) in dst.iter_mut().zip(src.chunks_exact(PIXEL_TYPE.size())) {
                *out = u32::from(px[0]) << 16 | u32::from(px[1]) << 8 | u32::from(px[2]);
            }
        }
    } else {
        use crate::pixel::Color;

        const PIXEL_TYPE: PixelType = PixelType::U8;
        const FILTER: FilterType = FilterType::Bilinear;

        /// Two tone maps a row of gray samples into display pixels.
        #[inline(always)]
        fn shade_row(dst: &mut [u32], src: &[u8]) {
            const THRESHOLD: u8 = 160;

            let (dark, light): (u32, u32) = (Color::Gray.into(), Color::WHITE.into());
            for (out, &gray) in dst.iter_mut().zip(src) {
                *out = if gray >= THRESHOLD { light } else { dark };
            }
        }
    }
}

/// Converts decoded video frames into display pixels, reusing its buffers across frames.
pub struct Renderer {
    pixels: Vec<u8>,
    scaled: Image<'static>,
    resizer: Resizer,
    options: ResizeOptions,
    display: (usize, usize),
    output: Vec<u32>,
}

impl Renderer {
    pub fn new(width: usize, height: usize) -> Self {
        let resizer = Resizer::new();

        Self {
            pixels: Vec::new(),
            scaled: Image::new(width as u32, height as u32, PIXEL_TYPE),
            resizer,
            options: ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FILTER)),
            display: (width, height),
            output: vec![0; width * height],
        }
    }

    /// The pixels of the last rendered frame, as `0x00RRGGBB` for the whole display.
    pub fn output(&self) -> &[u32] {
        &self.output
    }

    /// Converts a frame into display pixels in `output`, without writing to the display.
    pub fn render(&mut self, frame: &VideoFrame) -> Result<(), PipelineError> {
        // PERF: We allocate buffers once, and set their sizes on the initial frame,
        // then reuse them for the rest of the frames.
        let size = frame.width * frame.height * PIXEL_TYPE.size();
        if self.pixels.len() != size {
            self.pixels.resize(size, 0);
            self.scaled = Image::new(self.display.0 as u32, self.display.1 as u32, PIXEL_TYPE);

            log::debug!(
                "Video frames scaled from {}x{} to {}x{}",
                frame.width,
                frame.height,
                self.display.0,
                self.display.1,
            );
        }

        self.unpack(frame);

        let (width, height) = (frame.width as u32, frame.height as u32);
        let src = Image::from_slice_u8(width, height, &mut self.pixels, PIXEL_TYPE)?;
        self.resizer.resize(&src, &mut self.scaled, &self.options)?;

        let (width, _) = self.display;
        let src = self.scaled.buffer().chunks_exact(width * PIXEL_TYPE.size());
        for (dst, src) in self.output.chunks_exact_mut(width).zip(src) {
            shade_row(dst, src);
        }

        Ok(())
    }

    #[cfg(feature = "high_res")]
    fn unpack(&mut self, frame: &VideoFrame) {
        let (xs, ys) = frame.chroma_shift;
        let chroma_rows = // each row covers `1 << ys` luma rows
            frame.u.rows().zip(frame.v.rows()).flat_map(|uv| iter::repeat_n(uv, 1 << ys));

        let yuv = YuvToRgb::new(frame.color);
        for ((dst, y_row), (u_row, v_row)) in
            self.pixels.chunks_exact_mut(frame.width * 3).zip(frame.y.rows()).zip(chroma_rows)
        {
            for (x, (rgb, &y)) in dst.chunks_exact_mut(3).zip(y_row).enumerate() {
                rgb.copy_from_slice(&yuv.convert(y, u_row[x >> xs], v_row[x >> xs]));
            }
        }
    }

    #[cfg(not(feature = "high_res"))]
    fn unpack(&mut self, frame: &VideoFrame) {
        let w = frame.width;
        for (dst, src) in self.pixels.chunks_exact_mut(w).zip(frame.y.rows()) {
            dst.copy_from_slice(src);
        }
    }
}

cfg_if::cfg_if! {
    if #[cfg(feature = "high_res")] {
        /// YUV to RGB conversion in 16.16 fixed point.
        struct YuvToRgb {
            /// Whether samples are already RGB format, stored as G, B, R.
            gbr: bool,
            y_offset: i32,
            y: i32,
            rv: i32,
            gu: i32,
            gv: i32,
            bu: i32,
        }

        impl YuvToRgb {
            fn new(color: Colorimetry) -> Self {
                // Luma weights of the red and blue primaries from the Y'CbCr model
                let (kr, kb) = match color.matrix {
                    ColorMatrix::Bt601 => (0.299, 0.114),
                    ColorMatrix::Bt709 => (0.2126, 0.0722),
                    ColorMatrix::Smpte240 => (0.212, 0.087),
                    ColorMatrix::Bt2020 => (0.2627, 0.0593),
                    ColorMatrix::Identity => (0.0, 0.0), // we use the gbr field instead
                };
                let kg = 1.0 - kr - kb;

                // Limited range values are stretched back
                let (y_offset, y_scale, c_scale) = match color.range {
                    ColorRange::Limited => (16, 255.0 / 219.0, 255.0 / 224.0),
                    ColorRange::Full => (0, 1.0, 1.0),
                };

                // Q16.16 fixed point conversion
                let fixed = |x: f64| (x * 65536.0 + 0.5) as i32;
                Self {
                    gbr: color.matrix == ColorMatrix::Identity,
                    y_offset,
                    y: fixed(y_scale),
                    rv: fixed(2.0 * (1.0 - kr) * c_scale),
                    gu: fixed(2.0 * kb * (1.0 - kb) / kg * c_scale),
                    gv: fixed(2.0 * kr * (1.0 - kr) / kg * c_scale),
                    bu: fixed(2.0 * (1.0 - kb) * c_scale),
                }
            }

            fn convert(&self, y: u8, u: u8, v: u8) -> [u8; 3] {
                if self.gbr {
                    return [v, y, u];
                }

                let y = (i32::from(y) - self.y_offset) * self.y + (1 << 15); // added 0.5 to force round up
                let (u, v) = (i32::from(u) - 128, i32::from(v) - 128);
                let channel = |c: i32| (c >> 16).clamp(0, 255) as u8;

                [channel(y + self.rv * v), channel(y - self.gu * u - self.gv * v), channel(y + self.bu * u)]
            }
        }
    }
}
