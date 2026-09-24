//! Allocation cap of the helper process.
//!
//! A hostile file can declare huge dimensions: the decoder then asks for tens of gigabytes
//! (fuzzing found requests from 4 to 23 GB), which would push the whole machine into swap. Any
//! single allocation above the cap is refused, which stops the helper at once; the application
//! reports the file as unreadable and restarts the helper.

use std::alloc::{GlobalAlloc, Layout, System};

/// Largest single allocation. Real files stay far below: a 177 Mpx sensor in 16 bits is 354 MB.
pub const MAX_ALLOCATION: usize = 2 << 30;

pub struct Capped;

// SAFETY: delegates to the system allocator; refusing a request (null) is always allowed.
unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() > MAX_ALLOCATION {
            return std::ptr::null_mut();
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if layout.size() > MAX_ALLOCATION {
            return std::ptr::null_mut();
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was returned by this allocator, i.e. by `System`, with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > MAX_ALLOCATION {
            return std::ptr::null_mut();
        }
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}
