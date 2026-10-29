use alloc::alloc::{alloc, dealloc, Layout};

use core::ptr;

// Links the C `vsnprintf` for libc
use nostd_printf as _;

//
// Minimal libc Implementation
//

const HEADER_SIZE: usize = size_of::<usize>();
const MIN_ALIGNMENT: usize = 8;

/// `libc::malloc` implementation which tracks sizes.
///
/// Allocates `size` bytes of memory, returning a pointer to the allocated
/// memory.
///
/// Internally, it prepends a [`usize`] header representing the size of the
/// allocation, which is later used by other memory management APIs (such as
/// [`free`] and [`realloc`]).
///
/// ## Safety
/// Like other C memory management, the returned pointer must be manually freed,
/// the global allocator does not track it.
///
/// ## Returns
/// - An aligned ([`MIN_ALIGNMENT`]) pointer if successful
/// - A [`ptr::null_mut`] upon error, or if size is zero
#[no_mangle]
pub extern "C" fn malloc(size: usize) -> *mut u8 {
    if size == 0 {
        return ptr::null_mut();
    }

    let layout = Layout::from_size_align(HEADER_SIZE + size, MIN_ALIGNMENT).unwrap();
    let block = unsafe { alloc(layout) };

    if block.is_null() {
        log::warn!("malloc of {size} bytes failed, the heap is full");
        return ptr::null_mut();
    }

    unsafe {
        ptr::write(block as *mut usize, size);
        block.add(HEADER_SIZE)
    }
}

/// `libc::free` implementation which tracks sizes.
///
/// Deallocates memory held by `ptr`, previously allocated by [`malloc`]. If
/// `ptr` is a null pointer, this is a no-op.
///
/// Internally, it retrieves the pointer's size using the [`usize`] header
/// prepended during allocation.
///
/// ## Safety
/// - `ptr` must be a valid pointer that has previously been allocated
/// - The pointer must not be used after it has been freed
#[no_mangle]
pub extern "C" fn free(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }

    unsafe {
        let block = ptr.sub(HEADER_SIZE);
        let size = ptr::read(block as *const usize);
        let layout = Layout::from_size_align(HEADER_SIZE + size, MIN_ALIGNMENT).unwrap();

        dealloc(block, layout);
    }
}

/// `libc::realloc` implementation which tracks sizes.
///
/// Resizes a previously allocated `ptr` to `new_size` bytes. This will
/// [`free`] the old pointer, [`malloc`] a new one, and `memcpy` the data
/// from the old pointer to the new one.
///
/// - If `ptr` is null, this is equivalent to `malloc(new_size)`
/// - If `new_size` is zero, this is equivalent to `free(ptr)`
///
/// ## Safety
/// - `ptr` must be a valid pointer that has previously been allocated
/// - The old pointer will no longer be valid after this function has been
///   called
///
/// ## Returns
/// - A pointer to a new region of the memory of the new size
/// - [`ptr::null_mut`] if zero sized or allocation fails
#[no_mangle]
pub extern "C" fn realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if new_size == 0 {
        free(ptr);
        return ptr::null_mut();
    }

    if ptr.is_null() {
        return malloc(new_size);
    }

    unsafe {
        let block = ptr.sub(HEADER_SIZE);
        let old_size = ptr::read(block as *const usize);

        let new_ptr = malloc(new_size);
        if new_ptr.is_null() {
            return ptr::null_mut();
        }

        let copy_size = core::cmp::min(old_size, new_size);
        ptr::copy_nonoverlapping(ptr, new_ptr, copy_size);
        free(ptr);

        new_ptr
    }
}

/// `libc::calloc` implementation.
///
/// Allocates and initializes a region of `num` elements of `size` bytes each
/// with zero data.
///
/// ## Safety
/// Like other C memory management, the returned pointer must be manually freed,
/// the global allocator does not track it.
///
/// ## Returns
/// ## Returns
/// - An aligned ([`MIN_ALIGNMENT`]) pointer if successful
/// - A [`ptr::null_mut`] upon error, or if element size is zero or memory region
///   size overflows limit
#[no_mangle]
pub extern "C" fn calloc(num: usize, size: usize) -> *mut u8 {
    if let Some(total) = num.checked_mul(size) {
        let ptr = malloc(total);
        if !ptr.is_null() {
            unsafe { ptr::write_bytes(ptr, 0, total) };
        }

        ptr
    } else {
        // Overflow
        ptr::null_mut()
    }
}

/// Type of the comparison function which takes pointers to two elements
/// and returns:
///
/// - negative if first < second
/// - zero if equal
/// - positive if first > second
pub type ComparFn = extern "C" fn(*const u8, *const u8) -> i32;

/// Quick sort implementation, primarily used by `libvpx` for sorting tile
/// buffers.
///
/// ## Safety
/// - `base` must be a valid pointer to an array
/// - The array must be exactly `nmemb * size` bytes
/// - Null pointers for `base` or zero values for `nmemb` and `size` is a
///   no-op
///
/// ## Parameters
/// - `base`: pointer to the first element of the array
/// - `nmemb`: number of elements in the array
/// - `size`: size of each element in the array in bytes
/// - `compar`: Comparison callback function pointer
#[no_mangle]
pub unsafe extern "C" fn qsort(base: *mut u8, nmemb: usize, size: usize, compar: ComparFn) {
    if base.is_null() || nmemb == 0 || size == 0 {
        return;
    }

    for i in 1..nmemb {
        let mut j = i;
        while j > 0 {
            let a = base.add((j - 1) * size);
            let b = base.add(j * size);

            if compar(a, b) > 0 {
                ptr::swap_nonoverlapping(a, b, size);
                j -= 1;
            } else {
                break;
            }
        }
    }
}

//
// Stubs
//

#[no_mangle]
pub extern "C" fn getenv(_name: *const u8) -> *mut u8 {
    ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn strtol(_str: *const u8, _endptr: *mut *mut u8, _base: i32) -> i32 {
    0
}

#[no_mangle]
pub unsafe extern "C" fn strtoul(_str: *const u8, _endptr: *mut *mut u8, _base: i32) -> u32 {
    0
}

#[no_mangle]
#[cfg(target_arch = "x86_64")]
#[rustfmt::skip]
pub extern "C" fn __chkstk() {/* no-op msvc stub */}
