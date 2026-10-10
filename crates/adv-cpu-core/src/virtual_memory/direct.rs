//! Opt-in direct guest view with a private, permanently accessible checked alias.
//!
//! Native callers must pin a fault view for the complete execution lease and
//! stop all users before changing mappings. This owner is deliberately not Sync.
//! Guest x86 code bytes are data here, never host-executable memory. Shared/file
//! mappings and executor invalidation are separate contracts, not enabled here.
#[cfg(any(unix, test))]
use crate::memory::{EXEC, READ, WRITE};

/// Guest access attempted by a registered native access site.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuestAccess {
    Read,
    Write,
    Execute,
}

/// Guest permission bits and an independently maintained code-write barrier.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GuestPageProtection {
    permissions: u8,
    code_write_protected: bool,
}

#[cfg(any(unix, test))]
impl GuestPageProtection {
    pub const NONE: Self = Self {
        permissions: 0,
        code_write_protected: false,
    };
    pub const READ: Self = Self {
        permissions: READ,
        code_write_protected: false,
    };
    pub const READ_WRITE: Self = Self {
        permissions: READ | WRITE,
        code_write_protected: false,
    };

    pub fn new(permissions: u8, code_write_protected: bool) -> Result<Self, String> {
        if permissions & !(READ | WRITE | EXEC) != 0 {
            return Err("unsupported direct guest permission bits".into());
        }
        Ok(Self {
            permissions,
            code_write_protected,
        })
    }

    pub fn permissions(self) -> u8 {
        self.permissions
    }
    pub fn code_write_protected(self) -> bool {
        self.code_write_protected
    }

    // Only the unix fault classifier consults permissions.
    #[cfg(unix)]
    fn allows(self, access: GuestAccess) -> bool {
        let bit = match access {
            GuestAccess::Read => READ,
            GuestAccess::Write => WRITE,
            GuestAccess::Execute => EXEC,
        };
        self.permissions & bit != 0
    }
}

/// Classification only; no signal installation, guest exception dispatch, or
/// retry is implied. A code barrier is reported only for a guest-permitted write.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultClassification {
    OutsideGuest,
    /// Guest and host permissions allow access. This fault is not explained by
    /// page protection and must remain with the original host fault chain.
    Accessible,
    /// Guest-legal access requires the checked alias. Includes permissions the
    /// host cannot express safely, such as write-only pages on x86 hosts.
    MixedPage,
    AccessViolation,
    CodeWriteProtection,
}

#[cfg(any(unix, test))]
use super::{GUEST_PAGE_SIZE, PagePlan, UnixPages};
#[cfg(any(unix, test))]
use std::ops::Range;

#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NativeProtection {
    None,
    Read,
    ReadWrite,
}

#[cfg(any(unix, test))]
impl NativeProtection {
    fn for_guest(protection: GuestPageProtection) -> Self {
        // Some CPUs make writable pages readable. Restrict write-only guest
        // pages to NONE and use the alias, rather than granting an illegal read.
        if protection.permissions & READ == 0 {
            Self::None
        } else if protection.permissions & WRITE != 0 && !protection.code_write_protected {
            Self::ReadWrite
        } else {
            Self::Read
        }
    }

    fn intersect(self, other: Self) -> Self {
        match (self, other) {
            (Self::None, _) | (_, Self::None) => Self::None,
            (Self::Read, _) | (_, Self::Read) => Self::Read,
            _ => Self::ReadWrite,
        }
    }

    // Only the unix fault classifier consults permissions.
    #[cfg(unix)]
    fn allows(self, access: GuestAccess) -> bool {
        match access {
            GuestAccess::Read => self != Self::None,
            GuestAccess::Write => self == Self::ReadWrite,
            // Host execution of guest bytes is never part of this backend.
            GuestAccess::Execute => false,
        }
    }

    #[cfg(unix)]
    fn flags(self) -> libc::c_int {
        match self {
            Self::None => libc::PROT_NONE,
            Self::Read => libc::PROT_READ,
            Self::ReadWrite => libc::PROT_READ | libc::PROT_WRITE,
        }
    }
}

#[cfg(any(unix, test))]
pub(super) struct DirectPages {
    guest: Vec<GuestPageProtection>,
    host: Vec<NativeProtection>,
}

#[cfg(any(unix, test))]
pub(super) struct HostChange {
    span: Range<usize>,
    before: NativeProtection,
    after: NativeProtection,
}

#[cfg(any(unix, test))]
#[derive(Clone, Copy)]
enum Mutation {
    Commit(GuestPageProtection),
    Protect(GuestPageProtection),
    Decommit,
}

#[cfg(any(unix, test))]
impl UnixPages {
    fn enable_direct(&mut self) {
        self.direct = Some(DirectPages {
            guest: vec![GuestPageProtection::NONE; self.committed.len()],
            host: vec![NativeProtection::None; self.mapped_len / self.host_page_size],
        });
    }

    fn plan_direct(
        &self,
        offset: usize,
        end: usize,
        mutation: Mutation,
    ) -> Result<PagePlan, String> {
        let direct = self
            .direct
            .as_ref()
            .ok_or("direct protection is not enabled")?;
        let mut plan = match mutation {
            Mutation::Commit(_) => self.plan(offset, end, true)?,
            Mutation::Decommit => self.plan(offset, end, false)?,
            Mutation::Protect(_) => {
                let guest_pages = offset / GUEST_PAGE_SIZE..end / GUEST_PAGE_SIZE;
                if self.committed[guest_pages.clone()].iter().any(|&p| !p) {
                    return Err("direct protection requires committed guest pages".into());
                }
                PagePlan {
                    guest_pages,
                    host_spans: Vec::new(),
                    clear_spans: Vec::new(),
                    protection_changes: Vec::new(),
                }
            }
        };
        for host in offset / self.host_page_size..=(end - 1) / self.host_page_size {
            let pages = self.host_guest_pages(host);
            let mut after = NativeProtection::ReadWrite;
            // The rounded tail is not guest memory and must remain inaccessible.
            if pages.len() != self.host_page_size / GUEST_PAGE_SIZE {
                after = NativeProtection::None;
            }
            for page in pages {
                let protection = if plan.guest_pages.contains(&page) {
                    match mutation {
                        Mutation::Commit(p) | Mutation::Protect(p) => p,
                        Mutation::Decommit => GuestPageProtection::NONE,
                    }
                } else if self.committed[page] {
                    direct.guest[page]
                } else {
                    GuestPageProtection::NONE
                };
                after = after.intersect(NativeProtection::for_guest(protection));
            }
            let before = direct.host[host];
            if before != after {
                let start = host * self.host_page_size;
                let end = start + self.host_page_size;
                if let Some(last) = plan.protection_changes.last_mut()
                    && last.span.end == start
                    && last.before == before
                    && last.after == after
                {
                    last.span.end = end;
                } else {
                    plan.protection_changes.push(HostChange {
                        span: start..end,
                        before,
                        after,
                    });
                }
            }
        }
        Ok(plan)
    }

    fn apply_direct(&mut self, plan: &PagePlan, mutation: Mutation) {
        let direct = self.direct.as_mut().unwrap();
        for page in plan.guest_pages.clone() {
            match mutation {
                Mutation::Commit(p) => {
                    self.committed[page] = true;
                    direct.guest[page] = p;
                }
                Mutation::Protect(p) => direct.guest[page] = p,
                Mutation::Decommit => {
                    self.committed[page] = false;
                    direct.guest[page] = GuestPageProtection::NONE;
                }
            }
        }
        for change in &plan.protection_changes {
            direct.host
                [change.span.start / self.host_page_size..change.span.end / self.host_page_size]
                .fill(change.after);
        }
    }
}

#[cfg(unix)]
use super::Reservation;
#[cfg(unix)]
use std::cell::Ref;
#[cfg(unix)]
use std::ptr::NonNull;

#[cfg(unix)]
pub(super) struct SharedAlias {
    pointer: NonNull<u8>,
    len: usize,
    #[cfg(test)]
    fail_after: std::cell::Cell<Option<usize>>,
}

#[cfg(unix)]
impl SharedAlias {
    fn discard(&self, span: &Range<usize>) {
        // Only completely empty host pages reach this hint. On Linux/Android
        // MADV_REMOVE releases shared backing too, so the private alias cannot
        // keep decommitted memfd pages resident indefinitely. Recommit clears
        // explicitly, independently of whether the advisory request succeeds.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let advice = libc::MADV_REMOVE;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let advice = libc::MADV_DONTNEED;
        unsafe {
            libc::madvise(
                self.pointer.as_ptr().add(span.start).cast(),
                span.len(),
                advice,
            )
        };
    }

    fn create_backing() -> Result<std::os::fd::OwnedFd, String> {
        use std::os::fd::{FromRawFd, OwnedFd};
        // Match the existing PCM backend's API 29 contract: Android exposes
        // memfd_create in libc only starting at API 30; its syscall is older.
        #[cfg(target_os = "android")]
        let fd = unsafe {
            libc::syscall(
                libc::SYS_memfd_create,
                c"adv-cpu-direct".as_ptr(),
                libc::MFD_CLOEXEC,
            ) as libc::c_int
        };
        #[cfg(target_os = "linux")]
        let fd = unsafe { libc::memfd_create(c"adv-cpu-direct".as_ptr(), libc::MFD_CLOEXEC) };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            if fd < 0 {
                return Err(format!(
                    "create direct guest backing: {}",
                    std::io::Error::last_os_error()
                ));
            }
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            for _ in 0..128 {
                // Darwin has a short SHM_NAME_MAX. Hex names remain within it
                // even at the maximum process id and counter value.
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let name =
                    std::ffi::CString::new(format!("/ad-{:x}-{id:x}", std::process::id())).unwrap();
                // POSIX shm_open sets close-on-exec on the returned descriptor.
                let fd = unsafe {
                    libc::shm_open(
                        name.as_ptr(),
                        libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
                        0o600,
                    )
                };
                if fd < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::EEXIST) {
                        continue;
                    }
                    return Err(format!("create direct guest backing: {error}"));
                }
                let owned = unsafe { OwnedFd::from_raw_fd(fd) };
                loop {
                    if unsafe { libc::shm_unlink(name.as_ptr()) } == 0 {
                        break;
                    }
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EINTR) {
                        return Err(format!("unlink direct guest backing: {error}"));
                    }
                }
                return Ok(owned);
            }
            Err("create direct guest backing: exhausted exclusive names".into())
        }
    }

    /// Maps the window view at `place` when given, replacing what is there.
    fn new(len: usize, place: Option<NonNull<u8>>) -> Result<(NonNull<u8>, Self), String> {
        use std::os::fd::AsRawFd;
        let owned = Self::create_backing()?;
        let fd = owned.as_raw_fd();
        let size =
            libc::off_t::try_from(len).map_err(|_| "direct guest backing length overflow")?;
        if unsafe { libc::ftruncate(fd, size) } != 0 {
            return Err(format!(
                "size direct guest backing: {}",
                std::io::Error::last_os_error()
            ));
        }
        let map = |address: *mut u8, protection, fixed| unsafe {
            libc::mmap(
                address.cast(),
                len,
                protection,
                libc::MAP_SHARED | fixed,
                fd,
                0,
            )
        };
        let window = match place {
            Some(place) => map(place.as_ptr(), libc::PROT_NONE, libc::MAP_FIXED),
            None => map(std::ptr::null_mut(), libc::PROT_NONE, 0),
        };
        if window == libc::MAP_FAILED {
            return Err(format!(
                "map direct guest window: {}",
                std::io::Error::last_os_error()
            ));
        }
        let alias = map(std::ptr::null_mut(), libc::PROT_READ | libc::PROT_WRITE, 0);
        if alias == libc::MAP_FAILED {
            let error = std::io::Error::last_os_error();
            unsafe {
                match place {
                    Some(place) => super::window::vacate(place.as_ptr(), len),
                    None => {
                        libc::munmap(window, len);
                    }
                }
            }
            return Err(format!("map checked guest alias: {error}"));
        }
        drop(owned);
        Ok((
            NonNull::new(window.cast()).unwrap(),
            Self {
                pointer: NonNull::new(alias.cast()).unwrap(),
                len,
                #[cfg(test)]
                fail_after: std::cell::Cell::new(None),
            },
        ))
    }

    fn protect(
        &self,
        window: NonNull<u8>,
        change: &HostChange,
        protection: NativeProtection,
    ) -> bool {
        let pointer = unsafe { window.as_ptr().add(change.span.start) };
        #[cfg(test)]
        if let Some(remaining) = self.fail_after.get() {
            if remaining == 0 {
                self.fail_after.set(None);
                // Exercise a real failing syscall, then let rollback use the
                // actual aligned mapping. No guest bytes have been changed yet.
                return unsafe {
                    libc::mprotect(pointer.add(1).cast(), change.span.len(), protection.flags())
                        == 0
                };
            }
            self.fail_after.set(Some(remaining - 1));
        }
        unsafe { libc::mprotect(pointer.cast(), change.span.len(), protection.flags()) == 0 }
    }
}

#[cfg(unix)]
impl Drop for SharedAlias {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.pointer.as_ptr().cast(), self.len) };
    }
}

/// Pins guest metadata and both mappings. Acquire outside the signal handler;
/// `classify` itself only reads this frozen lease and does not borrow a RefCell.
/// Mapping/protection mutations are rejected by the live immutable borrow.
#[cfg(unix)]
pub struct DirectFaultView<'a> {
    reservation: &'a Reservation,
    pages: Ref<'a, UnixPages>,
}

#[cfg(unix)]
impl DirectFaultView<'_> {
    pub fn guest_offset(&self, host_address: usize) -> Option<usize> {
        host_address
            .checked_sub(self.reservation.ptr.as_ptr() as usize)
            .filter(|&offset| offset < self.reservation.len)
    }

    /// Allocation-free, lock-free classification for a frozen execution lease.
    /// A guest-legal synchronous fault is directed to the checked path; callers
    /// must still check its native PC/site before treating it as a guest fault.
    pub fn classify(&self, host_address: usize, access: GuestAccess) -> FaultClassification {
        let Some(offset) = self.guest_offset(host_address) else {
            return FaultClassification::OutsideGuest;
        };
        let direct = self.pages.direct.as_ref().unwrap();
        let page = offset / GUEST_PAGE_SIZE;
        if !self.pages.committed[page] || !direct.guest[page].allows(access) {
            return FaultClassification::AccessViolation;
        }
        if access == GuestAccess::Write && direct.guest[page].code_write_protected {
            return FaultClassification::CodeWriteProtection;
        }
        if direct.host[offset / self.pages.host_page_size].allows(access) {
            FaultClassification::Accessible
        } else {
            FaultClassification::MixedPage
        }
    }

    /// Whether an entire operand is accessible in the protected guest window.
    pub fn window_allows(&self, offset: usize, len: usize, access: GuestAccess) -> bool {
        let Ok(range) = self.validate(offset, len, access) else {
            return false;
        };
        if range.is_empty() {
            return true;
        }
        let direct = self.pages.direct.as_ref().unwrap();
        direct.host
            [range.start / self.pages.host_page_size..=(range.end - 1) / self.pages.host_page_size]
            .iter()
            .all(|&p| p.allows(access))
    }

    fn validate(
        &self,
        offset: usize,
        len: usize,
        access: GuestAccess,
    ) -> Result<Range<usize>, String> {
        let end = offset
            .checked_add(len)
            .filter(|&end| end <= self.reservation.len)
            .ok_or("checked direct access outside guest window")?;
        let range = offset..end;
        if range.is_empty() {
            return Ok(range);
        }
        let direct = self.pages.direct.as_ref().unwrap();
        for page in offset / GUEST_PAGE_SIZE..=(end - 1) / GUEST_PAGE_SIZE {
            if !self.pages.committed[page] || !direct.guest[page].allows(access) {
                return Err("direct guest access violation".into());
            }
            if access == GuestAccess::Write && direct.guest[page].code_write_protected {
                return Err("direct code write requires invalidation".into());
            }
        }
        Ok(range)
    }

    pub fn checked_read(&self, offset: usize, out: &mut [u8]) -> Result<(), String> {
        self.read_access(offset, out, GuestAccess::Read)
    }
    pub fn checked_fetch(&self, offset: usize, out: &mut [u8]) -> Result<(), String> {
        self.read_access(offset, out, GuestAccess::Execute)
    }

    fn read_access(
        &self,
        offset: usize,
        out: &mut [u8],
        access: GuestAccess,
    ) -> Result<(), String> {
        self.validate(offset, out.len(), access)?;
        if !out.is_empty() {
            let alias = self.reservation.direct_alias.as_ref().unwrap();
            // Only owned private storage is supported; this frozen lease keeps
            // the alias accessible across checking and copying, including NONE.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    alias.pointer.as_ptr().add(offset),
                    out.as_mut_ptr(),
                    out.len(),
                )
            };
        }
        Ok(())
    }

    pub fn checked_write(&self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        self.validate(offset, bytes.len(), GuestAccess::Write)?;
        if !bytes.is_empty() {
            let alias = self.reservation.direct_alias.as_ref().unwrap();
            // Validate every subpage before the first store: denied cross-page
            // writes cannot leave a committed prefix in private guest memory.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    alias.pointer.as_ptr().add(offset),
                    bytes.len(),
                )
            };
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Reservation {
    /// Explicit opt-in; existing Reservation::new and Memory constructors keep
    /// software permissions and their existing RW mapping behavior unchanged.
    pub fn new_direct(len: usize) -> Result<Self, String> {
        Self::new_direct_at_host_size(len, super::os::page_size()?, None)
    }

    /// A direct reservation whose guest view replaces `len` bytes at
    /// `offset` past the base of `window`, which must outlive it.
    pub fn new_direct_in(
        window: &super::GuestWindow,
        offset: usize,
        len: usize,
    ) -> Result<Self, String> {
        Self::new_direct_in_at_host_size(window, offset, len, super::os::page_size()?)
    }

    /// `new_direct_in` as on a host with `host_size` pages, a multiple of
    /// the native size; tests use it to run 16K placement on 4K hosts.
    pub(crate) fn new_direct_in_at_host_size(
        window: &super::GuestWindow,
        offset: usize,
        len: usize,
        host_size: usize,
    ) -> Result<Self, String> {
        if host_size == 0 || !host_size.is_multiple_of(super::os::page_size()?) {
            return Err("host size must be a native-page multiple".into());
        }
        if !offset.is_multiple_of(host_size) {
            return Err(format!(
                "guest window offset {offset:#x} is not host page aligned"
            ));
        }
        let place = window.place(offset, len.next_multiple_of(host_size))?;
        Self::new_direct_at_host_size(len, host_size, Some(place))
    }

    fn new_direct_at_host_size(
        len: usize,
        host_size: usize,
        place: Option<NonNull<u8>>,
    ) -> Result<Self, String> {
        if len == 0 || !len.is_multiple_of(GUEST_PAGE_SIZE) {
            return Err("reservation must be page aligned and nonempty".into());
        }
        let mut pages = UnixPages::new(len, host_size)?;
        pages.enable_direct();
        let (ptr, alias) = SharedAlias::new(pages.mapped_len, place)?;
        Ok(Self {
            ptr,
            len,
            pages: std::cell::RefCell::new(pages),
            direct_alias: Some(alias),
            placed: place.is_some(),
        })
    }

    /// The always-readable and writable view of committed guest bytes,
    /// whatever the guest view's protection.
    pub fn alias(&self) -> Option<NonNull<u8>> {
        self.direct_alias.as_ref().map(|alias| alias.pointer)
    }

    #[cfg(test)]
    pub fn new_direct_with_host_page_size_for_test(
        len: usize,
        host_size: usize,
    ) -> Result<Self, String> {
        if host_size == 0 || !host_size.is_multiple_of(super::os::page_size()?) {
            return Err("simulated host size must be a native-page multiple".into());
        }
        Self::new_direct_at_host_size(len, host_size, None)
    }

    /// Acquire only outside asynchronous handlers. The returned lease blocks
    /// commit, protection changes, and release until native execution is stopped.
    pub fn direct_fault_view(&self) -> Option<DirectFaultView<'_>> {
        self.direct_alias.as_ref()?;
        Some(DirectFaultView {
            reservation: self,
            pages: self.pages.borrow(),
        })
    }

    pub fn checked_read(&self, offset: usize, out: &mut [u8]) -> Result<(), String> {
        self.direct_fault_view()
            .ok_or("direct protection is not enabled")?
            .checked_read(offset, out)
    }
    pub fn checked_write(&self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        self.direct_fault_view()
            .ok_or("direct protection is not enabled")?
            .checked_write(offset, bytes)
    }

    pub fn commit_direct(
        &self,
        offset: usize,
        len: usize,
        protection: GuestPageProtection,
    ) -> Result<(), String> {
        self.mutate_direct(offset, len, Mutation::Commit(protection))
    }
    pub fn protect_direct(
        &self,
        offset: usize,
        len: usize,
        protection: GuestPageProtection,
    ) -> Result<(), String> {
        self.mutate_direct(offset, len, Mutation::Protect(protection))
    }
    pub fn decommit_direct(&self, offset: usize, len: usize) -> Result<(), String> {
        self.mutate_direct(offset, len, Mutation::Decommit)
    }

    fn mutate_direct(&self, offset: usize, len: usize, mutation: Mutation) -> Result<(), String> {
        let end = self.bounds(offset, len)?;
        let alias = self
            .direct_alias
            .as_ref()
            .ok_or("direct protection is not enabled")?;
        let mut pages = self
            .pages
            .try_borrow_mut()
            .map_err(|_| "direct mapping is pinned by an execution lease")?;
        let plan = pages.plan_direct(offset, end, mutation)?;
        for (index, change) in plan.protection_changes.iter().enumerate() {
            if !alias.protect(self.ptr, change, change.after) {
                let error = std::io::Error::last_os_error();
                // A failed mprotect may have changed a prefix. Restore it too.
                // Losing an owned mapping unexpectedly is an invariant failure;
                // never publish metadata that pretends rollback succeeded.
                for previous in plan.protection_changes[..=index].iter().rev() {
                    if !alias.protect(self.ptr, previous, previous.before) {
                        // Unwinding could let a caller reuse an owner with
                        // inconsistent OS protection and metadata. Fail closed
                        // if the OS cannot restore an owned mapping at all.
                        std::process::abort();
                    }
                }
                return Err(format!("protect direct guest memory: {error}"));
            }
        }
        if matches!(mutation, Mutation::Commit(_)) {
            // Recommit always clears through the stable alias, even when the
            // original guest window is NONE for a mixed or guest-protected page.
            unsafe { std::ptr::write_bytes(alias.pointer.as_ptr().add(offset), 0, len) };
        }
        if matches!(mutation, Mutation::Decommit) {
            // The lifecycle planner limits clearing to partial host pages with
            // live neighbours. Do not instantiate backing for untouched holes
            // when decommitting a sparse reservation spanning the 4 GiB window.
            for span in &plan.clear_spans {
                unsafe {
                    std::ptr::write_bytes(alias.pointer.as_ptr().add(span.start), 0, span.len())
                };
            }
            for span in &plan.host_spans {
                alias.discard(span);
            }
        }
        pages.apply_direct(&plan, mutation);
        Ok(())
    }
}

#[cfg(all(unix, test))]
mod tests {
    use super::*;
    const GUEST: usize = GUEST_PAGE_SIZE;

    fn reservation(host: usize, len: usize) -> Reservation {
        Reservation::new_direct_with_host_page_size_for_test(len, host).unwrap()
    }

    fn fill(r: &Reservation, offset: usize, len: usize, byte: u8) {
        r.checked_write(offset, &vec![byte; len]).unwrap();
    }

    fn expect(r: &Reservation, offset: usize, len: usize, byte: u8) {
        let mut bytes = vec![0; len];
        r.checked_read(offset, &mut bytes).unwrap();
        assert!(bytes.iter().all(|&b| b == byte));
    }

    #[test]
    fn direct_4k_and_16k_lifecycle_preserves_neighbours() {
        for host in [GUEST, 4 * GUEST]
            .into_iter()
            .filter(|host| host.is_multiple_of(super::super::os::page_size().unwrap()))
        {
            let r = reservation(host, 2 * host + GUEST);
            r.commit_direct(0, r.len, GuestPageProtection::READ_WRITE)
                .unwrap();
            for page in 0..r.len / GUEST {
                fill(&r, page * GUEST, GUEST, page as u8 + 1);
            }
            r.protect_direct(0, GUEST, GuestPageProtection::READ)
                .unwrap();
            r.decommit(GUEST, GUEST).unwrap();
            assert!(r.checked_read(GUEST, &mut [0]).is_err());
            assert!(r.checked_write(0, &[9]).is_err());
            expect(&r, 0, GUEST, 1);
            expect(&r, 2 * GUEST, GUEST, 3);
            let view = r.direct_fault_view().unwrap();
            assert_eq!(
                view.classify(r.ptr.as_ptr() as usize, GuestAccess::Write),
                FaultClassification::AccessViolation
            );
            assert_eq!(
                view.classify(r.ptr.as_ptr() as usize + GUEST, GuestAccess::Read),
                FaultClassification::AccessViolation
            );
            assert!(!view.window_allows(0, GUEST, GuestAccess::Write));
            drop(view);
            r.commit_direct(GUEST, GUEST, GuestPageProtection::READ_WRITE)
                .unwrap();
            expect(&r, GUEST, GUEST, 0);
            expect(&r, 0, GUEST, 1);
            assert!(r.checked_write(0, &[9]).is_err());
            fill(&r, GUEST, GUEST, 0x66);
            r.decommit(GUEST, GUEST).unwrap();
            r.commit_direct(GUEST, GUEST, GuestPageProtection::READ)
                .unwrap();
            expect(&r, GUEST, GUEST, 0);
            assert!(r.checked_write(GUEST, &[9]).is_err());
            expect(&r, 2 * GUEST, GUEST, 3);
            r.decommit(0, r.len).unwrap();
            r.commit_direct(0, r.len, GuestPageProtection::READ_WRITE)
                .unwrap();
            expect(&r, 0, r.len, 0);
            // Rounded non-guest tail cannot grant extra accessible addresses.
            assert_eq!(
                r.direct_fault_view()
                    .unwrap()
                    .classify(r.ptr.as_ptr() as usize + r.len, GuestAccess::Read),
                FaultClassification::OutsideGuest
            );
        }
    }

    #[test]
    fn mixed_checked_alias_never_retries_inaccessible_window() {
        let host = 4 * GUEST;
        let r = reservation(host, host);
        r.commit_direct(0, GUEST, GuestPageProtection::READ_WRITE)
            .unwrap();
        fill(&r, 0, GUEST, 0x5a);
        let view = r.direct_fault_view().unwrap();
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize, GuestAccess::Read),
            FaultClassification::MixedPage
        );
        assert!(!view.window_allows(0, 1, GuestAccess::Read));
        assert_eq!(
            view.classify(
                r.direct_alias.as_ref().unwrap().pointer.as_ptr() as usize,
                GuestAccess::Read
            ),
            FaultClassification::OutsideGuest
        );
        view.checked_write(0, &[0x7b]).unwrap();
        let mut value = [0];
        view.checked_read(0, &mut value).unwrap();
        assert_eq!(value, [0x7b]);
        assert!(!view.window_allows(0, 1, GuestAccess::Read));
        // Even metadata-only changes are rejected while native execution is pinned.
        assert!(
            r.protect_direct(0, GUEST, GuestPageProtection::READ)
                .is_err()
        );
        assert!(r.decommit(0, GUEST).is_err());
        assert!(r.commit(GUEST, GUEST).is_err());
        drop(view);
        r.commit_direct(GUEST, 3 * GUEST, GuestPageProtection::READ_WRITE)
            .unwrap();
        assert!(
            r.direct_fault_view()
                .unwrap()
                .window_allows(0, host, GuestAccess::Write)
        );
        r.protect_direct(GUEST, GUEST, GuestPageProtection::READ)
            .unwrap();
        let view = r.direct_fault_view().unwrap();
        assert!(view.window_allows(0, host, GuestAccess::Read));
        assert!(!view.window_allows(0, GUEST, GuestAccess::Write));
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize, GuestAccess::Write),
            FaultClassification::MixedPage
        );
        view.checked_write(0, &[0x33]).unwrap();
        assert!(view.checked_write(GUEST - 1, &[0x44, 0x55]).is_err());
        let mut value = [0];
        view.checked_read(GUEST - 1, &mut value).unwrap();
        assert_eq!(value, [0x5a]);
    }

    #[test]
    fn code_barrier_and_unrepresentable_guest_permissions_are_explicit() {
        let host = 4 * GUEST;
        let r = reservation(host, host);
        r.commit(0, host).unwrap();
        fill(&r, 0, host, 0x12);
        r.protect_direct(
            0,
            GUEST,
            GuestPageProtection::new(READ | WRITE | EXEC, true).unwrap(),
        )
        .unwrap();
        let view = r.direct_fault_view().unwrap();
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize, GuestAccess::Write),
            FaultClassification::CodeWriteProtection
        );
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize + GUEST, GuestAccess::Write),
            FaultClassification::MixedPage
        );
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize, GuestAccess::Read),
            FaultClassification::Accessible
        );
        assert!(view.checked_write(0, &[9]).is_err());
        view.checked_write(GUEST, &[9]).unwrap();
        let mut value = [0];
        view.checked_fetch(0, &mut value).unwrap();
        assert_eq!(value, [0x12]);
        drop(view);
        // Clearing the barrier is an explicit caller-side invalidation decision.
        r.protect_direct(0, GUEST, GuestPageProtection::READ_WRITE)
            .unwrap();
        r.checked_write(0, &[9]).unwrap();
        r.protect_direct(0, GUEST, GuestPageProtection::new(WRITE, false).unwrap())
            .unwrap();
        let view = r.direct_fault_view().unwrap();
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize, GuestAccess::Read),
            FaultClassification::AccessViolation
        );
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize, GuestAccess::Write),
            FaultClassification::MixedPage
        );
        assert!(view.checked_read(0, &mut value).is_err());
        view.checked_write(0, &[7]).unwrap();
        assert!(GuestPageProtection::new(0x80, false).is_err());
    }

    #[test]
    fn real_mprotect_failures_roll_back_mapping_metadata_and_bytes() {
        for host in [GUEST, 4 * GUEST]
            .into_iter()
            .filter(|host| host.is_multiple_of(super::super::os::page_size().unwrap()))
        {
            let r = reservation(host, 3 * host);
            let alias = r.direct_alias.as_ref().unwrap();
            alias.fail_after.set(Some(0));
            assert!(
                r.commit_direct(0, r.len, GuestPageProtection::READ_WRITE)
                    .is_err()
            );
            assert!(r.pages.borrow().committed.iter().all(|&p| !p));
            assert!(r.checked_read(0, &mut [0]).is_err());
            r.commit(0, r.len).unwrap();
            fill(&r, 0, r.len, 0xa5);
            r.protect_direct(host, host, GuestPageProtection::READ)
                .unwrap();
            for decommit in [false, true] {
                alias.fail_after.set(Some(1));
                let result = if decommit {
                    r.decommit(0, r.len)
                } else {
                    r.protect_direct(0, r.len, GuestPageProtection::READ)
                };
                assert!(result.is_err());
                assert!(r.pages.borrow().committed.iter().all(|&p| p));
                expect(&r, 0, r.len, 0xa5);
                let view = r.direct_fault_view().unwrap();
                assert!(view.window_allows(0, host, GuestAccess::Write));
                assert!(view.window_allows(2 * host, host, GuestAccess::Write));
                assert!(!view.window_allows(host, host, GuestAccess::Write));
                assert_eq!(unsafe { std::ptr::read_volatile(r.ptr.as_ptr()) }, 0xa5);
                unsafe { std::ptr::write_volatile(r.ptr.as_ptr().add(2 * host), 0xa5) };
            }
            // A failed operation cannot weaken the middle page's protection.
            assert!(r.checked_write(host, &[1]).is_err());
            assert!(
                r.commit_direct(0, host, GuestPageProtection::READ_WRITE)
                    .is_err()
            );
            assert!(
                r.protect_direct(r.len, GUEST, GuestPageProtection::READ)
                    .is_err()
            );
        }
    }

    #[test]
    fn commit_failure_after_partial_host_transition_preserves_live_boundaries() {
        let host = 4 * GUEST;
        let r = reservation(host, 3 * host);
        for (offset, byte) in [(0, 0x11), (r.len - GUEST, 0x22)] {
            r.commit(offset, GUEST).unwrap();
            fill(&r, offset, GUEST, byte);
            r.protect_direct(offset, GUEST, GuestPageProtection::READ)
                .unwrap();
        }
        r.direct_alias.as_ref().unwrap().fail_after.set(Some(1));
        assert!(
            r.commit_direct(GUEST, r.len - 2 * GUEST, GuestPageProtection::READ_WRITE)
                .is_err()
        );
        expect(&r, 0, GUEST, 0x11);
        expect(&r, r.len - GUEST, GUEST, 0x22);
        assert!(r.checked_read(GUEST, &mut [0]).is_err());
        let view = r.direct_fault_view().unwrap();
        assert_eq!(
            view.classify(r.ptr.as_ptr() as usize, GuestAccess::Read),
            FaultClassification::MixedPage
        );
        assert!(
            view.pages
                .direct
                .as_ref()
                .unwrap()
                .host
                .iter()
                .all(|&p| p == NativeProtection::None)
        );
        drop(view);
        r.commit_direct(GUEST, r.len - 2 * GUEST, GuestPageProtection::READ)
            .unwrap();
        expect(&r, GUEST, r.len - 2 * GUEST, 0);
        expect(&r, 0, GUEST, 0x11);
        assert!(
            r.direct_fault_view()
                .unwrap()
                .window_allows(0, r.len, GuestAccess::Read)
        );
    }

    #[test]
    fn frozen_view_checks_entire_operand_and_default_backend_stays_rw() {
        let host = super::super::os::page_size().unwrap();
        let r = Reservation::new_direct(host).unwrap();
        r.commit(0, host).unwrap();
        let view = r.direct_fault_view().unwrap();
        assert!(view.checked_read(usize::MAX, &mut [0]).is_err());
        assert!(view.checked_write(host - 1, &[1, 2]).is_err());
        assert!(view.checked_read(host, &mut []).is_ok());
        assert!(view.checked_read(host + 1, &mut []).is_err());
        assert_eq!(
            view.classify(
                (r.ptr.as_ptr() as usize).saturating_sub(1),
                GuestAccess::Read
            ),
            FaultClassification::OutsideGuest
        );
        let default = Reservation::new(host).unwrap();
        assert!(default.direct_fault_view().is_none());
        default.commit(0, host).unwrap();
        unsafe { std::ptr::write_volatile(default.ptr.as_ptr(), 0x55) };
        assert_eq!(
            unsafe { std::ptr::read_volatile(default.ptr.as_ptr()) },
            0x55
        );
        assert!(
            default
                .protect_direct(0, host, GuestPageProtection::NONE)
                .is_err()
        );
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    fn protected_window_faults_only_in_child_process() {
        use std::os::unix::process::ExitStatusExt;
        for scenario in [
            "readonly-write",
            "none-read",
            "decommit-read",
            "mixed-read",
            "rollback-readonly-write",
        ] {
            let executable = std::env::current_exe().unwrap();
            #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
            let mut child = if let Some(runner) =
                std::env::var_os("CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER")
            {
                // A user-mode emulator cannot exec a target ELF through the
                // host kernel without its configured runner or binfmt support.
                let mut command = std::process::Command::new(runner);
                command.arg(&executable);
                command
            } else {
                std::process::Command::new(executable)
            };
            #[cfg(not(all(target_arch = "aarch64", target_os = "linux")))]
            let mut child = std::process::Command::new(executable);
            let output = child
                .args([
                    "--exact",
                    "virtual_memory::direct::tests::fault_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("ADV_CPU_DIRECT_PROBE", scenario)
                .output()
                .unwrap();
            assert!(
                output.status.signal() == Some(libc::SIGSEGV)
                    || (cfg!(target_os = "macos") && output.status.signal() == Some(libc::SIGBUS)),
                "probe {scenario} ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    #[test]
    #[ignore = "expected SIGSEGV; executed only by protected_window_faults_only_in_child_process"]
    fn fault_child() {
        let scenario = std::env::var("ADV_CPU_DIRECT_PROBE").expect("run through parent probe");
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
        let host = if scenario == "mixed-read" {
            4 * GUEST
        } else {
            super::super::os::page_size().unwrap()
        };
        let r = reservation(host, 3 * host);
        if scenario == "mixed-read" {
            r.commit(0, GUEST).unwrap();
            r.checked_write(0, &[0x5a]).unwrap();
            expect(&r, 0, 1, 0x5a);
        } else {
            r.commit(0, r.len).unwrap();
            fill(&r, 0, r.len, 0x5a);
        }
        let at = if scenario == "rollback-readonly-write" {
            r.protect_direct(host, host, GuestPageProtection::READ)
                .unwrap();
            r.direct_alias.as_ref().unwrap().fail_after.set(Some(1));
            assert!(r.decommit(0, r.len).is_err());
            host
        } else {
            0
        };
        match scenario.as_str() {
            "readonly-write" => r
                .protect_direct(0, GUEST, GuestPageProtection::READ)
                .unwrap(),
            "none-read" => r
                .protect_direct(0, GUEST, GuestPageProtection::NONE)
                .unwrap(),
            "decommit-read" => r.decommit(0, GUEST).unwrap(),
            _ => {}
        }
        let pointer = unsafe { r.ptr.as_ptr().add(at) };
        if scenario.ends_with("write") {
            unsafe { std::ptr::write_volatile(pointer, 1) };
        } else {
            std::hint::black_box(unsafe { std::ptr::read_volatile(pointer) });
        }
        panic!("protected window access unexpectedly succeeded");
    }
}

#[cfg(test)]
mod planner_tests {
    use super::*;

    #[test]
    fn both_host_sizes_intersect_every_guest_subpage_and_code_barrier() {
        for host in [GUEST_PAGE_SIZE, 4 * GUEST_PAGE_SIZE] {
            let mut pages = UnixPages::new(2 * host, host).unwrap();
            pages.enable_direct();
            let commit = pages
                .plan_direct(
                    0,
                    GUEST_PAGE_SIZE,
                    Mutation::Commit(GuestPageProtection::READ_WRITE),
                )
                .unwrap();
            pages.apply_direct(&commit, Mutation::Commit(GuestPageProtection::READ_WRITE));
            let expected = if host == GUEST_PAGE_SIZE {
                NativeProtection::ReadWrite
            } else {
                NativeProtection::None
            };
            assert_eq!(
                pages.direct.as_ref().unwrap().host,
                [expected, NativeProtection::None]
            );
            if host > GUEST_PAGE_SIZE {
                let rest = pages
                    .plan_direct(
                        GUEST_PAGE_SIZE,
                        host,
                        Mutation::Commit(GuestPageProtection::READ_WRITE),
                    )
                    .unwrap();
                pages.apply_direct(&rest, Mutation::Commit(GuestPageProtection::READ_WRITE));
            }
            let barrier = GuestPageProtection::new(READ | WRITE | EXEC, true).unwrap();
            assert_eq!(barrier.permissions(), READ | WRITE | EXEC);
            assert!(barrier.code_write_protected());
            let protection = pages
                .plan_direct(0, GUEST_PAGE_SIZE, Mutation::Protect(barrier))
                .unwrap();
            pages.apply_direct(&protection, Mutation::Protect(barrier));
            assert_eq!(
                pages.direct.as_ref().unwrap().host,
                [NativeProtection::Read, NativeProtection::None]
            );
            let release = pages
                .plan_direct(0, GUEST_PAGE_SIZE, Mutation::Decommit)
                .unwrap();
            pages.apply_direct(&release, Mutation::Decommit);
            assert_eq!(
                pages.direct.as_ref().unwrap().host,
                [NativeProtection::None, NativeProtection::None]
            );
            assert!(
                pages
                    .plan_direct(0, host, Mutation::Protect(GuestPageProtection::READ))
                    .is_err()
            );
            assert_eq!(
                pages.direct.as_ref().unwrap().host,
                [NativeProtection::None, NativeProtection::None]
            );
        }
    }
}
