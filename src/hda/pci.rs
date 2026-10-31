use core::cell::Cell;
use core::ffi::c_void;

use pci_types::{ConfigRegionAccess, PciAddress, PciHeader};
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::hda::bindings::{uhda_class_matches, uhda_device_matches, UhdaIrqHandlerFn};

const CONFIG_ADDRESS: u16 = 0xCF8;
const CONFIG_DATA: u16 = 0xCFC;
const COMMAND: u8 = 0x04;
const COMMAND_INTX_DISABLE: u32 = 1 << 10;
const BARS: u8 = 0x10;

/// Makes selecting a register and accessing it through the address and data port pair atomic.
static CONFIG_LOCK: Mutex<()> = Mutex::new(());

/// Configuration space access through the legacy I/O ports.
///
/// Unlike the UEFI root bridge protocol, this works after exiting boot services. It reaches the
/// first 256 bytes of every function on segment 0, which is all HDA needs.
#[derive(Clone, Copy)]
pub struct PortAccess;

impl PortAccess {
    /// Reads `size` bytes at `offset`, which must be naturally aligned.
    pub fn read(address: PciAddress, offset: u8, size: u8) -> Option<u32> {
        let _guard = CONFIG_LOCK.lock();
        let port = Self::select(address, offset);

        // SAFETY: the data port exposes the selected DWORD at any aligned offset within it
        unsafe {
            match size {
                1 => Some(Port::<u8>::new(port).read().into()),
                2 => Some(Port::<u16>::new(port).read().into()),
                4 => Some(Port::<u32>::new(port).read()),
                _ => None,
            }
        }
    }

    /// Writes the low `size` bytes of `value` at `offset`, which must be naturally aligned.
    pub fn write(address: PciAddress, offset: u8, size: u8, value: u32) -> Option<()> {
        let _guard = CONFIG_LOCK.lock();
        let port = Self::select(address, offset);

        // SAFETY: Same as reads. Writes only touch the requested bytes, since writing the
        // whole DWORD would also write back the status register, clearing its bits
        unsafe {
            match size {
                1 => Port::new(port).write(value as u8),
                2 => Port::new(port).write(value as u16),
                4 => Port::new(port).write(value),
                _ => return None,
            }
        }

        Some(())
    }

    /// Points the data port at the DWORD holding `offset`, returning the port for it.
    fn select(address: PciAddress, offset: u8) -> u16 {
        let (bus, dev, fun) = (address.bus(), address.device(), address.function());
        let selector = 1 << 31 // enable bit
            | u32::from(bus) << 16
            | u32::from(dev) << 11
            | u32::from(fun) << 8
            | u32::from(offset & !0b11);

        // SAFETY: only selects a register, the access happens through the data port
        unsafe { Port::new(CONFIG_ADDRESS).write(selector) };
        CONFIG_DATA + u16::from(offset & 0b11)
    }
}

impl ConfigRegionAccess for PortAccess {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        Self::read(address, offset as u8, 4).unwrap_or(u32::MAX)
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        Self::write(address, offset as u8, 4, value);
    }
}

/// An interrupt handler registered by uHDA for a device.
#[derive(Clone, Copy)]
pub struct Irq {
    pub handler: UhdaIrqHandlerFn,
    pub arg: *mut c_void,
    pub enabled: bool,
}

/// A PCI function, which uHDA returns an opaque handle pointing to.
pub struct PciDevice {
    /// The PCI function's address.
    pub address: PciAddress,
    /// Not raised by the device, see [`PciDevice::poll_irq`].
    pub irq: Cell<Option<Irq>>,
}

impl PciDevice {
    /// Scans every bus on segment 0 looking for a PCI function uHDA can drive.
    pub fn find_hda() -> Option<Self> {
        let functions = (0..=u8::MAX).flat_map(|bus| {
            (0..32).flat_map(move |dev| {
                // Fn0 is guaranteed to exist for a device
                let header = PciHeader::new(PciAddress::new(0, bus, dev, 0));
                let count = match header.id(PortAccess) {
                    (0xFFFF, _) => 0,

                    // Fn0 always reports 1 unless there are multiple functions
                    _ if header.has_multiple_functions(PortAccess) => 8,
                    _ => 1,
                };

                (0..count).map(move |fun| PciHeader::new(PciAddress::new(0, bus, dev, fun)))
            })
        });

        functions
            .filter(|header| header.id(PortAccess).0 != 0xFFFF)
            .find(|header| {
                let (vendor, device) = header.id(PortAccess);
                let (_, class, subclass, _) = header.revision_and_class(PortAccess);

                // SAFETY: both are just lookups
                unsafe {
                    uhda_device_matches(vendor, device)
                        || uhda_class_matches(class.into(), subclass.into())
                }
            })
            .map(|header| {
                let (vendor, device) = header.id(PortAccess);
                log::debug!("HDA controller {vendor:04x}:{device:04x} at {}", header.address());
                Self { address: header.address(), irq: Cell::new(None) }
            })
    }

    /// Stops the device from raising legacy interrupts.
    ///
    /// We poll its interrupt status ourselves and dispatch a handler manually. See [`PciDevice::poll_irq`].
    pub fn disable_intx(&self) -> Option<()> {
        let command = PortAccess::read(self.address, COMMAND, 2)?;
        PortAccess::write(self.address, COMMAND, 2, command | COMMAND_INTX_DISABLE)
    }

    /// Runs our interrupt handler by polling the device's interrupt status. The handler must
    /// check the controller's interrupt status, so polling when nothing happened is fine.
    ///
    /// Must be called often enough that streams never run out of buffered samples.
    pub fn poll_irq(&self) {
        if let Some(Irq { handler, arg, enabled: true }) = self.irq.get() {
            // SAFETY: uHDA keeps the argument valid until it frees the IRQ
            unsafe { handler(arg) };
        }
    }

    /// Returns the address a memory BAR decodes, which is both physical and virtual since UEFI
    /// identity maps is a no-op.
    ///
    /// This avoids [`EndpointHeader::bar`](pci_types::EndpointHeader::bar), which sizes the
    /// BAR by writing to it while the device may still be decoding through it.
    pub fn bar_address(&self, bar: u8) -> Option<u64> {
        let register = BARS + bar * 4;
        let low = PortAccess::read(self.address, register, 4)?;

        // Whether it is 64-bit BAR, upper half is the next register
        let high = match (low >> 1) & 0b11 {
            0b10 => PortAccess::read(self.address, register + 4, 4)?,
            _ => 0,
        };

        Some((u64::from(high) << 32) | u64::from(low & !0xF))
    }
}
