use linked_list_allocator::LockedHeap;
use uefi::boot::{exit_boot_services, MemoryDescriptor, MemoryType, PAGE_SIZE};
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};

use crate::serial::Serial;

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
    /// available memory region, and sets up serial console. Returns the
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
        let region = Self::find_memory_region(mmap).expect("No usable memory found in memory map");

        ALLOCATOR.lock().init(region.start as *mut u8, region.size);
        Serial::init();

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
