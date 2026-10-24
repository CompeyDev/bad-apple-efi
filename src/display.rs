use thiserror::Error;
use uefi::boot;
use uefi::proto::console::gop::GraphicsOutput;

use crate::pixel::Color;

/// An abstraction around the low-level framebuffer for drawing graphics. Typically
/// constructed by calling [`Display::open`] when boot services is available.
#[derive(Debug)]
pub struct Display<'a> {
    framebuffer: &'a mut [u32],
    pub width: usize,
    pub height: usize,
    stride: usize,
}

type Result<T, E = DisplayError> = core::result::Result<T, E>;
#[derive(Error, Debug)]
pub enum DisplayError {
    #[error("UEFI error: {0}")]
    Uefi(#[from] uefi::Error),
    #[error("No available display modes")]
    NoDisplayModes,
}

impl<'a> Display<'a> {
    /// Creates a new Display instance from a framebuffer base pointer.
    pub fn new(
        framebuffer_base: *mut u32,
        framebuffer_size: usize,
        width: usize,
        height: usize,
        stride: usize,
    ) -> Display<'a> {
        Self {
            framebuffer: unsafe {
                core::slice::from_raw_parts_mut(framebuffer_base, framebuffer_size)
            },

            width,
            height,
            stride,
        }
    }

    /// Opens the display by initializing the Graphics Output Protocol (GOP). **This method must be
    /// called before exiting boot services**.
    ///
    /// ## Errors
    /// - If called after exiting boot services, this will return a UEFI error.
    /// - If no display modes are available, this will return a `NoDisplayModes` error.
    pub fn open() -> Result<Display<'a>, DisplayError> {
        let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>()?;
        let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle)?;

        // TODO: Better scaling to use proper screen real-estate
        let mode_count = gop.modes().count();
        let mode = gop
            .modes()
            .min_by_key(|m| m.info().resolution())
            .ok_or(DisplayError::NoDisplayModes)?;

        let (width, height) = mode.info().resolution();
        let stride = mode.info().stride();
        gop.set_mode(&mode)?;

        let mut framebuffer = gop.frame_buffer();
        let (base, len) = (framebuffer.as_mut_ptr() as *mut u32, framebuffer.size() / 4);

        log::info!("Display mode: {width}x{height}");
        log::debug!(
            "Picked mode out of {mode_count} modes: {:?} pixels, stride {stride}, {} KiB \
             framebuffer at {base:p}",
            mode.info().pixel_format(),
            framebuffer.size() / 1024,
        );

        Ok(Self::new(base, len, width, height, stride))
    }

    pub fn clear(&mut self) {
        self.fill(Color::default());
    }

    pub fn fill(&mut self, color: Color) {
        for pixel in self.framebuffer.iter_mut() {
            *pixel = color.into();
        }
    }

    /// Directly writes the frame onto the display, copying its bytes row by row, skipping
    /// any stride padding.
    pub fn blit(&mut self, pixels: &[Color]) {
        let rows = self.framebuffer.chunks_mut(self.stride).zip(pixels.chunks(self.width));
        for (dst, src) in rows {
            for (d, &c) in dst.iter_mut().zip(src) {
                *d = c.into();
            }
        }
    }
}
