//! Process-wide Rust heap accounting for command-line diagnostics.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Wraps [`System`] to count live and peak allocated Rust heap bytes.
pub struct CountingAllocator;

fn add(bytes: usize) {
    let now = LIVE
        .fetch_add(bytes, Ordering::Relaxed)
        .saturating_add(bytes);
    PEAK.fetch_max(now, Ordering::Relaxed);
}

fn sub(bytes: usize) {
    LIVE.fetch_sub(bytes, Ordering::Relaxed);
}

/// Current live and peak Rust heap bytes when [`CountingAllocator`] is active.
#[cfg(host_view)]
pub(crate) fn usage() -> Option<(usize, usize)> {
    let peak = PEAK.load(Ordering::Relaxed);
    (peak > 0).then(|| (LIVE.load(Ordering::Relaxed), peak))
}

// SAFETY: Every allocation delegates directly to `System` with identical
// layout, alignment, and pointer invariants.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Forwarded from the `GlobalAlloc::alloc` contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Forwarded from the `GlobalAlloc::alloc_zeroed` contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        sub(layout.size());
        // SAFETY: Forwarded from the `GlobalAlloc::dealloc` contract.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: Forwarded from the `GlobalAlloc::realloc` contract.
        let result = unsafe { System.realloc(ptr, layout, new_size) };
        if !result.is_null() {
            match new_size.cmp(&layout.size()) {
                std::cmp::Ordering::Greater => add(new_size - layout.size()),
                std::cmp::Ordering::Less => sub(layout.size() - new_size),
                std::cmp::Ordering::Equal => {}
            }
        }
        result
    }
}
