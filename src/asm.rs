use core::arch::asm;
pub use core::arch::x86_64::{_mm_mfence as mfence, _rdtsc as rdtsc};

//
// Port I/O
//

/// Read a byte from the given I/O port.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
    value
}

/// Write a byte to the given I/O port.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("al") value, in("dx") port, options(nomem, nostack, preserves_flags));
}

//
// CPU Control
//

/// Halt the CPU until the next interrupt fires.
#[inline]
pub unsafe fn hlt() {
    asm!("hlt", options(nomem, nostack, preserves_flags));
}

/// Disable maskable interrupts.
#[inline]
pub unsafe fn cli() {
    asm!("cli", options(nomem, nostack, preserves_flags));
}

/// Enable maskable interrupts.
#[inline]
pub unsafe fn sti() {
    asm!("sti", options(nomem, nostack, preserves_flags));
}
