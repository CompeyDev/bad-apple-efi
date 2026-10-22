use alloc::vec;
use alloc::vec::Vec;

use fast_image_resize::images::Image;
use fast_image_resize::{ImageBufferError, PixelType, ResizeError, Resizer};

use crate::apic::ApicTimer;
use crate::display::Display;
use crate::pixel::Color;
use crate::tsc::TSC;
use crate::video::{FrameSink, VideoError, VideoFrame};

cfg_if::cfg_if! {
    if #[cfg(feature = "high_res")] {
        use core::iter;
        use crate::video::{ColorMatrix, ColorRange, Colorimetry};

        // No need to two tone map for a "retro" feeling on high res mode
        const PIXEL_TYPE: PixelType = PixelType::U8x3;
        const SHADE: fn(&[u8]) -> Color = |px| Color::Rgb(px[0], px[1], px[2]);
    } else {
        const PIXEL_TYPE: PixelType = PixelType::U8;
        const SHADE: fn(&[u8]) -> Color =
            |px| Color::Rgb(px[0], px[0], px[0]).to_two_tone(Color::Gray, Color::WHITE, 160);
    }
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum PipelineError {
    #[error("video error: {0}")]
    Video(#[from] VideoError),
    #[error("invalid source image: {0}")]
    Image(#[from] ImageBufferError),
    #[error("resize failed: {0}")]
    Resize(#[from] ResizeError),
}

/// Renders decoded frames to the display, paced to the video's framerate.
pub struct Pipeline<'a, 'd> {
    display: &'a mut Display<'d>,
    pacer: FramePacer<'a>,
    pixels: Vec<u8>,
    scaled: Image<'static>,
    resizer: Resizer,
    output: Vec<Color>,
}

impl<'a, 'd> Pipeline<'a, 'd> {
    pub fn new(display: &'a mut Display<'d>, timer: &'a ApicTimer, tsc_hz: u64, fps: u32) -> Self {
        let &mut Display { width, height, .. } = display;

        Self {
            display,
            pacer: FramePacer::new(timer, tsc_hz, fps),
            pixels: Vec::new(),
            scaled: Image::new(width as u32, height as u32, PIXEL_TYPE),
            resizer: Resizer::new(),
            output: vec![Color::default(); width * height],
        }
    }

    /// Converts a frame into display pixels in `output`, without writing to the display.
    fn render(&mut self, frame: &VideoFrame) -> Result<(), PipelineError> {
        // PERF: We allocate buffers once, and set their sizes on the initial frame,
        // then reuse them for the rest of the frames.
        let size = frame.width * frame.height * PIXEL_TYPE.size();
        if self.pixels.len() != size {
            self.pixels.resize(size, 0);
        }

        self.unpack(frame);

        let (width, height) = (frame.width as u32, frame.height as u32);
        let src = Image::from_slice_u8(width, height, &mut self.pixels, PIXEL_TYPE)?;
        self.resizer.resize(&src, &mut self.scaled, None)?;

        let pixels = self.scaled.buffer().chunks_exact(PIXEL_TYPE.size());
        for (out, px) in self.output.iter_mut().zip(pixels) {
            *out = SHADE(px);
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

impl FrameSink for Pipeline<'_, '_> {
    type Error = PipelineError;

    fn present(&mut self, frame: &VideoFrame) -> Result<(), PipelineError> {
        self.render(frame)?;

        // Waiting before the blit keeps the time between frames on screen steady
        self.pacer.wait();
        self.display.blit(&self.output);
        Ok(())
    }
}

/// Scheduler for frame rendering.
///
/// Keeps track of time consumed for decoding and rendering against the frametime
/// budget and wait times adjusts accordingly in order to not accumulate a time
/// error across multiple frames.
struct FramePacer<'a> {
    timer: &'a ApicTimer,
    period: u64,
    deadline: Option<u64>,
}

impl<'a> FramePacer<'a> {
    fn new(timer: &'a ApicTimer, tsc_hz: u64, fps: u32) -> Self {
        Self { timer, period: tsc_hz / u64::from(fps), deadline: None }
    }

    /// Blocks until the current frame's deadline, then schedules the next one.
    fn wait(&mut self) {
        let now = TSC.now();
        let deadline = *self.deadline.get_or_insert(now);

        if let Some(remaining) = deadline.checked_sub(now).and_then(|ticks| TSC.to_duration(ticks))
        {
            let millis = remaining.as_secs() * 1000 + u64::from(remaining.subsec_millis());
            self.timer.delay(millis as u32);
            // TODO: use APIC TSC timer mode.
        }

        // When more than a frame behind, resync instead of trying to catch up.
        self.deadline = Some(deadline.max(now.saturating_sub(self.period)) + self.period);
    }
}
