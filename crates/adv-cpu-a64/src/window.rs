//! The guest window (workbook §6.1): 4 GiB at a 4 GiB-aligned host base,
//! with an inaccessible guard on each side. The low 32 bits of a biased
//! stack register are then the guest value itself, and a push or pop that
//! wraps past 0 or 4 GiB lands in a guard instead of host memory.

use std::ptr::NonNull;

pub const GUARD: usize = 128 << 10;
const SPAN: usize = 1 << 32;

pub struct Window {
    /// Start of the lower guard.
    start: NonNull<u8>,
}

impl Window {
    /// Reserves the window without committing anything; every byte starts
    /// inaccessible.
    pub fn new() -> Result<Self, String> {
        let len = SPAN + 2 * GUARD;
        // Over-reserve by one span so an aligned window fits, then trim.
        let over = len + SPAN;
        // SAFETY: a fresh inaccessible reservation at an OS-chosen address.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                over,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED {
            return Err(format!(
                "reserve guest window: {}",
                std::io::Error::last_os_error()
            ));
        }
        let raw = raw as usize;
        let base = (raw + GUARD).next_multiple_of(SPAN);
        let start = base - GUARD;
        let end = start + len;
        // SAFETY: both trims lie inside the reservation made above.
        unsafe {
            if start > raw {
                libc::munmap(raw as *mut _, start - raw);
            }
            if raw + over > end {
                libc::munmap(end as *mut _, raw + over - end);
            }
        }
        Ok(Self {
            start: NonNull::new(start as *mut u8).ok_or("guest window at null")?,
        })
    }

    /// Host address of guest address 0: `x27`.
    pub fn base(&self) -> usize {
        self.start.as_ptr() as usize + GUARD
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window owns exactly this range.
        unsafe { libc::munmap(self.start.as_ptr().cast(), SPAN + 2 * GUARD) };
    }
}
