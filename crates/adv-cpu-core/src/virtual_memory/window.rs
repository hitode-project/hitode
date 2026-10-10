//! A 4 GiB guest window at a 4 GiB-aligned host base, with an inaccessible
//! guard on each side (a64 workbook §6.1). Translated code addresses guest
//! memory as base + zero-extended guest address, so the low 32 bits of a
//! biased stack pointer are the guest value, and a push or pop that wraps
//! past 0 or 4 GiB lands in a guard instead of host memory.
//!
//! The window owns its whole address range. Direct reservations placed in it
//! replace part of the range and hand it back as inaccessible when dropped.

use std::ptr::NonNull;

pub const GUARD: usize = 128 << 10;
const SPAN: usize = 1 << 32;
const LEN: usize = SPAN + 2 * GUARD;

pub struct GuestWindow {
    /// Start of the lower guard.
    start: NonNull<u8>,
}

impl GuestWindow {
    /// Reserves the window without committing anything.
    pub fn new() -> Result<Self, String> {
        // Over-reserve by one span so an aligned window fits, then trim.
        let over = LEN + SPAN;
        // SAFETY: a fresh inaccessible reservation at an OS-chosen address.
        let raw = unsafe { reserve(std::ptr::null_mut(), over, 0) };
        if raw == libc::MAP_FAILED {
            return Err(format!(
                "reserve guest window: {}",
                std::io::Error::last_os_error()
            ));
        }
        let raw = raw as usize;
        let start = (raw + GUARD).next_multiple_of(SPAN) - GUARD;
        let end = start + LEN;
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

    /// Host address of guest address 0.
    pub fn base(&self) -> usize {
        self.start.as_ptr() as usize + GUARD
    }

    /// Host address of `[offset, offset + len)` past the base, checked to lie
    /// inside the 4 GiB span.
    pub(super) fn place(&self, offset: usize, len: usize) -> Result<NonNull<u8>, String> {
        if offset.checked_add(len).is_none_or(|end| end > SPAN) {
            return Err("placement outside the guest window".into());
        }
        Ok(NonNull::new((self.base() + offset) as *mut u8).unwrap())
    }
}

impl Drop for GuestWindow {
    fn drop(&mut self) {
        // SAFETY: the window owns exactly this range; placed reservations
        // have already been handed back.
        unsafe { libc::munmap(self.start.as_ptr().cast(), LEN) };
    }
}

unsafe fn reserve(address: *mut u8, len: usize, fixed: libc::c_int) -> *mut libc::c_void {
    // SAFETY: forwarded; MAP_FIXED callers own the range they replace.
    unsafe {
        libc::mmap(
            address.cast(),
            len,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_NORESERVE | fixed,
            -1,
            0,
        )
    }
}

/// Replaces `[address, address + len)` with inaccessible reservation.
///
/// # Safety
/// The range must belong to a live `GuestWindow` and nothing may use it.
pub(super) unsafe fn vacate(address: *mut u8, len: usize) {
    // SAFETY: forwarded.
    let replaced = unsafe { reserve(address, len, libc::MAP_FIXED) };
    // Failing would leave a hole another mapping could take.
    assert!(
        replaced == address.cast(),
        "cannot return a placement to the guest window"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::virtual_memory::{GuestPageProtection, Reservation};

    #[test]
    fn the_window_is_aligned_and_placements_stay_inside_it() {
        let window = GuestWindow::new().unwrap();
        assert_eq!(window.base() % SPAN, 0);
        assert!(window.place(SPAN - 4096, 4096).is_ok());
        assert!(window.place(SPAN - 4096, 8192).is_err());
    }

    #[test]
    fn a_placed_reservation_shows_alias_writes_in_the_window() {
        let window = GuestWindow::new().unwrap();
        let page = crate::virtual_memory::os::page_size().unwrap();
        let reservation = Reservation::new_direct_in(&window, 4 * page, 2 * page).unwrap();
        assert_eq!(reservation.ptr.as_ptr() as usize, window.base() + 4 * page);
        reservation
            .commit_direct(0, 2 * page, GuestPageProtection::READ)
            .unwrap();
        let alias = reservation.alias().unwrap();
        // SAFETY: both views map the committed bytes.
        unsafe {
            alias.as_ptr().add(page + 8).write(0x5a);
            assert_eq!(*reservation.ptr.as_ptr().add(page + 8), 0x5a);
        }
        drop(reservation);
        // The range is inaccessible reservation again, still owned by the window.
        let again = Reservation::new_direct_in(&window, 4 * page, page).unwrap();
        drop(again);
    }
}
