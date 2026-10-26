use linked_list_allocator::LockedHeap;
use uefi::boot::{exit_boot_services, MemoryDescriptor, MemoryType, PAGE_SIZE};
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};

use crate::interrupts::IDT;
use crate::segments::GDT;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Collection of functions to transition between a boot memory state and managed
/// memory state.
///
/// Make use of [`UefiAllocatorManager::init`] during the early boot
/// state, before most runtime logic in order to set up the allocator correctly.
pub struct UefiAllocatorManager;

impl UefiAllocatorManager {
    /// Exit boot services, initializes the global allocator with the largest
    /// available memory region, and loads the GDT and IDT. Returns the
    /// [`MemoryRegion`] inhabited by the allocator.
    ///
    /// ## Errors
    /// - Panics if there are no usable regions for the allocator
    ///
    /// ## Safety
    /// - Must only be called once
    /// - After this call, UEFI boot services are no longer available
    /// - The global allocator will be initialized and ready for use
    pub unsafe fn init() -> MemoryRegion<MemoryMapOwned> {
        let mmap = exit_boot_services(None);
        log::debug!("Exited boot services");
        let region = Self::find_memory_region(mmap).expect("No usable memory found in memory map");

        // Heap setup
        ALLOCATOR.lock().init(region.start as *mut u8, region.size);
        log::info!("Heap: {} MiB", region.size >> 20);
        log::debug!("Heap at {:#x}..{:#x} ({:?})", region.start, region.end(), region.memory_type);

        // DMA pool setup
        let dma_pool_size = 1 << 21; // 2 MiB
        let dma_pool = dma::init(dma_pool_size);
        log::debug!("DMA pool at {dma_pool:p}, {} KiB uncacheable", dma_pool_size >> 10);

        // Interrupts
        GDT.load();
        IDT.load();
        log::debug!("Loaded the GDT and IDT");

        region
    }

    /// Find the largest usable chunk of memory from the memory map.
    pub fn find_memory_region(memory_map: MemoryMapOwned) -> Option<MemoryRegion<MemoryMapOwned>> {
        let mut best_region = MemoryRegion::default();
        for desc in memory_map.entries() {
            let region = MemoryRegion::from(*desc);
            if region.is_usable() && region.page_count() > best_region.page_count() {
                best_region = region
            }
        }

        best_region.memory_map = Some(memory_map);
        if !best_region.is_usable() {
            None
        } else {
            Some(best_region)
        }
    }
}

pub mod dma {
    use core::alloc::{GlobalAlloc, Layout};

    use linked_list_allocator::LockedHeap;

    use crate::paging::PageTables;

    /// Memory for DMA buffers, mapped as uncacheable.
    static DMA_POOL: LockedHeap = LockedHeap::empty();

    /// Initializes a DMA buffer of `size` bytes, and returns a pointer to it.
    ///
    /// It is recommended to use [`memory::dma::alloc`] and [`memory::dma::dealloc`] instead
    /// of the raw pointer.
    pub unsafe fn init(size: usize) -> *mut u8 {
        let mut tables = PageTables::take_over();
        let layout = Layout::from_size_align(size, size).unwrap();

        let pool = super::ALLOCATOR.alloc(layout);
        assert!(!pool.is_null(), "No memory left for the DMA pool");

        tables.set_uncacheable(pool as u64, size as u64).expect("couldn't make DMA uncacheable");
        DMA_POOL.lock().init(pool, size);
        pool
    }

    /// Allocates zeroed memory from the uncacheable DMA pool, returning null on failure.
    pub fn alloc(layout: Layout) -> *mut u8 {
        unsafe { DMA_POOL.alloc_zeroed(layout) }
    }

    /// Frees memory from [`dma_alloc`].
    ///
    /// ## Safety
    /// `ptr` must come from [`dma_alloc`] with the same `layout`.
    pub unsafe fn dealloc(ptr: *mut u8, layout: Layout) {
        unsafe { DMA_POOL.dealloc(ptr, layout) }
    }
}

/// A reference to a contiguous block of memory on an underlying [`MemoryMap`]. Not
/// all regions are usable. Call `is_usable` to check whether a region of memory is
/// usable first.
///
/// Generally, this is constructed from a [`MemoryDescriptor`], by using the `From`
/// implementation.
#[derive(Debug)]
pub struct MemoryRegion<M: MemoryMap> {
    pub start: usize,
    pub size: usize,
    pub memory_type: MemoryType,
    pub memory_map: Option<M>,
}

impl<M: MemoryMap> MemoryRegion<M> {
    #[allow(unused)]
    pub fn end(&self) -> usize {
        self.start + self.size
    }

    pub fn page_count(&self) -> usize {
        self.size / PAGE_SIZE
    }

    pub fn is_usable(&self) -> bool {
        matches!(
            self.memory_type,
            MemoryType::CONVENTIONAL
                | MemoryType::BOOT_SERVICES_DATA
                | MemoryType::BOOT_SERVICES_CODE
        )
    }
}

impl<M: MemoryMap> Default for MemoryRegion<M> {
    fn default() -> Self {
        Self { start: 0, size: 0, memory_type: MemoryType::UNUSABLE, memory_map: None }
    }
}

impl<M: MemoryMap> From<MemoryDescriptor> for MemoryRegion<M> {
    fn from(desc: MemoryDescriptor) -> Self {
        MemoryRegion {
            start: desc.phys_start as usize,
            size: desc.page_count as usize * PAGE_SIZE,
            memory_type: desc.ty,
            memory_map: None,
        }
    }
}
