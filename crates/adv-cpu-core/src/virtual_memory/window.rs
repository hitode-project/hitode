//! A 4 GiB guest window at a 4 GiB-aligned host base, with an inaccessible
//! guard on each side (A64-MEM-1). Translated code addresses guest
//! memory as base + zero-extended guest address, so the low 32 bits of a
//! biased stack pointer are the guest value, and a push or pop that wraps
//! past 0 or 4 GiB lands in a guard instead of host memory.
//!
//! The window owns its whole address range. Direct reservations placed in it
//! replace part of the range and hand it back as inaccessible when dropped.
//!
//! A compact window (IOS-MEM-1) gives the guest only the low 2 GiB, the
//! user space of 32-bit Windows, and holds the always-accessible alias of
//! that memory in its upper half. Both views then fit one 4 GiB reservation,
//! which an iOS app without extended virtual addressing can make. Such an
//! app has room for one, so the processes of a family share it and each
//! maps its memory there while it runs (IOS-MEM-2).

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};

pub const GUARD: usize = 128 << 10;
const SPAN: usize = 1 << 32;
/// The guest part of a compact window, and where its alias starts.
const HALF: usize = 1 << 31;
const LEN: usize = SPAN + 2 * GUARD;

pub struct GuestWindow {
    /// Start of the owned range: the lower guard, or the base when an
    /// unmapped range the kernel will not hand out serves as that guard.
    start: NonNull<u8>,
    len: usize,
    /// Host address of guest address 0, 4 GiB aligned.
    base: usize,
    compact: bool,
    /// The memory whose views a compact window holds; 0 when none does.
    tenant: AtomicUsize,
}

// SAFETY: the window only owns an address range; the views placed in it
// belong to their memories, and the tenant is atomic.
unsafe impl Send for GuestWindow {}
unsafe impl Sync for GuestWindow {}

impl GuestWindow {
    /// Reserves the window without committing anything.
    pub fn new() -> Result<Self, String> {
        Self::with_layout(Self::compact())
    }

    /// Whether windows are compact: always on iOS, elsewhere with
    /// `ADV_A64_COMPACT_WINDOW=1` to run the same layout.
    pub fn compact() -> bool {
        static COMPACT: OnceLock<bool> = OnceLock::new();
        *COMPACT.get_or_init(|| {
            cfg!(target_os = "ios")
                || std::env::var_os("ADV_A64_COMPACT_WINDOW").is_some_and(|value| value == "1")
        })
    }

    /// A window for one more memory. Compact windows are shared by the
    /// memories made on one thread, which a process family runs on, so their
    /// tenants never overlap; other layouts give each memory its own.
    pub fn shared() -> Result<Arc<Self>, String> {
        if !Self::compact() {
            return Self::new().map(Arc::new);
        }
        thread_local! {
            static SHARED: RefCell<Weak<GuestWindow>> = const { RefCell::new(Weak::new()) };
        }
        SHARED.with_borrow_mut(|shared| {
            if let Some(window) = shared.upgrade() {
                return Ok(window);
            }
            let window = Arc::new(Self::with_layout(true)?);
            *shared = Arc::downgrade(&window);
            Ok(window)
        })
    }

    /// A new tenant identity for `holds` and `claim`.
    pub fn tenant() -> usize {
        static NEXT: AtomicUsize = AtomicUsize::new(1);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// Whether `tenant`'s views are the ones mapped; always for a window
    /// that is not shared.
    #[inline]
    pub fn holds(&self, tenant: usize) -> bool {
        !self.compact || self.tenant.load(Ordering::Relaxed) == tenant
    }

    /// Clears the guest part and its alias for `tenant`, which then maps its
    /// own views; 0 leaves the window to whichever tenant maps next.
    ///
    /// # Safety
    /// Nothing may use the memory of the tenant that held the window.
    pub(crate) unsafe fn claim(&self, tenant: usize) {
        if self.compact && self.tenant.swap(tenant, Ordering::Relaxed) != tenant {
            // SAFETY: forwarded; the range is the window's own.
            unsafe { vacate(self.base as *mut u8, SPAN) };
        }
    }

    /// Guest bytes a window holds from address 0.
    pub fn guest_span() -> usize {
        if Self::compact() { HALF } else { SPAN }
    }

    pub(crate) fn with_layout(compact: bool) -> Result<Self, String> {
        if compact {
            return Self::search();
        }
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
            len: LEN,
            base: start + GUARD,
            compact: false,
            tenant: AtomicUsize::new(0),
        })
    }

    /// Tries aligned bases upward from where the kernel places a window
    /// itself, never reserving more than one window: an iOS app's whole
    /// address space may hold little more. Darwin ignores hints outside
    /// the range it allocates from, so the search starts inside it.
    fn search() -> Result<Self, String> {
        // SAFETY: a fresh inaccessible reservation at an OS-chosen address.
        let raw = unsafe { reserve(std::ptr::null_mut(), LEN, 0) };
        if raw == libc::MAP_FAILED {
            return Err(format!(
                "reserve guest window: {}",
                std::io::Error::last_os_error()
            ));
        }
        release(raw, LEN);
        let mut base = (raw as usize).next_multiple_of(SPAN);
        for _ in 0..64 {
            let low = base - GUARD;
            // SAFETY: hints only; the kernel picks a free range.
            let raw = unsafe { reserve(low as *mut u8, LEN, 0) };
            if raw as usize == low {
                return Self::found(low, LEN, base);
            }
            release(raw, LEN);
            // The kernel searches upward from a taken hint, so nothing fits
            // between the hint and the range it chose instead.
            let next = if raw == libc::MAP_FAILED {
                base + SPAN
            } else {
                (raw as usize + GUARD)
                    .next_multiple_of(SPAN)
                    .max(base + SPAN)
            };
            // The kernel would not hand out the lower guard. When nothing is
            // mapped there either, it stays inaccessible and serves as one;
            // on iOS this is the end of the dyld shared region.
            // SAFETY: as above.
            let raw = unsafe { reserve(base as *mut u8, SPAN + GUARD, 0) };
            if raw as usize == base && unmapped(low, GUARD) {
                return Self::found(base, SPAN + GUARD, base);
            }
            release(raw, SPAN + GUARD);
            base = next;
        }
        Err("no 4 GiB-aligned guest window fits the address space".into())
    }

    fn found(start: usize, len: usize, base: usize) -> Result<Self, String> {
        Ok(Self {
            start: NonNull::new(start as *mut u8).ok_or("guest window at null")?,
            len,
            base,
            compact: true,
            tenant: AtomicUsize::new(0),
        })
    }

    /// Host address of guest address 0.
    pub fn base(&self) -> usize {
        self.base
    }

    /// Host address of `[offset, offset + len)` past the base, checked to lie
    /// inside the guest part of the window.
    pub(super) fn place(&self, offset: usize, len: usize) -> Result<NonNull<u8>, String> {
        let span = if self.compact { HALF } else { SPAN };
        if offset.checked_add(len).is_none_or(|end| end > span) {
            return Err("placement outside the guest window".into());
        }
        Ok(NonNull::new((self.base() + offset) as *mut u8).unwrap())
    }

    /// Where a compact window holds the alias of a placement made by
    /// `place(offset, len)`; other windows leave the alias to the OS.
    pub(super) fn alias_place(&self, offset: usize) -> Option<NonNull<u8>> {
        self.compact
            .then(|| NonNull::new((self.base() + HALF + offset) as *mut u8).unwrap())
    }
}

impl Drop for GuestWindow {
    fn drop(&mut self) {
        // SAFETY: the window owns exactly this range; placed reservations
        // have already been handed back.
        unsafe { libc::munmap(self.start.as_ptr().cast(), self.len) };
    }
}

/// Undoes a hinted reservation that landed elsewhere.
fn release(raw: *mut libc::c_void, len: usize) {
    if raw != libc::MAP_FAILED {
        // SAFETY: the reservation was just made and is unused.
        unsafe { libc::munmap(raw, len) };
    }
}

/// Whether no page of `[address, address + len)` is mapped.
fn unmapped(address: usize, len: usize) -> bool {
    let Ok(page) = super::os::page_size() else {
        return false;
    };
    (address..address + len).step_by(page).all(|page_address| {
        // SAFETY: an asynchronous msync changes nothing; ENOMEM means
        // unmapped. Darwin's mincore reports holes as nonresident instead.
        let result = unsafe { libc::msync(page_address as *mut _, page, libc::MS_ASYNC) };
        result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOMEM)
    })
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

    // spec: IOS-MEM-1
    #[test]
    fn a_compact_window_holds_the_alias_in_its_upper_half() {
        let window = GuestWindow::with_layout(true).unwrap();
        assert_eq!(window.base() % SPAN, 0);
        assert!(window.place(HALF - 4096, 4096).is_ok());
        assert!(window.place(HALF - 4096, 8192).is_err());
        let page = crate::virtual_memory::os::page_size().unwrap();
        let reservation = Reservation::new_direct_in(&window, 2 * page, 2 * page).unwrap();
        let alias = reservation.alias().unwrap();
        assert_eq!(alias.as_ptr() as usize, window.base() + HALF + 2 * page);
        reservation
            .commit_direct(0, 2 * page, GuestPageProtection::READ)
            .unwrap();
        // SAFETY: both views map the committed bytes.
        unsafe {
            alias.as_ptr().add(page + 8).write(0x5a);
            assert_eq!(*reservation.ptr.as_ptr().add(page + 8), 0x5a);
        }
        drop(reservation);
        // Both halves are inaccessible reservation again, owned by the window.
        let address = window.base() + HALF + 2 * page;
        assert!(!unmapped(address, page));
        let again = Reservation::new_direct_in(&window, 2 * page, page).unwrap();
        assert_eq!(again.alias().unwrap().as_ptr() as usize, address);
    }

    #[test]
    fn unmapped_tells_holes_from_reservations() {
        let page = crate::virtual_memory::os::page_size().unwrap();
        // SAFETY: a fresh reservation, then a hole where its second page was.
        let raw = unsafe { reserve(std::ptr::null_mut(), 3 * page, 0) } as usize;
        assert!(!unmapped(raw, 3 * page));
        unsafe { libc::munmap((raw + page) as *mut _, page) };
        assert!(unmapped(raw + page, page));
        assert!(!unmapped(raw, 2 * page));
        unsafe {
            libc::munmap(raw as *mut _, page);
            libc::munmap((raw + 2 * page) as *mut _, page);
        }
    }
}
