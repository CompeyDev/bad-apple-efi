use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::size_of;

use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

//
// Segment Selectors
//

/// Kernel 64-bit code segment selector (GDT entry 1).
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;

/// Kernel 64-bit data segment selector (GDT entry 2).
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;

/// Task state segment selector (GDT entries 5 and 6).
pub const TSS_SELECTOR: u16 = 0x28;

//
// Global Descriptor Table (GDT)
//

pub static GDT: Gdt = Gdt::new();

const GDT_ENTRIES: usize = 7;

pub const DOUBLE_FAULT_IST: u8 = 1;
const DOUBLE_FAULT_STACK_SIZE: usize = 16 * 1024;

#[repr(align(16))]
#[allow(dead_code)] // only its address is used
struct Stack([u8; DOUBLE_FAULT_STACK_SIZE]);

/// Represents an x86_64 GDT entry (8 bytes total).
///
/// ## Layout
/// - `limit_low`         - bits 0–15
/// - `base_low`          - bits 16–31
/// - `base_mid`          - bits 32–39
/// - `access`            - bits 40–47
/// - `flags_limit_high`  - bits 48–51 (flags), 52–55 (limit high)
/// - `base_high`         - bits 56–63
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GdtEntry {
    /// Low 16 bits of the segment limit.
    pub limit_low: u16,
    /// Low 16 bits of the segment base address.
    pub base_low: u16,
    /// Middle 8 bits of the segment base address.
    pub base_mid: u8,
    /// Access byte (present, DPL, type flags).
    pub access: u8,
    /// Flags (bits 4–7) and limit high (bits 0–3).
    pub flags_limit_high: u8,
    /// High 8 bits of the segment base address.
    pub base_high: u8,
}

impl GdtEntry {
    pub const ZERO: GdtEntry = GdtEntry {
        limit_low: 0,
        base_low: 0,
        base_mid: 0,
        access: 0,
        flags_limit_high: 0,
        base_high: 0,
    };

    pub const fn kernel_code() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0x9A,
            flags_limit_high: 0xAF,
            base_high: 0,
        }
    }

    pub const fn kernel_data() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0x92,
            flags_limit_high: 0xCF,
            base_high: 0,
        }
    }

    pub const fn user_code() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0xFA,
            flags_limit_high: 0xAF,
            base_high: 0,
        }
    }

    pub const fn user_data() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0xF2,
            flags_limit_high: 0xCF,
            base_high: 0,
        }
    }

    /// A 64-bit TSS descriptor, which takes up two entries since it holds a full address.
    pub fn tss(tss: *const TaskStateSegment) -> [Self; 2] {
        let base = tss as u64;
        let low = Self {
            limit_low: (size_of::<TaskStateSegment>() - 1) as u16,
            base_low: base as u16,
            base_mid: (base >> 16) as u8,
            access: 0x89, // present, ring 0, available 64-bit TSS
            flags_limit_high: 0,
            base_high: (base >> 24) as u8,
        };

        // The upper 32 address bits go in the first 4 bytes of the second entry
        let high =
            Self { limit_low: (base >> 32) as u16, base_low: (base >> 48) as u16, ..Self::ZERO };
        [low, high]
    }
}

/// The Global Descriptor Table.
pub struct Gdt {
    entries: UnsafeCell<[GdtEntry; GDT_ENTRIES]>,
    tss: UnsafeCell<TaskStateSegment>,
    double_fault_stack: UnsafeCell<Stack>,
}

impl Gdt {
    pub const fn new() -> Self {
        Self {
            entries: UnsafeCell::new([GdtEntry::ZERO; GDT_ENTRIES]),
            tss: UnsafeCell::new(TaskStateSegment::new()),
            double_fault_stack: UnsafeCell::new(Stack([0; DOUBLE_FAULT_STACK_SIZE])),
        }
    }

    pub unsafe fn load(&self) {
        // NOTE:
        // Don't deref here, we want a pointer to the *static* storage, we deref
        // every single index instead to get stack copies. If we deref'd here, a
        // stack copy would be handed to the gdtr which would be invalid
        let entries_ptr = self.entries.get();
        (*entries_ptr)[0] = GdtEntry::ZERO;
        (*entries_ptr)[1] = GdtEntry::kernel_code();
        (*entries_ptr)[2] = GdtEntry::kernel_data();
        (*entries_ptr)[3] = GdtEntry::user_code();
        (*entries_ptr)[4] = GdtEntry::user_data();

        // The stack grows downwards, we point the TSS to the end of it
        let tss = self.tss.get();
        let stack_end = self.double_fault_stack.get().add(1);
        (*tss).interrupt_stack_table[usize::from(DOUBLE_FAULT_IST - 1)] =
            VirtAddr::from_ptr(stack_end);
        [(*entries_ptr)[5], (*entries_ptr)[6]] = GdtEntry::tss(tss);

        #[repr(C, packed)]
        struct Gdtr {
            limit: u16,
            base: u64,
        }

        let gdtr = Gdtr {
            limit: (core::mem::size_of::<GdtEntry>() * GDT_ENTRIES - 1) as u16,
            base: entries_ptr as u64,
        };

        asm!("lgdt [{}]", in(reg) &gdtr, options(readonly, nostack, preserves_flags));

        // Reload segment registers
        asm!(
            "mov ax, {sel}",
            "mov ds, ax",
            "mov es, ax",
            "mov fs, ax",
            "mov gs, ax",
            "mov ss, ax",
            sel = const KERNEL_DATA_SELECTOR,
            out("ax") _,
            options(nostack, preserves_flags)
        );

        asm!(
            "push {cs}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq", // far return trick
            "2:",
            cs = const KERNEL_CODE_SELECTOR,
            tmp = out(reg) _,
            options(preserves_flags),
        );

        asm!("ltr {:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
    }
}

// SAFETY: GDT must only be modified in a single-threaded context during boot
unsafe impl Sync for Gdt {}
