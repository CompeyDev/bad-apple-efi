use dispi::{read, write, ID, IDS, VIDEO_MEMORY_64K, VIRT_HEIGHT, Y_OFFSET};

/// Two framebuffer pages stacked vertically in video memory, where one is shown while the other
/// is being drawn, then swapped, preventing a half-drawn frame from being displayed (i.e., screen
/// tearing).
///
/// Flipping goes through the [Bochs VBE / DISPI] interface, which QEMU's std VGA and VirtualBox
/// implement.
///
/// [Bochs VBE / DISPI]: https://wiki.osdev.org/Bochs_VBE_Extensions
#[derive(Debug)]
pub struct DoubleBuffer {
    height: u16,
    front: usize, // either 0 or 1, page being scanned out
}

impl DoubleBuffer {
    /// Sets up a second page below the visible one, each `page_size` bytes and `height` lines
    /// tall.
    ///
    /// Returns returns `None` if there is no Bochs VBE interface or not enough video memory.
    #[rustfmt::skip]
    pub fn new(height: usize, page_size: usize) -> Option<Self> {
        if !IDS.contains(&read(ID)) {
            return None;
        }

        let video_memory = usize::from(read(VIDEO_MEMORY_64K)) << 16;
        let height = u16::try_from(height).ok()?;
        let virt_height = height.checked_mul(2)?;
        if video_memory < page_size * 2 {
            log::debug!("Not enough video memory for two pages: {} KiB", video_memory / 1024);
            return None;
        }

        // Try setting the virtual height, but if the device does not accept our value as we need
        // it, restore the original value and disable double buffering
        if { write(VIRT_HEIGHT, virt_height); read(VIRT_HEIGHT) } < virt_height {
            write(VIRT_HEIGHT, height);
            return None;
        }

        log::info!("Display is double buffered");
        Some(Self { height, front: 0 })
    }

    /// The hidden page that we want to draw our frame to.
    pub fn back(&self) -> usize {
        self.front ^ 1
    }

    /// Swaps the front page for the back page once it has been finalized.
    pub fn flip(&mut self) {
        self.front ^= 1;
        write(Y_OFFSET, self.front as u16 * self.height);
    }
}

impl Drop for DoubleBuffer {
    fn drop(&mut self) {
        // Show the first page again
        write(Y_OFFSET, 0);
        write(VIRT_HEIGHT, self.height);
    }
}

// TODO: backends other than DISPI, potentially Intel

/// Access to the Bochs VBE registers through I/O ports.
mod dispi {
    use core::ops::RangeInclusive;

    use x86_64::instructions::port::Port;

    const INDEX_PORT: u16 = 0x1CE;
    const DATA_PORT: u16 = 0x1CF;

    // Register indices
    pub const ID: u16 = 0x0;
    pub const VIRT_HEIGHT: u16 = 0x7;
    pub const Y_OFFSET: u16 = 0x9;
    pub const VIDEO_MEMORY_64K: u16 = 0xA;

    /// Possible values of the [`ID`] register lie in this range
    pub const IDS: RangeInclusive<u16> = 0xB0C0..=0xB0C5;

    pub fn read(index: u16) -> u16 {
        // SAFETY: Bochs VBE registers are unused on other hardware
        unsafe {
            Port::new(INDEX_PORT).write(index);
            Port::new(DATA_PORT).read()
        }
    }

    pub fn write(index: u16, value: u16) {
        // SAFETY: same as read
        unsafe {
            Port::new(INDEX_PORT).write(index);
            Port::new(DATA_PORT).write(value);
        }
    }
}
