use core::arch::asm;
use core::cell::UnsafeCell;

//
// Segment Selectors
//

/// Kernel 64-bit code segment selector (GDT entry 1).
pub const KERNEL_CODE_SELECTOR: u16 = 0x08;

/// Kernel 64-bit data segment selector (GDT entry 2).
pub const KERNEL_DATA_SELECTOR: u16 = 0x10;

//
// Global Descriptor Table (GDT)
//

pub static GDT: Gdt = Gdt::new();

const GDT_ENTRIES: usize = 5;

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
}

/// The Global Descriptor Table.
pub struct Gdt {
    entries: UnsafeCell<[GdtEntry; GDT_ENTRIES]>,
}

impl Gdt {
    pub const fn new() -> Self {
        Self { entries: UnsafeCell::new([GdtEntry::ZERO; GDT_ENTRIES]) }
    }

    pub unsafe fn load(&self) {
        let mut entries = *self.entries.get();
        entries[0] = GdtEntry::ZERO;
        entries[1] = GdtEntry::kernel_code();
        entries[2] = GdtEntry::kernel_data();
        entries[3] = GdtEntry::user_code();
        entries[4] = GdtEntry::user_data();

        #[repr(C, packed)]
        struct Gdtr {
            limit: u16,
            base: u64,
        }

        let gdtr = Gdtr {
            limit: (core::mem::size_of::<GdtEntry>() * GDT_ENTRIES - 1) as u16,
            base: entries.as_ptr() as u64,
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
    }
}

// SAFETY: GDT must only be modified in a single-threaded context during boot
unsafe impl Sync for Gdt {}
