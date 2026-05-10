use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::asm::*;

/// The global Interrupt Descriptor Table.
pub static IDT: Idt = Idt::new();

const IDT_ENTRIES: usize = 256;

/// Represents an x86_64 IDT entry (16 bytes total).
///
/// ## Layout
/// - `offset_low`  - bits 0–15
/// - `selector`    - bits 16–31
/// - `ist`         - bits 32–34
/// - `attributes`  - bits 40–47
/// - `offset_mid`  - bits 48–63
/// - `offset_high` - bits 64–95
/// - `reserved`    - bits 96–127
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct IdtEntry {
    /// Low 16 bits of the handler virtual address.
    pub offset_low: u16,
    /// GDT code segment (`0x08` for kernel code).
    pub selector: u16,
    /// Interrupt Stack Table index (`0` = current stack).
    pub ist: u8,
    /// Gate type, DPL, and present flag.
    pub attributes: u8,
    /// Middle 16 bits of the handler virtual address.
    pub offset_mid: u16,
    /// High 32 bits of the handler virtual address.
    pub offset_high: u32,
    /// Reserved field. Must be zero.
    pub reserved: u32,
}

impl IdtEntry {
    pub const fn new() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            attributes: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        }
    }
}

/// The Interrupt Descriptor Table.
pub struct Idt {
    entries: UnsafeCell<[IdtEntry; IDT_ENTRIES]>,
}

impl Idt {
    pub const fn new() -> Self {
        Self { entries: UnsafeCell::new([IdtEntry::new(); IDT_ENTRIES]) }
    }

    pub fn load(&self) {
        unsafe { init_pic() };

        #[repr(C, packed)]
        struct Idtr {
            limit: u16,
            base: u64,
        }

        unsafe {
            let entries_ptr = self.entries.get();
            let idtr = Idtr {
                limit: (core::mem::size_of::<IdtEntry>() * IDT_ENTRIES - 1) as u16,
                base: entries_ptr as u64,
            };

            asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags));
        }
    }

    #[rustfmt::skip]
    pub fn register_handler(&self, vector: u8, handler: extern "C" fn()) {
        let handler_addr = handler as usize;
        let entries = self.entries.get();
        let entry = unsafe { &mut (*entries)[vector as usize] };

        entry.offset_low  = (handler_addr & 0xFFFF) as u16;          // address bits 0–15
        entry.selector    = 0x08;                                    // kernel 64-bit code segment
        entry.ist         = 0;                                       // do not switch stacks
        entry.attributes  = 0x8E;                                    // present | ring 0 | 64-bit interrupt gate
        entry.offset_mid  = ((handler_addr >> 16) & 0xFFFF) as u16;  // address bits 16–31
        entry.offset_high = (handler_addr >> 32) as u32;             // address bits 32–63
        entry.reserved    = 0;                                       // must be zero
    }
}

// SAFETY: IDT must only be modified in a single-threaded context during boot
unsafe impl Sync for Idt {}

//
// Interrupt Request (IRQ)
//

/// A synchronization primitive for waiting on interrupt completion.
pub struct InterruptSemaphore {
    completed: AtomicBool,
}

impl InterruptSemaphore {
    pub const fn new() -> Self {
        Self { completed: AtomicBool::new(false) }
    }

    /// Wait for the interrupt to complete. Blocks until signal() is called.
    pub fn wait(&self) {
        // Reset state
        self.completed.store(false, Ordering::SeqCst);

        unsafe {
            // Enable interrupts
            sti();

            // Halt execution until an interrupt is received
            while !self.completed.load(Ordering::SeqCst) {
                hlt();
            }

            // Disable interrupts
            cli();
        }
    }

    /// Signal that the interrupt has completed. Called from interrupt handler.
    pub fn release(&self) {
        self.completed.store(true, Ordering::SeqCst);
    }

    /// Reset the completion flag before waiting.
    pub fn reset(&self) {
        self.completed.store(false, Ordering::SeqCst);
    }

    /// Get the memory offset of the underlying atomic boolean.
    pub const fn completed_offset(&self) -> usize {
        core::mem::offset_of!(InterruptSemaphore, completed)
    }
}

//
// Programmable Interrupt Controller (PIC)
//

/// Initialize the PIC and mask all IRQs for safe APIC operation.
pub unsafe fn init_pic() {
    // ICW1: initialize
    outb(0x20, 0x11); // master: init + ICW4 expected
    outb(0xA0, 0x11); // slave:  init + ICW4 expected

    // ICW2: vector offsets
    outb(0x21, 0x20); // master: IRQ[0-7]  = vectors 20h-27h
    outb(0xA1, 0x28); // slave:  IRQ[8-15] = vectors 28h-2Fh

    // ICW3: cascade
    outb(0x21, 0x04); // master: slave on IRQ2
    outb(0xA1, 0x02); // slave:  casade ID 2

    // ICW4: set both master and slave modes to 8086
    outb(0x21, 0x01); // master
    outb(0xA1, 0x01); // slave

    // Mask all IRQs
    outb(0x21, 0xFF); // master
    outb(0xA1, 0xFF); // slave
    outb(0x20, 0x60); // send EOI to all pending IRQs
}
