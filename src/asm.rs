use core::arch::asm;

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

/// Arm the memory barrier ensuring load and store operations are ordered.
#[inline]
pub unsafe fn mfence() {
    asm!("mfence", options(nomem, nostack, preserves_flags));
}

//
// Interrupt Descriptor Table (IDT)
//

const IDT_ENTRIES: usize = 256;

/// The global Interrupt Descriptor Table.
pub static mut IDT: [IdtEntry; IDT_ENTRIES] = [IdtEntry {
    offset_low: 0,
    selector: 0,
    ist: 0,
    attributes: 0,
    offset_mid: 0,
    offset_high: 0,
    reserved: 0,
}; IDT_ENTRIES];

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

//
// Interrupt Request (IRQ)
//

static mut IRQ_HANDLER: Option<extern "C" fn()> = None;
static mut IRQ_COMPLETED: bool = false;

/// Load the IDT into the CPU. Must be called before any interrupts fire.
pub fn setup_idt() {
    #[repr(C, packed)]
    struct Idtr {
        limit: u16,
        base: u64,
    }

    unsafe {
        let idtr = Idtr {
            limit: (core::mem::size_of::<IdtEntry>() * IDT_ENTRIES - 1) as u16,
            base: &raw const IDT as *const _ as u64,
        };

        asm!("lidt [{}]", in(reg) &idtr, options(nostack, preserves_flags));
    }
}

/// Register an IRQ handler at the given interrupt vector.
///
/// The handler will be called with all registers saved. The EOI will be
/// sent if the `with_eoi` trampoline variant was selected.
#[rustfmt::skip]
pub fn register_irq_handler(vector: u8, handler: extern "C" fn()) {
    unsafe {
        IRQ_HANDLER = Some(handler);

        let handler_addr = handler as usize;
        let entry = &mut IDT[vector as usize];

        entry.offset_low  = (handler_addr & 0xFFFF) as u16;          // address bits 0–15
        entry.selector    = 0x08;                                    // kernel 64-bit code segment
        entry.ist         = 0;                                       // do not switch stacks
        entry.attributes  = 0x8E;                                    // present | ring 0 | 64-bit interrupt gate
        entry.offset_mid  = ((handler_addr >> 16) & 0xFFFF) as u16;  // address bits 16–31
        entry.offset_high = (handler_addr >> 32) as u32;             // address bits 32–63
        entry.reserved    = 0;                                       // must be zero
    }
}

/// Returns whether the last IRQ completed. Used by interrupt-based delay.
pub fn irq_completed() -> bool {
    unsafe { IRQ_COMPLETED }
}

/// Resets the IRQ completion flag.
pub fn reset_irq_completed() {
    unsafe { IRQ_COMPLETED = false }
}

macro_rules! save_regs {
    () => {
        "push rax; push rcx; push rdx; push rbx; push rbp; push rsi; push rdi;
         push r8; push r9; push r10; push r11; push r12; push r13; push r14; push r15"
    };
}

macro_rules! restore_regs {
    () => {
        "pop r15; pop r14; pop r13; pop r12; pop r11; pop r10; pop r9; pop r8;
         pop rdi; pop rsi; pop rbp; pop rbx; pop rdx; pop rcx; pop rax"
    };
}

/// Generates an IRQ trampoline that saves all GPRs, calls the handler,
/// optionally sends EOI to the APIC, then restores GPRs and returns.
macro_rules! define_irq_trampoline {
    ($name:ident, $handler:path, with_eoi) => {
        #[unsafe(naked)]
        pub extern "C" fn $name() {
            core::arch::naked_asm!(
                save_regs!(),
                "sub rsp, 8",
                "call {handler}",
                "add rsp, 8",

                // Trigger EOI
                "mov rax, 0xFEE000B0",
                "mov dword ptr [rax], 0",
                "mfence",

                restore_regs!(),
                "iretq",
                handler = sym $handler,
            );
        }
    };

    ($name:ident, $handler:path) => {
        #[unsafe(naked)]
        pub extern "C" fn $name() {
            core::arch::naked_asm!(
                save_regs!(),
                "sub rsp, 8",
                "call {handler}",
                "add rsp, 8",
                restore_regs!(),
                "iretq",
                handler = sym $handler,
            );
        }
    };
}

//
// IRQ handlers
//

pub(crate) extern "C" fn pit_irq_handler() {
    unsafe {
        IRQ_COMPLETED = true;
        outb(0x20, 0x20); // send EOI to PIC
    }
}

define_irq_trampoline!(pit_irq_trampoline, pit_irq_handler);

pub(crate) extern "C" fn timer_irq_handler() {
    unsafe {
        IRQ_COMPLETED = true;
    }
}

define_irq_trampoline!(timer_irq_trampoline, timer_irq_handler, with_eoi);

extern "C" fn generic_irq_handler() {
    unsafe {
        if let Some(handler) = IRQ_HANDLER {
            handler();
        }
    }
}

define_irq_trampoline!(irq_trampoline, generic_irq_handler, with_eoi);
