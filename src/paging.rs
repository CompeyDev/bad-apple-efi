use core::arch::asm;
use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

use x86_64::registers::control::Cr3;
use x86_64::registers::model_specific::Msr;
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{PageTable, PageTableFlags as Flags, PhysFrame};
use x86_64::{PhysAddr, VirtAddr};

use self::raw::PageTableEntryExt;

pub const SLOT_SIZE: u64 = 1 << 21; // 2 MiB
pub const SUPERPAGE_SIZE: u64 = 1 << 30; // 1 GiB

/// The PAT bit of a 1 GiB or 2 MiB page, a part of its address bits.
const LARGE_PAT: u64 = 1 << 12;
/// The PAT bit of a 4 KiB page.
const SMALL_PAT: Flags = Flags::HUGE_PAGE;

/// The cache control bits in an entry's flags. Combined with the PAT bit, they index the
/// PAT (equivalent to `PCD | PWT`).
const CACHE_BITS: Flags = Flags::NO_CACHE.union(Flags::WRITE_THROUGH);

/// A memory type, picked through one of the PAT entries.
///
/// The first four assume the PAT still has its default layout for those entries.
#[allow(unused)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryType {
    /// PAT entry 0, normal cached memory.
    WriteBack,
    /// PAT entry 1, reads are cached but writes go straight to memory.
    WriteThrough,
    /// PAT entry 3, the CPU only ever reads and writes the memory directly.
    Uncacheable,
    /// PAT entry 5, writes are buffered and sent to memory in bursts. Reads are slow.
    WriteCombining,
}

impl MemoryType {
    /// The `PCD | PWT` bits that pick this type's PAT entry, and whether its PAT bit is set.
    fn bits(self) -> (Flags, bool) {
        match self {
            Self::WriteBack => (Flags::empty(), false),
            Self::WriteThrough => (Flags::WRITE_THROUGH, false),
            Self::Uncacheable => (CACHE_BITS, false),
            Self::WriteCombining => (Flags::WRITE_THROUGH, true),
        }
    }
}

/// An address that wasn't mapped, so its memory type couldn't be changed.
#[derive(Debug, Clone, Copy)]
pub struct Unmapped(pub u64);

impl fmt::Display for Unmapped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "address {:#x} is not mapped", self.0)
    }
}

/// The mapping of a 2 MiB aligned slot of the address space.
enum Mapping<'a> {
    /// A single 2 MiB page.
    Huge(&'a mut PageTableEntry),
    /// A table of 4 KiB pages.
    Small(&'a mut PageTable),
}

impl Mapping<'_> {
    /// Changes the memory type of every page in the slot.
    fn set_memory_type(self, ty: MemoryType) {
        match self {
            Self::Huge(entry) => {
                // For large pages, the PAT bit is a part of the address bits
                let (cache, pat) = ty.bits();
                let addr = (entry.addr().as_u64() & !LARGE_PAT) | if pat { LARGE_PAT } else { 0 };
                let flags = (entry.flags() - CACHE_BITS) | cache;
                entry.set_addr(PhysAddr::new(addr), flags);
            }
            Self::Small(table) => {
                let (cache, pat) = ty.bits();
                let pat = if pat { SMALL_PAT } else { Flags::empty() };
                for entry in table.iter_mut() {
                    entry.set_flags((entry.flags() - CACHE_BITS - SMALL_PAT) | cache | pat);
                }
            }
        }
    }
}

/// Our own copy of the firmware's identity-mapped page tables, so memory types can be changed.
///
/// Typically, this is constructed once with [`PageTables::take_over`] during early boot, then
/// used to change the memory types of ranges by using [`PageTables::set_memory_type`].
pub struct PageTables {
    pml4: &'static mut PageTable, // invariant: tables are allocated by us, and not the firmware
}

impl PageTables {
    /// Copies the firmware's page tables to the heap and switches to them.
    ///
    /// The original tables allocated by the firmware are write protected as a part of the boot
    /// services memory. We cannot change them directly, so we would have to build a tables, which
    /// is more complicated than just copying them to the heap.
    ///
    /// ## Errors
    /// - Panics if called more than once
    ///
    /// ## Safety
    /// - Heap must be identity mapped, and must not overlap the firmware's tables
    /// - Must be called before anything changes memory types
    pub unsafe fn take_over() -> Self {
        static TAKEN: AtomicBool = AtomicBool::new(false);
        assert!(!TAKEN.swap(true, Ordering::SeqCst), "page tables were already taken over");

        let (frame, flags) = Cr3::read();
        let pml4 = raw::copy(unsafe { raw::phys_to_table(frame.start_address()) }, 4);
        unsafe { Cr3::write(PhysFrame::containing_address(raw::phys_of(pml4)), flags) };

        log::debug!("Switched to our own page tables at {pml4:p}");
        Self { pml4 }
    }

    /// Changes the memory type of a 2 MiB aligned range, then flushes the TLB and cache.
    ///
    /// ## Errors
    /// - Returns [`Unmapped`] if part of the range isn't mapped
    /// - Panics if the range isn't 2 MiB aligned
    ///
    /// ## Safety
    /// The range must not hold anything relying on being cacheable, like page tables.
    pub unsafe fn set_memory_type(
        &mut self,
        start: u64,
        size: u64,
        ty: MemoryType,
    ) -> Result<(), Unmapped> {
        assert!(
            start.is_multiple_of(SLOT_SIZE) && size.is_multiple_of(SLOT_SIZE),
            "range must be 2 MiB aligned"
        );

        if ty == MemoryType::WriteCombining {
            const IA32_PAT_MSR: u32 = 0x277;
            const WRITE_COMBINING_ENTRY: u32 = 5;
            const PAT_WRITE_COMBINING: u64 = 0x01;

            let shift = WRITE_COMBINING_ENTRY * 8;
            let mut msr = Msr::new(IA32_PAT_MSR);

            // SAFETY: The PAT is read and written as a whole, only changing the entry we own
            unsafe {
                let old = msr.read();
                let new = (old & !(0xFF << shift)) | (PAT_WRITE_COMBINING << shift);
                if new != old {
                    msr.write(new);
                    log::debug!("Reprogrammed the PAT: {old:#018x} to {new:#018x}");
                }
            }
        }

        let mut result = Ok(());
        for addr in (start..start + size).step_by(SLOT_SIZE as usize) {
            match self.mapping(addr) {
                Ok(mapping) => mapping.set_memory_type(ty),
                Err(err) => {
                    result = Err(err);
                    break;
                }
            }
        }

        self.flush(); // flush even on an error, since some slots may have changed anyway
        result
    }

    /// Maps a 2 MiB aligned range as uncacheable, i.e., the CPU only ever reads and writes it in
    /// memory.
    ///
    /// ## Errors
    /// Same as [`Self::set_memory_type`].
    ///
    /// ## Safety
    /// Same as [`Self::set_memory_type`].
    pub unsafe fn set_uncacheable(&mut self, start: u64, size: u64) -> Result<(), Unmapped> {
        unsafe { self.set_memory_type(start, size, MemoryType::Uncacheable) }
    }

    /// Walks to the 2 MiB slot containing `addr`, splitting a 1 GiB page if necessary.
    fn mapping(&mut self, addr: u64) -> Result<Mapping<'_>, Unmapped> {
        let unmapped = Unmapped(addr);
        let virt = VirtAddr::try_new(addr).map_err(|_| unmapped)?;

        let pdpt = self.pml4[virt.p4_index()].child().ok_or(unmapped)?;
        let entry = &mut pdpt[virt.p3_index()];
        if !entry.flags().contains(Flags::PRESENT) {
            return Err(unmapped);
        }

        // 1 GiB pages are split to change only one slot
        if entry.flags().contains(Flags::HUGE_PAGE) {
            entry.split();
        }

        let pd = entry.child().ok_or(unmapped)?;
        let entry = &mut pd[virt.p2_index()];

        let flags = entry.flags();
        if !flags.contains(Flags::PRESENT) {
            Err(unmapped)
        } else if flags.contains(Flags::HUGE_PAGE) {
            Ok(Mapping::Huge(entry))
        } else {
            entry.child().map(Mapping::Small).ok_or(unmapped)
        }
    }

    /// Makes the CPU see the new tables and writes back anything cached before a type change.
    fn flush(&mut self) {
        // HACK: reloading CR3 flushes the TLB
        let (frame, flags) = Cr3::read();
        unsafe {
            Cr3::write(frame, flags);
            asm!("wbinvd", options(nostack, preserves_flags));
        }
    }
}

/// Helpers that access page tables by their physical address, which relies on the identity
/// mapping.
mod raw {
    use alloc::boxed::Box;
    use core::ptr;

    use x86_64::structures::paging::page_table::PageTableEntry;
    use x86_64::structures::paging::{PageTable, PageTableFlags as Flags};
    use x86_64::PhysAddr;

    use super::{LARGE_PAT, SLOT_SIZE, SUPERPAGE_SIZE};

    /// Deep copies a table.
    pub(super) fn copy(src: &PageTable, level: u8) -> &'static mut PageTable {
        let dst = Box::leak(Box::new(src.clone()));
        if level > 1 {
            for entry in dst.iter_mut() {
                // Fails for non existent entries or large pages
                if let Ok(frame) = entry.frame() {
                    // spooky recursion 👻
                    let child = copy(unsafe { phys_to_table(frame.start_address()) }, level - 1);
                    entry.set_addr(phys_of(child), entry.flags());
                }
            }
        }

        dst
    }

    /// Reads a table at a physical address.
    ///
    /// ## Safety
    /// - `addr` must point to a valid page table
    /// - The memory must be identity-mapped
    /// - The memory must not be written to while the reference is in scope
    pub(super) unsafe fn phys_to_table<'a>(addr: PhysAddr) -> &'a PageTable {
        unsafe { &*(addr.as_u64() as *const PageTable) }
    }

    /// Returns the physical address of a table.
    pub(super) fn phys_of(table: &PageTable) -> PhysAddr {
        PhysAddr::new(ptr::from_ref(table) as u64)
    }

    pub(super) trait PageTableEntryExt {
        /// Splits a 1 GiB page into 2 MiB pages of the same memory type.
        fn split(&mut self);

        /// The table an entry points to, or `None` if it's not present or maps a large page.
        fn child(&mut self) -> Option<&mut PageTable>;
    }

    impl PageTableEntryExt for PageTableEntry {
        fn split(&mut self) {
            let (flags, raw) = (self.flags(), self.addr().as_u64());
            let (base, pat) = (raw & !(SUPERPAGE_SIZE - 1), raw & LARGE_PAT);

            let pd = Box::leak(Box::new(PageTable::new()));
            for (i, page) in pd.iter_mut().enumerate() {
                page.set_addr(PhysAddr::new((base + i as u64 * SLOT_SIZE) | pat), flags);
            }

            self.set_addr(phys_of(pd), flags - Flags::HUGE_PAGE);
        }

        fn child(&mut self) -> Option<&mut PageTable> {
            let frame = self.frame().ok()?;
            Some(unsafe { &mut *(frame.start_address().as_u64() as *mut PageTable) })
        }
    }
}
