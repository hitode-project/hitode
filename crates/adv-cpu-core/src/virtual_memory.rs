//! Stable host virtual address reservation. The default software-checking backend
//! keeps committed pages RW; the explicit direct backend protects a separate view.
use std::cell::RefCell;

#[cfg(any(unix, test))]
use std::ops::Range;
use std::ptr::NonNull;

const GUEST_PAGE_SIZE: usize = 4096;

mod direct;
#[cfg(unix)]
mod window;
#[cfg(unix)]
pub use direct::DirectFaultView;
#[cfg(unix)]
pub use direct::{FaultClassification, GuestAccess, GuestPageProtection};
#[cfg(unix)]
pub use window::{GUARD as WINDOW_GUARD, GuestWindow};

#[cfg(unix)]
pub fn host_page_size() -> Result<usize, String> {
    os::page_size()
}

#[cfg(unix)]
mod os {
    pub fn page_size() -> Result<usize, String> {
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(size)
            .ok()
            .filter(|&size| size != 0)
            .ok_or_else(|| "cannot determine host page size".into())
    }

    pub unsafe fn reserve(size: usize) -> *mut u8 {
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            std::ptr::null_mut()
        } else {
            p.cast()
        }
    }
    pub unsafe fn commit(p: *mut u8, size: usize) -> bool {
        unsafe { libc::mprotect(p.cast(), size, libc::PROT_READ | libc::PROT_WRITE) == 0 }
    }
    pub unsafe fn decommit(p: *mut u8, size: usize) -> bool {
        unsafe { libc::mprotect(p.cast(), size, libc::PROT_NONE) == 0 }
    }
    pub unsafe fn discard(p: *mut u8, size: usize) {
        // This is a physical-memory hint. Commit supplies the zero-fill
        // guarantee independently of whether the OS retained these bytes.
        unsafe { libc::madvise(p.cast(), size, libc::MADV_DONTNEED) };
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub unsafe fn zero(p: *mut u8, size: usize) {
        // Linux guarantees zero-fill on demand after successful DONTNEED on
        // private anonymous pages. Failed calls still clear explicitly.
        if unsafe { libc::madvise(p.cast(), size, libc::MADV_DONTNEED) == 0 } {
            return;
        }
        unsafe { std::ptr::write_bytes(p, 0, size) };
    }
    pub unsafe fn release(p: *mut u8, size: usize) {
        unsafe {
            libc::munmap(p.cast(), size);
        }
    }
}
pub struct Reservation {
    pub ptr: NonNull<u8>,
    pub len: usize,

    #[cfg(unix)]
    pages: RefCell<UnixPages>,
    #[cfg(unix)]
    direct_alias: Option<direct::SharedAlias>,
    /// Placed inside a `GuestWindow`, which owns the address range.
    #[cfg(unix)]
    placed: bool,
}

// Keep this planner independent of Unix syscalls to check mixed 4/16 KiB
// policies. Native tests still use the actual host size.
#[cfg(any(unix, test))]
struct UnixPages {
    committed: Vec<bool>,
    host_page_size: usize,
    mapped_len: usize,
    direct: Option<direct::DirectPages>,
}

#[cfg(any(unix, test))]
struct PagePlan {
    guest_pages: Range<usize>,
    // Whole host pages whose accessibility changes in this operation.
    host_spans: Vec<Range<usize>>,
    // Guest bytes in host pages that must stay RW for live neighbours.
    clear_spans: Vec<Range<usize>>,
    protection_changes: Vec<direct::HostChange>,
}

#[cfg(any(unix, test))]
impl UnixPages {
    fn new(len: usize, host_page_size: usize) -> Result<Self, String> {
        if host_page_size == 0 || !host_page_size.is_multiple_of(GUEST_PAGE_SIZE) {
            return Err("host page size must be a multiple of the 4 KiB guest page".into());
        }
        let mapped_len = len
            .checked_add(host_page_size - 1)
            .map(|len| len / host_page_size * host_page_size)
            .ok_or("host reservation length overflow")?;
        Ok(Self {
            committed: vec![false; len / GUEST_PAGE_SIZE],
            host_page_size,
            mapped_len,
            direct: None,
        })
    }

    fn host_guest_pages(&self, host: usize) -> Range<usize> {
        let start = host * (self.host_page_size / GUEST_PAGE_SIZE);
        start..(start + self.host_page_size / GUEST_PAGE_SIZE).min(self.committed.len())
    }

    fn push_span(spans: &mut Vec<Range<usize>>, span: Range<usize>) {
        if let Some(last) = spans.last_mut()
            && last.end == span.start
        {
            last.end = span.end;
        } else {
            spans.push(span);
        }
    }

    // Callers validate nonempty, guest-aligned bounds before building a plan.
    fn plan(&self, offset: usize, end: usize, commit: bool) -> Result<PagePlan, String> {
        let guest_pages = offset / GUEST_PAGE_SIZE..end / GUEST_PAGE_SIZE;
        if commit && self.committed[guest_pages.clone()].iter().any(|&page| page) {
            return Err("overlapping reservation commit".into());
        }
        let mut plan = PagePlan {
            guest_pages,
            host_spans: Vec::new(),
            clear_spans: Vec::new(),
            protection_changes: Vec::new(),
        };
        for host in offset / self.host_page_size..=(end - 1) / self.host_page_size {
            let pages = self.host_guest_pages(host);
            let has_live_pages = self.committed[pages.clone()].iter().any(|&page| page);
            let host_start = host * self.host_page_size;
            let host_end = host_start + self.host_page_size;
            if commit {
                if !has_live_pages {
                    Self::push_span(&mut plan.host_spans, host_start..host_end);
                } else {
                    Self::push_span(
                        &mut plan.clear_spans,
                        offset.max(host_start)..end.min(host_end),
                    );
                }
            } else {
                let retains_live_pages = pages
                    .clone()
                    .any(|page| self.committed[page] && !plan.guest_pages.contains(&page));
                if retains_live_pages {
                    Self::push_span(
                        &mut plan.clear_spans,
                        offset.max(host_start)..end.min(host_end),
                    );
                } else if has_live_pages {
                    Self::push_span(&mut plan.host_spans, host_start..host_end);
                }
            }
        }
        Ok(plan)
    }

    fn apply(&mut self, plan: &PagePlan, commit: bool) {
        self.committed[plan.guest_pages.clone()].fill(commit);
    }
}

impl Reservation {
    pub fn new(len: usize) -> Result<Self, String> {
        if len == 0 || !len.is_multiple_of(GUEST_PAGE_SIZE) {
            return Err("reservation must be page aligned and nonempty".into());
        }
        #[cfg(unix)]
        let pages = UnixPages::new(len, os::page_size()?)?;
        #[cfg(unix)]
        let mapped_len = pages.mapped_len;

        let ptr = NonNull::new(unsafe { os::reserve(mapped_len) })
            .ok_or_else(|| format!("reserve guest memory: {}", std::io::Error::last_os_error()))?;
        Ok(Self {
            ptr,
            len,
            #[cfg(unix)]
            pages: RefCell::new(pages),
            #[cfg(unix)]
            direct_alias: None,
            #[cfg(unix)]
            placed: false,
        })
    }
    pub fn commit(&self, offset: usize, len: usize) -> Result<(), String> {
        #[cfg(unix)]
        return self.commit_unix(offset, len);
    }
    pub fn decommit(&self, offset: usize, len: usize) -> Result<(), String> {
        #[cfg(unix)]
        return self.decommit_unix(offset, len);
    }

    fn bounds(&self, offset: usize, len: usize) -> Result<usize, String> {
        if !offset.is_multiple_of(GUEST_PAGE_SIZE)
            || len == 0
            || !len.is_multiple_of(GUEST_PAGE_SIZE)
        {
            return Err("unaligned reservation operation".into());
        }
        offset
            .checked_add(len)
            .filter(|&end| end <= self.len)
            .ok_or_else(|| "outside reservation".into())
    }
}

#[cfg(unix)]
impl Reservation {
    // Change all host protections before modifying bytes or guest bookkeeping.
    // A failed operation leaves the caller's original mapping usable.
    fn change_host_spans(&self, spans: &[Range<usize>], commit: bool) -> Result<(), String> {
        for (index, span) in spans.iter().enumerate() {
            let ptr = unsafe { self.ptr.as_ptr().add(span.start) };
            let ok = unsafe {
                if commit {
                    os::commit(ptr, span.len())
                } else {
                    os::decommit(ptr, span.len())
                }
            };
            if !ok {
                let error = std::io::Error::last_os_error();
                // Include the failed span in case mprotect changed a prefix.
                for span in spans[..=index].iter().rev() {
                    let ptr = unsafe { self.ptr.as_ptr().add(span.start) };
                    let restored = unsafe {
                        if commit {
                            os::decommit(ptr, span.len())
                        } else {
                            os::commit(ptr, span.len())
                        }
                    };
                    assert!(restored, "cannot roll back guest host protection");
                }
                let operation = if commit { "commit" } else { "decommit" };
                return Err(format!("{operation} guest memory: {error}"));
            }
        }
        Ok(())
    }

    fn commit_unix(&self, offset: usize, len: usize) -> Result<(), String> {
        if self.direct_alias.is_some() {
            return self.commit_direct(offset, len, GuestPageProtection::READ_WRITE);
        }
        let end = self.bounds(offset, len)?;
        let mut pages = self.pages.borrow_mut();
        let plan = pages.plan(offset, end, true)?;
        self.change_host_spans(&plan.host_spans, true)?;
        // Only wholly empty host pages can be reset without touching live
        // neighbours. Partial host pages retain explicit guest-range clearing.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            for span in &plan.host_spans {
                unsafe { os::zero(self.ptr.as_ptr().add(span.start), span.len()) };
            }
            for span in &plan.clear_spans {
                unsafe { std::ptr::write_bytes(self.ptr.as_ptr().add(span.start), 0, span.len()) };
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        unsafe {
            std::ptr::write_bytes(self.ptr.as_ptr().add(offset), 0, len)
        };
        pages.apply(&plan, true);
        Ok(())
    }

    fn decommit_unix(&self, offset: usize, len: usize) -> Result<(), String> {
        if self.direct_alias.is_some() {
            return self.decommit_direct(offset, len);
        }
        let end = self.bounds(offset, len)?;
        let mut pages = self.pages.borrow_mut();
        let plan = pages.plan(offset, end, false)?;
        self.change_host_spans(&plan.host_spans, false)?;
        for span in &plan.clear_spans {
            unsafe { std::ptr::write_bytes(self.ptr.as_ptr().add(span.start), 0, span.len()) };
        }
        pages.apply(&plan, false);
        for span in &plan.host_spans {
            // Only wholly empty host pages may be discarded. Failure of this
            // advisory call cannot expose bytes: the span is already NONE and
            // every future guest commit initializes its own bytes again.
            unsafe { os::discard(self.ptr.as_ptr().add(span.start), span.len()) };
        }
        Ok(())
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            let len = self.pages.get_mut().mapped_len;
            if self.placed {
                // Hand the range back to the window as inaccessible
                // reservation in one step, so no other mapping can land there.
                window::vacate(self.ptr.as_ptr(), len);
            } else {
                os::release(self.ptr.as_ptr(), len)
            }
        }
    }
}

#[cfg(test)]
// Assertions intentionally compare lists of spans, not collected range elements.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    const GUEST: usize = GUEST_PAGE_SIZE;
    const HOST_16K: usize = 4 * GUEST;

    fn commit(pages: &mut UnixPages, start: usize, end: usize) -> PagePlan {
        let plan = pages.plan(start, end, true).unwrap();
        pages.apply(&plan, true);
        plan
    }

    fn decommit(pages: &mut UnixPages, start: usize, end: usize) -> PagePlan {
        let plan = pages.plan(start, end, false).unwrap();
        pages.apply(&plan, false);
        plan
    }

    #[test]
    fn mixed_4k_guest_16k_host_page_lifetime() {
        let mut pages = UnixPages::new(HOST_16K, HOST_16K).unwrap();
        assert_eq!(commit(&mut pages, 0, GUEST).host_spans, [0..HOST_16K]);
        assert!(
            commit(&mut pages, 2 * GUEST, 3 * GUEST)
                .host_spans
                .is_empty()
        );

        let plan = decommit(&mut pages, 0, GUEST);
        assert!(plan.host_spans.is_empty());
        assert_eq!(plan.clear_spans, [0..GUEST]);
        assert!(pages.committed[2]);

        // Reusing an empty guest slot must not restrict its live neighbour.
        assert!(commit(&mut pages, 0, GUEST).host_spans.is_empty());
        assert!(
            decommit(&mut pages, 2 * GUEST, 3 * GUEST)
                .host_spans
                .is_empty()
        );
        let plan = decommit(&mut pages, 0, GUEST);
        assert_eq!(plan.host_spans, [0..HOST_16K]);
        assert!(plan.clear_spans.is_empty());
        assert_eq!(
            commit(&mut pages, GUEST, 2 * GUEST).host_spans,
            [0..HOST_16K]
        );
    }

    #[test]
    fn partial_decommit_retains_both_boundary_host_pages() {
        let len = 3 * HOST_16K;
        let mut pages = UnixPages::new(len, HOST_16K).unwrap();
        commit(&mut pages, 0, len);
        let plan = decommit(&mut pages, GUEST, len - GUEST);
        assert_eq!(plan.host_spans, [HOST_16K..2 * HOST_16K]);
        assert_eq!(
            plan.clear_spans,
            [GUEST..HOST_16K, 2 * HOST_16K..len - GUEST]
        );
        assert!(pages.committed[0]);
        assert!(pages.committed[pages.committed.len() - 1]);
        assert!(
            pages.committed[1..pages.committed.len() - 1]
                .iter()
                .all(|p| !p)
        );
    }

    #[test]
    fn sparse_decommit_changes_only_previously_live_host_pages() {
        let len = 3 * HOST_16K;
        let mut pages = UnixPages::new(len, HOST_16K).unwrap();
        commit(&mut pages, HOST_16K + GUEST, HOST_16K + 2 * GUEST);
        let plan = decommit(&mut pages, 0, len);
        assert_eq!(plan.host_spans, [HOST_16K..2 * HOST_16K]);
        assert!(plan.clear_spans.is_empty());
        let plan = decommit(&mut pages, 0, len);
        assert!(plan.host_spans.is_empty());
        assert!(plan.clear_spans.is_empty());

        commit(&mut pages, 0, GUEST);
        commit(&mut pages, 2 * HOST_16K, 2 * HOST_16K + GUEST);
        let plan = decommit(&mut pages, 0, len);
        assert_eq!(plan.host_spans, [0..HOST_16K, 2 * HOST_16K..len]);
    }

    #[test]
    fn commit_in_live_boundary_host_only_enables_new_host_pages() {
        let len = 3 * HOST_16K;
        let mut pages = UnixPages::new(len, HOST_16K).unwrap();
        commit(&mut pages, HOST_16K, HOST_16K + GUEST);
        // One operation may span an accessible host page whose guest slots
        // inside the operation are all empty; it needs no host transition.
        let plan = commit(&mut pages, HOST_16K + GUEST, len);
        assert_eq!(plan.host_spans, [2 * HOST_16K..len]);
        assert_eq!(plan.clear_spans, [HOST_16K + GUEST..2 * HOST_16K]);
        assert!(pages.committed[HOST_16K / GUEST]);
    }

    // spec: A64-MEM-3
    #[test]
    fn guest_aligned_reservation_rounds_host_tail() {
        let mut pages = UnixPages::new(5 * GUEST, HOST_16K).unwrap();
        assert_eq!(pages.mapped_len, 2 * HOST_16K);
        assert_eq!(pages.committed.len(), 5);
        assert_eq!(
            commit(&mut pages, 4 * GUEST, 5 * GUEST).host_spans,
            [HOST_16K..2 * HOST_16K]
        );
        assert_eq!(
            decommit(&mut pages, 4 * GUEST, 5 * GUEST).host_spans,
            [HOST_16K..2 * HOST_16K]
        );
    }

    #[test]
    fn overlapping_commit_keeps_original_plan_state() {
        let mut pages = UnixPages::new(HOST_16K, HOST_16K).unwrap();
        commit(&mut pages, GUEST, 2 * GUEST);
        let original = pages.committed.clone();
        assert!(pages.plan(0, 3 * GUEST, true).is_err());
        assert_eq!(pages.committed, original);
    }

    #[test]
    fn matching_4k_host_pages_use_exact_ranges() {
        let mut pages = UnixPages::new(5 * GUEST, GUEST).unwrap();
        assert_eq!(
            commit(&mut pages, GUEST, 4 * GUEST).host_spans,
            [GUEST..4 * GUEST]
        );
        let plan = decommit(&mut pages, 2 * GUEST, 3 * GUEST);
        assert_eq!(plan.host_spans, [2 * GUEST..3 * GUEST]);
        assert!(plan.clear_spans.is_empty());
        assert!(pages.committed[1]);
        assert!(pages.committed[3]);
    }

    #[test]
    fn unsupported_host_granularity_and_rounding_overflow_are_rejected() {
        for host in [0, GUEST / 2, GUEST + 1] {
            assert!(UnixPages::new(GUEST, host).is_err());
        }
        let len = usize::MAX / GUEST * GUEST;
        assert!(UnixPages::new(len, HOST_16K).is_err());
    }

    #[test]
    fn reservation_operations_require_nonempty_guest_page_bounds() {
        let reservation = Reservation::new(GUEST).unwrap();
        for (offset, len) in [
            (0, 0),
            (1, GUEST),
            (0, 1),
            (GUEST, GUEST),
            (usize::MAX, GUEST),
        ] {
            assert!(reservation.commit(offset, len).is_err());
            assert!(reservation.decommit(offset, len).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_mixed_host_recommit_zeroes_slots_and_preserves_boundary_neighbours() {
        let host = os::page_size().unwrap().max(HOST_16K);
        let len = 3 * host;
        let reservation = Reservation::new(len).unwrap();
        // Exercise the mixed-host policy on 4 KiB machines as well as native
        // larger-page hosts; all syscall spans remain native-page aligned.
        *reservation.pages.borrow_mut() = UnixPages::new(len, host).unwrap();
        reservation.commit(0, len).unwrap();
        unsafe { std::ptr::write_bytes(reservation.ptr.as_ptr(), 0x5a, len) };
        reservation.decommit(GUEST, len - 2 * GUEST).unwrap();
        reservation.commit(GUEST, len - 2 * GUEST).unwrap();
        let bytes = unsafe { std::slice::from_raw_parts(reservation.ptr.as_ptr(), len) };
        assert!(bytes[..GUEST].iter().all(|&byte| byte == 0x5a));
        assert!(bytes[GUEST..len - GUEST].iter().all(|&byte| byte == 0));
        assert!(bytes[len - GUEST..].iter().all(|&byte| byte == 0x5a));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn unix_failed_zero_reset_falls_back_to_exact_byte_clearing() {
        let host = os::page_size().unwrap();
        let reservation = Reservation::new(host).unwrap();
        reservation.commit(0, host).unwrap();
        unsafe { std::ptr::write_bytes(reservation.ptr.as_ptr(), 0x5a, host) };
        // An unaligned address makes DONTNEED fail; the fallback must clear
        // exactly the requested bytes and leave both sentinels unchanged.
        unsafe { os::zero(reservation.ptr.as_ptr().add(1), host - 2) };
        let bytes = unsafe { std::slice::from_raw_parts(reservation.ptr.as_ptr(), host) };
        assert_eq!(bytes[0], 0x5a);
        assert!(bytes[1..host - 1].iter().all(|&byte| byte == 0));
        assert_eq!(bytes[host - 1], 0x5a);
    }

    #[cfg(unix)]
    #[test]
    fn unix_actual_host_pages_clear_recommits_and_preserve_neighbours() {
        let host = os::page_size().unwrap();
        eprintln!("Unix reservation native test: actual host page size = {host} bytes");
        // Include a guest-aligned partial final host page when host > 4 KiB.
        let len = 2 * host + GUEST;
        let reservation = Reservation::new(len).unwrap();
        reservation.commit(0, len).unwrap();
        for page in 0..len / GUEST {
            unsafe {
                std::ptr::write_bytes(
                    reservation.ptr.as_ptr().add(page * GUEST),
                    page as u8 + 1,
                    GUEST,
                )
            };
        }

        reservation.decommit(GUEST, GUEST).unwrap();
        // Read only live guest pages; a released guest page may share a host
        // mapping, but software guest permissions must still reject access.
        for page in [0, 2, len / GUEST - 1] {
            let bytes = unsafe {
                std::slice::from_raw_parts(reservation.ptr.as_ptr().add(page * GUEST), GUEST)
            };
            assert!(bytes.iter().all(|&byte| byte == page as u8 + 1));
        }
        reservation.commit(GUEST, GUEST).unwrap();
        let bytes =
            unsafe { std::slice::from_raw_parts(reservation.ptr.as_ptr().add(GUEST), GUEST) };
        assert!(bytes.iter().all(|&byte| byte == 0));
        assert!(reservation.commit(GUEST, GUEST).is_err());

        reservation.decommit(0, len).unwrap();
        reservation.commit(0, len).unwrap();
        let bytes = unsafe { std::slice::from_raw_parts(reservation.ptr.as_ptr(), len) };
        assert!(bytes.iter().all(|&byte| byte == 0));
    }
}
