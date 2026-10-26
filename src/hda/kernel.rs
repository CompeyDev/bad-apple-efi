//! The kernel API uHDA expects its host to provide, see `uhda/kernel_api.h`.

use alloc::alloc::Layout;
use alloc::boxed::Box;
use core::ffi::{c_char, c_void, CStr};
use core::hint::spin_loop;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};
use core::time::Duration;

use x86_64::instructions::interrupts;
use x86_64::registers::rflags::{self, RFlags};

use crate::hda::bindings::*;
use crate::hda::pci::{Irq, PciDevice, PortAccess};
use crate::tsc::TSC;
use crate::{libc, memory};

const PAGE_SIZE: usize = 0x1000;
const SCATTER_ALIGNMENT: usize = 128;

//
// PCI
//

/// Recovers the device behind a handle passed to [`bindings::uhda_init`].
///
/// [`bindings::uhda_init`]: crate::hda::bindings::uhda_init
unsafe fn device<'a>(handle: usize) -> &'a PciDevice {
    unsafe { &*(handle as *const PciDevice) }
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_pci_read(
    handle: usize,
    offset: u8,
    size: u8,
    res: *mut u32,
) -> UhdaStatus {
    match PortAccess::read(unsafe { device(handle) }.address, offset, size) {
        Some(value) => {
            unsafe { res.write(value) };
            UhdaStatus::SUCCESS
        }
        None => UhdaStatus::UNSUPPORTED,
    }
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_pci_write(
    handle: usize,
    offset: u8,
    size: u8,
    value: u32,
) -> UhdaStatus {
    PortAccess::write(unsafe { device(handle) }.address, offset, size, value)
        .map_or(UhdaStatus::UNSUPPORTED, |()| UhdaStatus::SUCCESS)
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_pci_map_bar(
    handle: usize,
    bar: u32,
    virt: *mut *mut c_void,
) -> UhdaStatus {
    let address = u8::try_from(bar).ok().and_then(|bar| unsafe { device(handle) }.bar_address(bar));
    match address {
        Some(address) => {
            unsafe { virt.write(address as *mut c_void) };
            UhdaStatus::SUCCESS
        }
        None => UhdaStatus::UNSUPPORTED,
    }
}

#[no_mangle]
extern "C" fn uhda_kernel_pci_unmap_bar(_handle: usize, _bar: u32, _virt: *mut c_void) {}

// The handler is only ever polled, see `PciDevice::poll_irq`, so there is no vector or line to
// allocate. The device has a single IRQ, so `opaque_irq` is left unused
#[no_mangle]
unsafe extern "C" fn uhda_kernel_pci_allocate_irq(
    handle: usize,
    _hint: UhdaIrqHint,
    handler: UhdaIrqHandlerFn,
    arg: *mut c_void,
    opaque_irq: *mut *mut c_void,
) -> UhdaStatus {
    let device = unsafe { device(handle) };
    if device.disable_intx().is_none() {
        return UhdaStatus::UNSUPPORTED;
    }

    device.irq.set(Some(Irq { handler, arg, enabled: false }));
    unsafe { opaque_irq.write(ptr::null_mut()) };
    UhdaStatus::SUCCESS
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_pci_deallocate_irq(handle: usize, _opaque_irq: *mut c_void) {
    unsafe { device(handle) }.irq.set(None);
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_pci_enable_irq(
    handle: usize,
    _opaque_irq: *mut c_void,
    enabled: bool,
) {
    let irq = &unsafe { device(handle) }.irq;
    irq.set(irq.get().map(|irq| Irq { enabled, ..irq }));
}

//
// Memory
//

#[no_mangle]
extern "C" fn uhda_kernel_malloc(size: usize) -> *mut c_void {
    libc::malloc(size).cast()
}

// The size is ignored, `free` knows it from the allocation's header
#[no_mangle]
unsafe extern "C" fn uhda_kernel_free(ptr: *mut c_void, _size: usize) {
    libc::free(ptr.cast())
}

//
// DMA
//

/// Allocates uncacheable zeroed memory, or returns null for an empty or failed allocation.
fn allocate(size: usize, align: usize) -> *mut u8 {
    match Layout::from_size_align(size, align) {
        Ok(layout) if size != 0 => memory::dma::alloc(layout),
        _ => ptr::null_mut(),
    }
}

/// Frees memory from [`allocate`] with the same size and alignment.
unsafe fn deallocate(ptr: *mut u8, size: usize, align: usize) {
    match Layout::from_size_align(size, align) {
        Ok(layout) if !ptr.is_null() && size != 0 => unsafe { memory::dma::dealloc(ptr, layout) },
        _ => {}
    }
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_allocate_physical(
    _handle: usize,
    size: usize,
    res: *mut usize,
) -> UhdaStatus {
    match allocate(size, PAGE_SIZE) {
        ptr if ptr.is_null() => UhdaStatus::NO_MEMORY,
        ptr => {
            unsafe { res.write(ptr as usize) };
            UhdaStatus::SUCCESS
        }
    }
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_deallocate_physical(_handle: usize, phys: usize, size: usize) {
    unsafe { deallocate(phys as *mut u8, size, PAGE_SIZE) }
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_allocate_scatter(
    _handle: usize,
    count: usize,
    size: usize,
    res: *mut UhdaScatterChunk,
) -> UhdaStatus {
    for i in 0..count {
        let ptr = allocate(size, SCATTER_ALIGNMENT);
        if ptr.is_null() {
            unsafe { uhda_kernel_deallocate_scatter(res, i, size) };
            return UhdaStatus::NO_MEMORY;
        }

        unsafe { res.add(i).write(UhdaScatterChunk { phys: ptr as usize, virt: ptr.cast() }) };
    }

    UhdaStatus::SUCCESS
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_deallocate_scatter(
    chunks: *mut UhdaScatterChunk,
    count: usize,
    size: usize,
) {
    for i in 0..count {
        unsafe { deallocate((*chunks.add(i)).phys as *mut u8, size, SCATTER_ALIGNMENT) }
    }
}

// Physical memory is identity-mapped, and DMA memory is already uncacheable from the pool
#[no_mangle]
unsafe extern "C" fn uhda_kernel_map(
    _handle: usize,
    phys: usize,
    _size: usize,
    virt: *mut *mut c_void,
) -> UhdaStatus {
    unsafe { virt.write(phys as *mut c_void) };
    UhdaStatus::SUCCESS
}

#[no_mangle]
extern "C" fn uhda_kernel_unmap(_virt: *mut c_void, _size: usize) {}

//
// Synchronization
//

#[no_mangle]
unsafe extern "C" fn uhda_kernel_create_spinlock(spinlock: *mut *mut c_void) -> UhdaStatus {
    let lock = Box::into_raw(Box::new(AtomicBool::new(false)));
    unsafe { spinlock.write(lock.cast()) };
    UhdaStatus::SUCCESS
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_free_spinlock(spinlock: *mut c_void) {
    drop(unsafe { Box::from_raw(spinlock.cast::<AtomicBool>()) });
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_lock_spinlock(spinlock: *mut c_void) -> UhdaIrqState {
    let flags = rflags::read_raw();
    interrupts::disable();

    let lock = unsafe { &*spinlock.cast::<AtomicBool>() };
    while lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        spin_loop();
    }

    flags as UhdaIrqState
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_unlock_spinlock(spinlock: *mut c_void, irq_state: UhdaIrqState) {
    unsafe { &*spinlock.cast::<AtomicBool>() }.store(false, Ordering::Release);

    // Only re-enable interrupts if they were enabled before locking
    if RFlags::from_bits_truncate(irq_state as u64).contains(RFlags::INTERRUPT_FLAG) {
        interrupts::enable();
    }
}

//
// Miscellaneous
//

#[no_mangle]
extern "C" fn uhda_kernel_delay(microseconds: u32) {
    let ticks = TSC.to_ticks(Duration::from_micros(microseconds.into()));
    let end = TSC.now() + ticks.expect("TSC is calibrated before HDA is initialized");

    while TSC.now() < end {
        spin_loop();
    }
}

#[no_mangle]
unsafe extern "C" fn uhda_kernel_log(str: *const c_char) {
    let message = unsafe { CStr::from_ptr(str) }.to_string_lossy();
    log::debug!(target: "uhda", "{message}");
}
