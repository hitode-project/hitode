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
//! maps its memory there while it runs (IOS-MEM-2). An app reserves it
//! before anything else can take that range and keeps it (IOS-MEM-3).

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

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
    /// Kept for the next family when the last memory drops (IOS-MEM-3).
    preserved: bool,
}

/// The preserved window while no family holds it.
static PRESERVED: Mutex<Option<Arc<GuestWindow>>> = Mutex::new(None);

/// The address space the first `preserve` saw.
#[cfg(target_vendor = "apple")]
static LAUNCH: OnceLock<AddressSpace> = OnceLock::new();

/// Mapped ranges an address space record keeps at most, as reports allow.
#[cfg(target_vendor = "apple")]
const MAPPED_RANGES: usize = 16;

/// The address space a launch reservation saw (IOS-MEM-5).
#[cfg(target_vendor = "apple")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddressSpace {
    /// The end of the range the app may map.
    pub max: u64,
    /// The window's base; none when no window fit.
    pub window: Option<u64>,
    /// When no window fit, the mapped ranges from the lowest candidate's
    /// guard up to `max`, merged where they touch.
    pub mapped: Vec<std::ops::Range<u64>>,
}

#[cfg(target_vendor = "apple")]
impl AddressSpace {
    fn observe(found: &Result<GuestWindow, usize>) -> Self {
        let max = darwin::max_address();
        match found {
            Ok(window) => Self {
                max,
                window: Some(window.base as u64),
                mapped: Vec::new(),
            },
            Err(from) => Self {
                max,
                window: None,
                mapped: darwin::mapped(*from as u64, max),
            },
        }
    }
}

#[cfg(target_vendor = "apple")]
impl std::fmt::Display for AddressSpace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "max {:#x}", self.max)?;
        if let Some(window) = self.window {
            write!(f, ", window {window:#x}")?;
        }
        for range in &self.mapped {
            write!(f, ", mapped {:#x}-{:#x}", range.start, range.end)?;
        }
        Ok(())
    }
}

fn missed(from: usize) -> String {
    format!("no 4 GiB-aligned guest window fits the address space above {from:#x}")
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
            let window = match Self::take_preserved() {
                Some(window) => window,
                None => Arc::new(Self::with_layout(true)?),
            };
            *shared = Arc::downgrade(&window);
            Ok(window)
        })
    }

    /// Reserves a compact window for the life of the process (IOS-MEM-3).
    /// On iOS, call it first thing at launch: WebKit's JavaScript heap then
    /// reserves 2 GiB at the start of the only free range a window fits in.
    pub fn preserve() -> Result<(), String> {
        let mut slot = PRESERVED.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            let found = Self::search();
            #[cfg(target_vendor = "apple")]
            let _ = LAUNCH.set(AddressSpace::observe(&found));
            let mut window = found.map_err(missed)?;
            window.preserved = true;
            *slot = Some(Arc::new(window));
        }
        Ok(())
    }

    /// What the first `preserve` saw (IOS-MEM-5).
    #[cfg(target_vendor = "apple")]
    pub fn launch_address_space() -> Option<AddressSpace> {
        LAUNCH.get().cloned()
    }

    /// The preserved window, unless a family holds it.
    fn take_preserved() -> Option<Arc<Self>> {
        PRESERVED
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
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
            return Self::search().map_err(missed);
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
            preserved: false,
        })
    }

    /// Tries aligned bases upward from where the kernel places a window
    /// itself, never reserving more than one window: an iOS app's whole
    /// address space may hold little more. Darwin ignores hints outside
    /// the range it allocates from, so the search starts inside it.
    /// Failing, it returns where the lowest candidate's guard starts.
    fn search() -> Result<Self, usize> {
        // SAFETY: a fresh inaccessible reservation at an OS-chosen address.
        let raw = unsafe { reserve(std::ptr::null_mut(), LEN, 0) };
        if raw == libc::MAP_FAILED {
            return Err(SPAN - GUARD);
        }
        release(raw, LEN);
        let first = (raw as usize).next_multiple_of(SPAN);
        let mut base = first;
        for _ in 0..64 {
            let low = base - GUARD;
            // SAFETY: hints only; the kernel picks a free range.
            let raw = unsafe { reserve(low as *mut u8, LEN, 0) };
            if raw as usize == low {
                return Ok(Self::found(low, LEN, base));
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
            // The kernel would not hand out the lower guard. When nothing
            // there can become accessible either, it serves as one; on iOS
            // this is the padding that ends the dyld shared region.
            // SAFETY: as above.
            let raw = unsafe { reserve(base as *mut u8, SPAN + GUARD, 0) };
            if raw as usize == base && inaccessible(low, GUARD) {
                return Ok(Self::found(base, SPAN + GUARD, base));
            }
            release(raw, SPAN + GUARD);
            base = next;
        }
        Err(first - GUARD)
    }

    fn found(start: usize, len: usize, base: usize) -> Self {
        Self {
            start: NonNull::new(start as *mut u8).expect("the kernel never maps at null"),
            len,
            base,
            compact: true,
            tenant: AtomicUsize::new(0),
            preserved: false,
        }
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
        if self.preserved {
            // The range stays reserved, already inaccessible again, for the
            // next family.
            let window = Self {
                tenant: AtomicUsize::new(0),
                ..*self
            };
            *PRESERVED.lock().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(window));
            return;
        }
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

/// Whether no page of `[address, address + len)` is accessible or can be
/// made so. On Darwin the dyld shared region ends in padding that never
/// opens, which `msync` counts as mapped.
fn inaccessible(address: usize, len: usize) -> bool {
    #[cfg(target_vendor = "apple")]
    {
        darwin::sealed(address as u64, (address + len) as u64)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        unmapped(address, len)
    }
}

/// Whether no page of `[address, address + len)` is mapped.
#[cfg(any(test, not(target_vendor = "apple")))]
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

#[cfg(target_vendor = "apple")]
mod darwin {
    use std::ops::Range;

    unsafe extern "C" {
        static mach_task_self_: libc::mach_port_t;
        fn task_info(
            task: libc::mach_port_t,
            flavor: u32,
            info: *mut i32,
            count: *mut u32,
        ) -> libc::c_int;
        fn mach_vm_region(
            task: libc::mach_port_t,
            address: *mut u64,
            size: *mut u64,
            flavor: libc::c_int,
            info: *mut i32,
            count: *mut u32,
            object_name: *mut libc::mach_port_t,
        ) -> libc::c_int;
        fn mach_vm_region_recurse(
            task: libc::mach_port_t,
            address: *mut u64,
            size: *mut u64,
            depth: *mut u32,
            info: *mut i32,
            count: *mut u32,
        ) -> libc::c_int;
    }

    /// `vm_region_submap_info_64` in 32-bit words, and the words of its
    /// `max_protection` and `is_submap`.
    const SUBMAP_INFO_WORDS: usize = 19;
    const MAX_PROTECTION: usize = 1;
    const IS_SUBMAP: usize = 12;
    /// Deeper than Darwin nests submaps.
    const SUBMAP_DEPTH: u32 = 8;
    /// `mach_vm_region_recurse` found no mapping at or above the address.
    const KERN_INVALID_ADDRESS: libc::c_int = 1;

    /// `TASK_VM_INFO` up to revision 2's `max_address`, in 64-bit words.
    const VM_INFO_WORDS: usize = 21;

    /// The end of the range the task may map; 0 when the kernel will not say.
    pub fn max_address() -> u64 {
        const TASK_VM_INFO: u32 = 22;
        let mut info = [0u64; VM_INFO_WORDS];
        let mut count = (VM_INFO_WORDS * 2) as u32;
        // SAFETY: the buffer holds `count` 32-bit words.
        let result = unsafe {
            task_info(
                mach_task_self_,
                TASK_VM_INFO,
                info.as_mut_ptr().cast(),
                &mut count,
            )
        };
        if result == 0 && count as usize >= VM_INFO_WORDS * 2 {
            info[VM_INFO_WORDS - 1]
        } else {
            0
        }
    }

    /// Whether no mapping in `[from, to)` can ever be accessed: each one
    /// there, looked up through submaps such as the dyld shared region, has
    /// no maximum protection. A failed lookup answers no.
    pub fn sealed(from: u64, to: u64) -> bool {
        let mut address = from;
        while address < to {
            match innermost(address) {
                Ok(Some((start, end, max_protection))) if start < to => {
                    if max_protection != 0 {
                        return false;
                    }
                    address = end;
                }
                Ok(_) => return true,
                Err(()) => return false,
            }
        }
        true
    }

    /// The innermost mapping holding or following `address`: its start, end
    /// and maximum protection, or `None` when no mapping follows. Fails when
    /// the kernel refuses the lookup or submaps nest past `SUBMAP_DEPTH`.
    fn innermost(address: u64) -> Result<Option<(u64, u64, i32)>, ()> {
        let mut depth = 0;
        while depth < SUBMAP_DEPTH {
            let (mut start, mut size) = (address, 0);
            let mut info = [0i32; SUBMAP_INFO_WORDS];
            let mut count = SUBMAP_INFO_WORDS as u32;
            // SAFETY: out-parameters for the mapping at or above `address`.
            let result = unsafe {
                mach_vm_region_recurse(
                    mach_task_self_,
                    &mut start,
                    &mut size,
                    &mut depth,
                    info.as_mut_ptr(),
                    &mut count,
                )
            };
            match result {
                0 => {}
                KERN_INVALID_ADDRESS => return Ok(None),
                _ => return Err(()),
            }
            if info[IS_SUBMAP] == 0 {
                return Ok(Some((start, start + size, info[MAX_PROTECTION])));
            }
            depth += 1;
        }
        Err(())
    }

    /// Mapped ranges in `[from, to)`, merged where they touch, at most
    /// `MAPPED_RANGES` of them.
    pub fn mapped(from: u64, to: u64) -> Vec<Range<u64>> {
        const VM_REGION_BASIC_INFO_64: libc::c_int = 9;
        let mut ranges: Vec<Range<u64>> = Vec::new();
        let mut address = from;
        while address < to {
            let (mut size, mut name) = (0, 0);
            let mut info = [0i32; 9];
            let mut count = info.len() as u32;
            // SAFETY: out-parameters for the region at or above `address`.
            let result = unsafe {
                mach_vm_region(
                    mach_task_self_,
                    &mut address,
                    &mut size,
                    VM_REGION_BASIC_INFO_64,
                    info.as_mut_ptr(),
                    &mut count,
                    &mut name,
                )
            };
            if result != 0 || address >= to {
                break;
            }
            // The region holding `from` may start below it.
            let (start, end) = (address.max(from), (address + size).min(to));
            if let Some(last) = ranges.last_mut().filter(|last| last.end == start) {
                last.end = end;
            } else if ranges.len() == super::MAPPED_RANGES {
                break;
            } else {
                ranges.push(start..end);
            }
            address = end;
        }
        ranges
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

    // spec: IOS-MEM-3
    #[test]
    fn a_preserved_window_stays_reserved_for_the_next_family() {
        GuestWindow::preserve().unwrap();
        let window = GuestWindow::take_preserved().unwrap();
        let base = window.base();
        assert!(GuestWindow::take_preserved().is_none());
        let page = crate::virtual_memory::os::page_size().unwrap();
        unsafe { window.claim(GuestWindow::tenant()) };
        drop(Reservation::new_direct_in(&window, 0, page).unwrap());
        drop(window);
        let again = GuestWindow::take_preserved().unwrap();
        assert_eq!(again.base(), base);
        assert_eq!(again.tenant.load(Ordering::Relaxed), 0);
        assert!(!unmapped(base, page));
    }

    // spec: IOS-MEM-5
    #[cfg(target_vendor = "apple")]
    #[test]
    fn mapped_ranges_merge_where_they_touch_and_skip_holes() {
        assert!(darwin::max_address() > 0);
        let page = crate::virtual_memory::os::page_size().unwrap() as u64;
        // SAFETY: a fresh reservation, then a hole where its third page was.
        let raw = unsafe { reserve(std::ptr::null_mut(), 4 * page as usize, 0) } as u64;
        unsafe { libc::munmap((raw + 2 * page) as *mut _, page as usize) };
        assert_eq!(
            darwin::mapped(raw + page / 2, raw + 4 * page),
            [
                raw + page / 2..raw + 2 * page,
                raw + 3 * page..raw + 4 * page
            ]
        );
        unsafe {
            libc::munmap(raw as *mut _, 2 * page as usize);
            libc::munmap((raw + 3 * page) as *mut _, page as usize);
        }
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

    // spec: IOS-MEM-1
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_mapping_that_can_never_open_serves_as_a_guard() {
        unsafe extern "C" {
            static mach_task_self_: libc::mach_port_t;
            fn mach_vm_protect(
                task: libc::mach_port_t,
                address: u64,
                size: u64,
                set_maximum: libc::boolean_t,
                protection: libc::c_int,
            ) -> libc::c_int;
        }
        let page = crate::virtual_memory::os::page_size().unwrap();
        // SAFETY: a fresh reservation whose middle page then loses even its
        // maximum protection, as the dyld shared region's padding has.
        let raw = unsafe { reserve(std::ptr::null_mut(), 3 * page, 0) } as usize;
        let sealed =
            unsafe { mach_vm_protect(mach_task_self_, (raw + page) as u64, page as u64, 1, 0) };
        assert_eq!(sealed, 0);
        assert!(inaccessible(raw + page, page));
        // An inaccessible reservation can still be opened.
        assert!(!inaccessible(raw, 2 * page));
        unsafe { libc::munmap(raw as *mut _, 3 * page) };
    }

    // spec: IOS-MEM-1
    #[cfg(all(target_vendor = "apple", target_arch = "aarch64"))]
    #[test]
    fn the_dyld_shared_region_padding_serves_as_a_guard() {
        unsafe extern "C" {
            fn _dyld_get_shared_cache_range(length: *mut usize) -> *const libc::c_void;
        }
        /// Where arm64 Darwin nests the dyld shared region.
        const SHARED_REGION: usize = 0x1_8000_0000;
        let page = crate::virtual_memory::os::page_size().unwrap();
        let mut len = 0;
        // SAFETY: an out-parameter for the cache's length.
        let cache = unsafe { _dyld_get_shared_cache_range(&mut len) } as usize;
        // The cache slides inside the region, and padding fills the slide.
        if cache < SHARED_REGION + page {
            return;
        }
        assert!(inaccessible(SHARED_REGION, cache - SHARED_REGION));
        assert!(!inaccessible(cache, page));
    }
}
