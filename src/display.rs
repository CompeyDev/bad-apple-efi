use thiserror::Error;
use uefi::{boot, println, proto::console::gop::GraphicsOutput};

use crate::pixel::Color;

/// An abstraction around the low-level framebuffer for drawing graphics. Typically
/// constructed by calling [`Display::open`] when boot services is available.
#[derive(Debug)]
pub struct Display<'a> {
    framebuffer: &'a mut [u32],
    pub width: usize,
    pub height: usize,
}

type Result<T, E = DisplayError> = core::result::Result<T, E>;
#[derive(Error, Debug)]
pub enum DisplayError {
    #[error("UEFI error: {0}")]
    Uefi(#[from] uefi::Error),
    #[error("No available display modes")]
    NoDisplayModes,
    #[error("Failed to draw at position ({x}, {y}): {reason}")]
    DrawError {
        x: usize,
        y: usize,
        reason: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct Frame {
    width: usize,
    height: usize,
}

impl<'a> Display<'a> {
    /// Creates a new Display instance from a framebuffer base pointer.
    pub fn new(
        framebuffer_base: *mut u32,
        framebuffer_size: usize,
        width: usize,
        height: usize,
    ) -> Display<'a> {
        Self {
            framebuffer: unsafe {
                core::slice::from_raw_parts_mut(framebuffer_base, framebuffer_size)
            },

            width,
            height,
        }
    }

    /// Opens the display by initializing the Graphics Output Protocol (GOP). **This method must be
    /// called before exiting boot services**.
    ///
    /// ## Errors
    /// - If called after exiting boot services, this will return a UEFI error.
    /// - If no display modes are available, this will return a `NoDisplayModes` error.
    pub fn open() -> Result<Display<'a>, DisplayError> {
        macro_rules! protected_uefi {
            ($expr:expr) => {
                match $expr {
                    Ok(val) => val,
                    Err(e) => return Err(DisplayError::Uefi(e)),
                }
            };
        }

        let gop_handle = protected_uefi!(boot::get_handle_for_protocol::<GraphicsOutput>());
        let mut gop = protected_uefi!(boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle));

        // TODO: Better scaling to use proper screen real-estate
        let mode = gop
            .modes()
            .min_by_key(|m| m.info().resolution())
            .ok_or(DisplayError::NoDisplayModes)?;

        let (width, height) = mode.info().resolution();
        protected_uefi!(gop.set_mode(&mode));

        let mut framebuffer = gop.frame_buffer();
        Ok(Self::new(
            framebuffer.as_mut_ptr() as *mut u32,
            framebuffer.size() / 4,
            width,
            height,
        ))
    }

    /// Draws content onto the display at specified (x, y) coordinates with given colors.
    ///
    /// ## Errors
    /// - If any (x, y) coordinate is out of bounds, this will return a `DrawError`.
    pub fn draw<I: Iterator<Item = (usize, usize, Color)>>(
        &mut self,
        content: I,
        frame: Frame,
    ) -> Result<()> {
        for (x, y, pixel) in content {
            if x >= frame.width || y >= frame.height {
                println!("Attempted to draw out of bounds at x={}, y={}", x, y);
                return Err(DisplayError::DrawError {
                    x,
                    y,
                    reason: "Out of bounds",
                });
            }

            let y_centered_offset = (self.height - frame.height) / 2;
            let x_centered_offset = (self.width - frame.width) / 2;
            let offset = ((y_centered_offset + y) * self.width) + (x_centered_offset + x);

            let color = if let Color::Rgb(r, g, b) = pixel {
                ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
            } else {
                pixel.into()
            };

            if offset < self.framebuffer.len() {
                // HACK: Prevent potential OOB writes
                self.framebuffer[offset] = color;
            }
        }

        Ok(())
    }

    pub fn clear(&mut self) {
        for pixel in self.framebuffer.iter_mut() {
            *pixel = Color::default().into();
        }
    }

    pub fn as_frame(&self) -> Frame {
        Frame {
            width: self.width,
            height: self.height,
        }
    }
}
