use core::arch::asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{ExceptionVector, InterruptStackFrame};

use crate::apic::ApicTimer;
use crate::segments::DOUBLE_FAULT_IST;

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
        self.register_exceptions();

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

    pub fn register_handler(&self, vector: u8, handler: extern "C" fn()) {
        self.set_gate(vector, handler as *const (), 0);
    }

    /// Points `vector` at `handler`, switching to the interrupt stack `ist` from the TSS if it's
    /// not zero.
    #[rustfmt::skip]
    fn set_gate(&self, vector: u8, handler: *const (), ist: u8) {
        let handler_addr = handler as usize;
        let entries = self.entries.get();
        let entry = unsafe { &mut (*entries)[vector as usize] };

        entry.offset_low  = (handler_addr & 0xFFFF) as u16;          // address bits 0–15
        entry.selector    = 0x08;                                    // kernel 64-bit code segment
        entry.ist         = ist;                                     // interrupt stack, 0 for current
        entry.attributes  = 0x8E;                                    // present | ring 0 | 64-bit interrupt gate
        entry.offset_mid  = ((handler_addr >> 16) & 0xFFFF) as u16;  // address bits 16–31
        entry.offset_high = (handler_addr >> 32) as u32;             // address bits 32–63
        entry.reserved    = 0;                                       // must be zero
    }
}

// SAFETY: IDT must only be modified in a single-threaded context during boot
unsafe impl Sync for Idt {}

//
// CPU Exceptions
//

const DOUBLE_FAULT_VECTOR: u8 = 8;
const GENERAL_PROTECTION_VECTOR: u8 = 13;
const PAGE_FAULT_VECTOR: u8 = 14;

/// Routes every vector of the [`Idt`] towards clousure-like handlers.
///
/// The left part of an arm can take two values in the form `<vector>, <ist>` to swap to a
/// separate stack for the interrupt. The right part of an arm can take a `code` argument
/// containing the error code.
macro_rules! gates {
    ($idt:expr; $($vector:expr $(, $ist:expr)? => |$($arg:ident),+| $body:expr),* $(,)?) => {
        $($idt.set_gate($vector, gates!(@handler |$($arg),+| $body), gates!(@ist $($ist)?));)*
    };
    (@handler |$frame:ident| $body:expr) => {{
        extern "x86-interrupt" fn handler($frame: InterruptStackFrame) { $body }
        handler as *const ()
    }};
    (@handler |$frame:ident, $code:ident| $body:expr) => {{
        extern "x86-interrupt" fn handler($frame: InterruptStackFrame, $code: u64) { $body }
        handler as *const ()
    }};
    (@ist) => { 0 };
    (@ist $ist:expr) => { $ist };
}

impl Idt {
    /// Registers handlers which turn CPU exceptions into panics, instead of escalating into a
    /// triple fault which silently resets the machine.
    fn register_exceptions(&self) {
        gates! { self;
            // All general faults without a specific handler of its own
            GENERAL_PROTECTION_VECTOR => |frame, code| general_protection(frame, code),
            PAGE_FAULT_VECTOR => |frame, code| panic!(
                "CPU exception: Page Fault at {:#x} accessing {:#x} (code {code:#x})",
                frame.instruction_pointer,
                Cr2::read_raw(),
            ),

            // Double faults can be raised due to stack overflows, so we give it a stack of its own
            DOUBLE_FAULT_VECTOR, DOUBLE_FAULT_IST => |frame, _code| panic!(
                "CPU exception: Double Fault at {:#x}",
                frame.instruction_pointer,
            ),

            // Not a real exception, no-op
            ApicTimer::SPURIOUS_VECTOR as u8 => |_frame| {},
        }
    }
}

/// Panics for a general protection fault.
///
/// This is raised by the CPU when it cannot locate a suitable IDT entry for a specific fault,
/// along with a `code` which can be used to identify the original exception by its vector.
fn general_protection(frame: InterruptStackFrame, code: u64) -> ! {
    const IDT_BIT: u64 = 1 << 1;

    let rip = frame.instruction_pointer;
    if code & IDT_BIT == 0 {
        // Regular general protection fault
        panic!("CPU exception: General Protection at {rip:#x} (code {code:#x})");
    }

    // Raised due to missing handler for another fault
    let vector = (code >> 3) as u8; // u8 is safe, only 256 IDT entries
    match ExceptionVector::try_from(vector) {
        Ok(exception) => panic!("Unhandled {exception:?} (vector {vector:#x}) at {rip:#x}"),
        Err(_) => panic!("Unhandled interrupt (vector {vector:#x}) at {rip:#x}"),
    }
}

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
        // Check with interrupts disabled, so one can't fire between checking and halting
        interrupts::disable();

        // Halt execution until an interrupt is received
        while !self.completed.load(Ordering::SeqCst) {
            // NOTE:
            // `sti` waits until the next instruction to take effect, so the next instruction
            // must immediately be `hlt`. Previously, debug builds had instructions inserted
            // after the wrapper we had in asm.rs which is where it would take effect, before
            // the actual `hlt`.
            //
            // `nomem` is enabled, but interrupt handlers writing to memory while halted is fine,
            // as `completed` is atomic and `cli` after it doesn't enable `nomem`.
            interrupts::enable_and_hlt();
            interrupts::disable();
        }
    }

    /// Returns whether the interrupt has completed.
    pub fn is_completed(&self) -> bool {
        self.completed.load(Ordering::SeqCst)
    }

    /// Signal that the interrupt has completed. Called from interrupt handler.
    #[allow(unused)]
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
#[rustfmt::skip]
pub unsafe fn init_pic() {
    let mut master_command = Port::<u8>::new(0x20);
    let mut master_data = Port::<u8>::new(0x21);
    let mut slave_command = Port::<u8>::new(0xA0);
    let mut slave_data = Port::<u8>::new(0xA1);

    // ICW1: initialize
    master_command.write(0x11); // master: init + ICW4 expected
    slave_command.write(0x11);  // slave:  init + ICW4 expected

    // ICW2: vector offsets
    master_data.write(0x20);    // master: IRQ[0-7]  = vectors 20h-27h
    slave_data.write(0x28);     // slave:  IRQ[8-15] = vectors 28h-2Fh

    // ICW3: cascade
    master_data.write(0x04);    // master: slave on IRQ2
    slave_data.write(0x02);     // slave:  casade ID 2

    // ICW4: set both master and slave modes to 8086
    master_data.write(0x01);    // master
    slave_data.write(0x01);     // slave

    // Mask all IRQs
    master_data.write(0xFF);    // master
    slave_data.write(0xFF);     // slave
    master_command.write(0x60); // send EOI to all pending IRQs
}
