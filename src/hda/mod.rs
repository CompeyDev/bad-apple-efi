use alloc::boxed::Box;
use core::ffi::{c_char, c_int, CStr};
use core::ptr::{self, NonNull};

use owo_colors::OwoColorize;

use crate::hda::bindings::*;
use crate::hda::pci::PciDevice;

mod bindings;
mod kernel;
mod pci;
mod stream;

pub use stream::Stream;

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum HdaError {
    #[error("no HDA controller found")]
    NoController,
    #[error("no outputs found")]
    NoOutput,
    #[error("no output streams available")]
    NoStream,
    #[error("uHDA error {0}")]
    Uhda(c_int),
}

/// An initialized HDA controller and the codecs attached to it.
pub struct Hda {
    controller: NonNull<UhdaController>,
    device: Box<PciDevice>, // uHDA refers to device by address, heap allocated
}

impl Hda {
    /// Finds the first supported HDA controller on the PCI bus, and resets it along with its
    /// codecs.
    ///
    /// Requires [`TSC`](crate::tsc::TSC) to be calibrated.
    pub fn init() -> Result<Self, HdaError> {
        let device = Box::new(PciDevice::find_hda().ok_or(HdaError::NoController)?);

        let mut controller = ptr::null_mut();
        unsafe { uhda_init(&raw const *device as usize, &mut controller) }.ok()?;

        let controller = NonNull::new(controller).expect("uHDA didn't return a controller");
        let hda = Self { controller, device };

        for output in hda.outputs() {
            log::info!("Found output: {output}");
        }

        Ok(hda)
    }

    /// Opens an interleaved 16-bit PCM stream.
    ///
    /// See [`Stream::open`] for which output it is associated with.
    pub fn open_stream(&self, sample_rate: u32, channels: u32) -> Result<Stream<'_>, HdaError> {
        Stream::open(self, sample_rate, channels)
    }

    /// Lets the controller's streams move queued samples into the hardware buffer, instead of
    /// the interrupt handler.
    ///
    /// Streams stay 16 KiB ahead of the hardware, so this must be called at least that often,
    /// which would mean every 85ms for 48KHz 16-bit stereo audio.
    pub fn poll(&self) {
        self.device.poll_irq();
    }

    /// Lists the outputs of every codec, across all of their output groups.
    pub fn outputs(&self) -> impl Iterator<Item = Output<'_>> {
        let codecs = unsafe { util::list(self.controller.as_ptr(), uhda_get_codecs) };

        codecs
            .iter()
            .flat_map(|&codec| unsafe { util::list(codec, uhda_codec_get_output_groups) })
            .flat_map(|&group| unsafe { util::list(group, uhda_output_group_get_outputs) })
            .map(|&output| Output(unsafe { &*output }))
    }
}

impl Drop for Hda {
    fn drop(&mut self) {
        if let Err(err) = unsafe { uhda_destroy(self.controller.as_ptr()) }.ok() {
            log::warn!("Failed to shut the HDA controller down: {err}");
        }
    }
}

/// A physical output, e.g., a speaker or a headphone jack.
pub struct Output<'a>(&'a UhdaOutput);

impl Output<'_> {
    pub fn info(&self) -> UhdaOutputInfo {
        unsafe { uhda_output_get_info(self.0) }
    }

    /// Whether something is plugged into the output.
    pub fn is_present(&self) -> Option<bool> {
        let mut present = false;
        unsafe { uhda_output_get_presence(self.0, &mut present) }.ok().ok().map(|()| present)
    }
}

impl core::fmt::Display for Output<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let info = self.info();
        let name = |table: &[*const c_char; 16], index: c_int| {
            let name = table.get(index as usize).filter(|name| !name.is_null());
            name.map_or("unknown".into(), |&name| unsafe { CStr::from_ptr(name) }.to_string_lossy())
        };

        let (color, location) = unsafe {
            (
                name(&UHDA_OUTPUT_COLOR_STRINGS, info.color.0),
                name(&UHDA_LOCATION_STRINGS, info.location),
            )
        };

        let color = color.style(util::jack_style(info.color));
        write!(f, "{} ({color}, {location}", info.kind)?;
        match self.is_present() {
            Some(true) => write!(f, ", plugged in)"),
            Some(false) => write!(f, ", unplugged)"),
            None => write!(f, ")"),
        }
    }
}

mod util {
    use core::{ptr, slice};

    use owo_colors::{Style, XtermColors};

    use crate::hda::bindings::UhdaColor;

    pub fn jack_style(color: UhdaColor) -> Style {
        let (background, text) = match color {
            UhdaColor::BLACK => (16, 231),
            UhdaColor::GREY => (245, 16),
            UhdaColor::BLUE => (27, 231),
            UhdaColor::GREEN => (34, 16),
            UhdaColor::RED => (160, 231),
            UhdaColor::ORANGE => (208, 16),
            UhdaColor::YELLOW => (226, 16),
            UhdaColor::PURPLE => (92, 231),
            UhdaColor::PINK => (211, 16),
            UhdaColor::WHITE => (255, 16),
            _ => return Style::new(),
        };

        Style::new().on_color(XtermColors::from(background)).color(XtermColors::from(text))
    }

    pub unsafe fn list<'a, P, T>(
        parent: P,
        get: unsafe extern "C" fn(P, *mut *const *const T, *mut usize),
    ) -> &'a [*const T] {
        let (mut items, mut count) = (ptr::null(), 0);
        unsafe { get(parent, &mut items, &mut count) };

        match count {
            0 => &[],
            count => unsafe { slice::from_raw_parts(items, count) },
        }
    }
}
