use core::ops::Range;

use thiserror::Error;
use uefi::boot;
use uefi::proto::console::gop::{GraphicsOutput, Mode, PixelFormat};

use crate::paging::{MemoryType, PageTables, SLOT_SIZE};
use crate::pixel::Color;
use double_buffer::DoubleBuffer;

mod double_buffer;

/// An abstraction around the low-level framebuffer for drawing graphics. Typically
/// constructed by calling [`Display::open`] when boot services is available.
#[derive(Debug)]
pub struct Display<'a> {
    framebuffer: &'a mut [u32],
    double_buffer: Option<DoubleBuffer>,

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
    /// Opens the display by initializing the Graphics Output Protocol (GOP). **This method must be
    /// called before exiting boot services**.
    ///
    /// ## Errors
    /// - If called after exiting boot services, this will return a UEFI error.
    /// - If no display modes are available, this will return a `NoDisplayModes` error.
    pub fn open() -> Result<Display<'a>, DisplayError> {
        let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>()?;
        let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle)?;

        let mode_count = gop.modes().count();
        let bgr_modes = gop
            .modes()
            // TODO: add support for other pixel formats too
            .filter(|mode| mode.info().pixel_format() == PixelFormat::Bgr);

        let native = gop.current_mode_info().resolution();
        let mode = Display::pick_mode(bgr_modes, native).ok_or(DisplayError::NoDisplayModes)?;

        let (width, height) = mode.info().resolution();
        let stride = mode.info().stride();
        if gop.current_mode_info().resolution() != (width, height) {
            gop.set_mode(&mode)?;
        }

        let mut framebuffer = gop.frame_buffer();
        let (base, size) = (framebuffer.as_mut_ptr() as *mut u32, framebuffer.size());

        log::info!("Display mode: {width}x{height}");
        log::debug!(
            "Picked mode out of {mode_count} modes: {:?} pixels, stride {stride}, {} KiB \
             framebuffer at {base:p}",
            mode.info().pixel_format(),
            size / 1024,
        );

        // The second page is after the framebuffer ends, in video memory
        let page = stride * height;
        let double_buffer = DoubleBuffer::new(height, page * 4);
        let len = match double_buffer {
            Some(_) => page * 2,
            None => {
                log::warn!("Display is not double buffered, expect some screen tearing");
                size / 4
            }
        };

        Ok(Self {
            // SAFETY: framebuffer stays valid even after boot services exiting, and the second
            // page fits in video memory
            framebuffer: unsafe { core::slice::from_raw_parts_mut(base, len) },
            double_buffer,
            width,
            height,
            stride,
        })
    }

    /// Picks the mode to use out of `modes`, given the `native` resolution.
    ///
    /// In QEMU, uploading the framebuffer has large overhead. To maintain a consistent framerate,
    /// we render to the smallest mode possible in order to minimize the total number of pixels.
    #[cfg(feature = "qemu")]
    fn pick_mode(modes: impl Iterator<Item = Mode>, _native: (usize, usize)) -> Option<Mode> {
        modes.min_by_key(|mode| mode.info().resolution())
    }

    /// Picks the mode to use out of `modes`, given the `native` resolution.
    #[cfg(not(feature = "qemu"))]
    fn pick_mode(modes: impl Iterator<Item = Mode>, native: (usize, usize)) -> Option<Mode> {
        // Maximum number of pixels that we can pick a mode for. Modes too large would
        // render as a slideshow without a GPU, therefore we limit it. Values derived by
        // testing in QEMU.
        let pixel_budget = if cfg!(feature = "high_res") { 1280 * 800 } else { 1920 * 1080 };
        let (native_width, native_height) = native;

        modes.max_by_key(|mode| {
            let (width, height) = mode.info().resolution();
            let pixels = width * height;

            // Prefer the same aspect ratio to prevent stretching
            if pixels <= pixel_budget {
                let same_aspect = width * native_height == height * native_width;
                (true, same_aspect, pixels)
            } else {
                (false, false, usize::MAX - pixels)
            }
        })
    }

    /// Returns the physical address range of the framebuffer.
    pub fn framebuffer_range(&self) -> Range<u64> {
        let start = self.framebuffer.as_ptr() as u64;
        start..start + core::mem::size_of_val(&*self.framebuffer) as u64
    }

    /// Makes the framebuffer write-combining, which improves performance. Failures are
    /// ignored.
    ///
    /// ## Safety
    /// Page tables should have been taken over first, i.e. [`UefiAllocatorManager::init`]
    /// should have been called before.
    pub fn set_write_combining(&mut self, tables: &mut PageTables, heap: Range<u64>) {
        let framebuffer = self.framebuffer_range();
        let start = framebuffer.start & !(SLOT_SIZE - 1);
        let end = framebuffer.end.next_multiple_of(SLOT_SIZE);

        // If the display's memory is on regular RAM, it can share a page with the heap, so to be
        // safe, we do not set it as write combining
        if start < heap.end && heap.start < end {
            log::warn!("Framebuffer shares memory with the heap, leaving it as is");
            return;
        }

        // SAFETY: the framebuffer only holds pixels adnd does not need caching
        match unsafe { tables.set_memory_type(start, end - start, MemoryType::WriteCombining) } {
            Ok(()) => log::info!("Framebuffer is write-combining"),
            Err(err) => {
                log::warn!("Framebuffer is not write-combining, drawing will be slow: {err}")
            }
        }
    }

    pub fn clear(&mut self) {
        self.fill(Color::default());
    }

    /// Fills the whole display with a color.
    pub fn fill(&mut self, color: Color) {
        let value: u32 = color.into();
        for pixel in self.framebuffer.iter_mut() {
            *pixel = value;
        }
    }

    /// Writes the frame onto the display, one row at a time and skipping any stride padding. If
    /// double buffered, it is drawn to the hidden page instead and then shown instead.
    ///
    /// `pixels` are `width * height` values in the display's `0x00RRGGBB` format.
    pub fn blit(&mut self, pixels: &[u32]) {
        let page = self.double_buffer.as_ref().map_or(0, DoubleBuffer::back);
        let framebuffer = &mut self.framebuffer[page * self.stride * self.height..];

        let rows = framebuffer.chunks_mut(self.stride).zip(pixels.chunks(self.width));
        for (dst, src) in rows {
            dst[..src.len()].copy_from_slice(src);
        }

        if let Some(double_buffer) = &mut self.double_buffer {
            double_buffer.flip();
        }
    }
}
