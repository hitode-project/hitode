use crate::shared_memory::SharedAllocation;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

mod fast;
mod locks;

pub use fast::FastPermissions;

pub const PAGE_SIZE: usize = 4096;

/// Each window counts epochs in a private range, so a version stamped in one
/// window never matches another, even one at a reused host address.
fn epoch_origin() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed) << EPOCH_BITS
}
const EPOCH_BITS: u32 = 40;
/// Changed byte ranges kept between refreshes; more compare whole code.
pub const DIRTY_RANGES: usize = 64;

/// Split a window range into (page, in-page byte range) pieces.
fn page_spans(
    range: std::ops::Range<usize>,
) -> impl Iterator<Item = (usize, std::ops::Range<usize>)> {
    let mut at = range.start;
    std::iter::from_fn(move || {
        (at < range.end).then(|| {
            let page = at / PAGE_SIZE;
            let stop = range.end.min((page + 1) * PAGE_SIZE);
            let span = (page, at % PAGE_SIZE..stop - page * PAGE_SIZE);
            at = stop;
            span
        })
    })
}

/// The (word, mask) pairs covering a byte range of a page's code bitmap.
fn bit_words(bytes: std::ops::Range<usize>) -> impl Iterator<Item = (usize, u64)> {
    (bytes.start / 64..bytes.end.div_ceil(64)).map(move |word| {
        let low = bytes.start.max(word * 64) - word * 64;
        let high = bytes.end.min((word + 1) * 64) - word * 64;
        (word, (u64::MAX >> (64 - (high - low))) << low)
    })
}
pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const EXEC: u8 = 4;
/// Internal execution barrier; never exposed as a guest protection flag.
pub const SHARED: u8 = 8;
/// A shared PCM page has a stable alias at base + guest offset. Access must
/// still use only matching byte atomics, even for guest word/vector operations.
pub const AUDIO: u8 = 16;
/// Derived grants in immutable native metadata, never guest protection flags.
pub const FAST_READ: u8 = READ << 5;
pub const FAST_WRITE: u8 = WRITE << 5;

enum SharedPage {
    Audio {
        storage: Arc<SharedAllocation>,
        offset: usize,
    },
    File {
        page: std::rc::Rc<crate::backend::FilePage>,
        copy_on_write: bool,
    },
}

impl SharedPage {
    fn read_into(&self, offset: usize, out: &mut [u8]) -> Result<(), String> {
        match self {
            Self::Audio {
                storage,
                offset: base,
            } => {
                crate::backend::SharedReadView::copy_into(storage.as_ref(), base + offset, out);
            }
            Self::File { page, .. } => return page.read_into(offset, out),
        }
        Ok(())
    }

    fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::Audio {
                storage,
                offset: base,
            } => {
                storage.store_bytes(*base + offset, bytes);
            }
            Self::File {
                page,
                copy_on_write,
            } => {
                debug_assert!(
                    !*copy_on_write,
                    "COW pages must be prepared before committing a write"
                );
                page.write(offset, bytes)?;
            }
        }
        Ok(())
    }

    fn file_page(&self) -> bool {
        match self {
            Self::Audio { .. } => false,
            Self::File { .. } => true,
        }
    }

    fn identity(&self) -> (u8, usize, usize) {
        match self {
            Self::Audio { storage, offset } => (0, Arc::as_ptr(storage) as usize, *offset),
            Self::File { page, .. } => (1, std::rc::Rc::as_ptr(page) as usize, 0),
        }
    }
}

/// A fixed, page-aligned guest window. Both executors borrow this same allocation.
pub struct Memory {
    pub base: u32,
    pub len: usize,
    pub ptr: NonNull<u8>,
    pub perms: Arc<Vec<u8>>,
    pub fast: FastPermissions,
    pub epoch: u64,
    /// Allocation identity survives moves and cannot alias a reused host address.
    pub versions: Vec<u64>,
    /// Monotonic code observations owned by the controller, not guest protection.
    watched: Vec<bool>,
    /// Bytes of each watched page that were fetched or captured as code.
    code: BTreeMap<usize, Box<[u64; PAGE_SIZE / 64]>>,
    pub mapped: Arc<Vec<bool>>,
    /// Pages changed since the controller last looked, each with its version
    /// before the first change, and the changed bytes while they stay few.
    pub dirty_pages: BTreeMap<u32, u64>,
    pub dirty_ranges: Option<Vec<(u32, usize)>>,
    reservation: Option<crate::virtual_memory::Reservation>,
    shared_pages: BTreeMap<usize, SharedPage>,
    /// Owned snapshots have no external alias. SHARED is only their first-store
    /// barrier, retired by the same transactional store preparation as COPY.
    private_copy_pages: BTreeMap<usize, bool>,
    /// File pages whose bytes live in this window's committed pages.
    resident: BTreeMap<usize, std::rc::Rc<crate::backend::FilePage>>,
    /// Real host pins owned by this window; repeated locks do not add a count.
    locked_pages: BTreeSet<usize>,
    #[cfg(windows)]
    native_protection: bool,
    #[cfg(windows)]
    native_read_exceptions: usize,
    /// The window `reservation` is placed in; declared after it so the
    /// reservation hands its range back first.
    #[cfg(unix)]
    window: Option<crate::virtual_memory::GuestWindow>,
}

impl Memory {
    /// Only software-checked memory distinguishes RWX data from code.
    /// Native host protection retains the full EXEC barrier.
    fn selective_code_writes(&self) -> bool {
        #[cfg(windows)]
        if self.native_protection {
            return false;
        }
        true
    }

    pub fn native_permissions(&mut self) -> Arc<Vec<u8>> {
        let watched = self
            .selective_code_writes()
            .then_some(self.watched.as_slice());
        // Where a host page holds several guest pages, the window protects
        // each with the intersection of their permissions (A64-MEM-4).
        #[cfg(unix)]
        if self.window.is_some() {
            let slack = self.window_slack();
            let view = self.reservation.as_ref().unwrap().direct_fault_view();
            if let Some(view) = view.filter(|view| view.host_page_size() > PAGE_SIZE) {
                use crate::virtual_memory::GuestAccess;
                let allows = |page: usize| {
                    let offset = slack + page * PAGE_SIZE;
                    u8::from(view.host_allows(offset, GuestAccess::Read)) * READ
                        | u8::from(view.host_allows(offset, GuestAccess::Write)) * WRITE
                };
                let span = view.host_page_size() / PAGE_SIZE;
                return self.fast.snapshot(
                    &self.perms,
                    watched,
                    &self.private_copy_pages,
                    Some((span, &allows)),
                );
            }
        }
        self.fast
            .snapshot(&self.perms, watched, &self.private_copy_pages, None)
    }

    /// Register sampled code bytes before reading or reusing them. This is
    /// monotonic, including negative tail dependencies and remappings. A
    /// page's first observation makes it a store barrier: the epoch revokes
    /// grants at a paused trap without changing byte stamps.
    pub fn watch_code(&mut self, address: u32, len: usize) {
        let begin = u64::from(address).max(u64::from(self.base));
        let end = (u64::from(address).saturating_add(len as u64))
            .min(u64::from(self.base) + self.len as u64);
        if begin >= end {
            return;
        }
        let (begin, end) = (
            (begin - u64::from(self.base)) as usize,
            (end - u64::from(self.base)) as usize,
        );
        let mut fresh = None;
        for (page, bytes) in page_spans(begin..end) {
            let bits = self
                .code
                .entry(page)
                .or_insert_with(|| Box::new([0; PAGE_SIZE / 64]));
            for (word, mask) in bit_words(bytes) {
                bits[word] |= mask;
            }
            if !self.watched[page] {
                self.watched[page] = true;
                fresh = Some(fresh.map_or(page..page + 1, |old: std::ops::Range<usize>| {
                    old.start..page + 1
                }));
            }
        }
        if let Some(pages) = fresh {
            // Translated code must not store to observed code directly.
            self.sync_window(pages.clone())
                .expect("write-protect observed code in the guest window");
            if self.selective_code_writes() {
                self.fast.invalidate(pages);
                self.epoch += 1;
            }
        }
    }

    /// Software-checked memory changes code only when a store reaches bytes
    /// observed as code; other data on a code page is ordinary data.
    fn writes_code(&self, range: &std::ops::Range<usize>) -> bool {
        if range.is_empty() {
            return false;
        }
        if !self.selective_code_writes() {
            return (range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE)
                .any(|page| self.perms[page] & EXEC != 0);
        }
        page_spans(range.clone()).any(|(page, bytes)| {
            self.perms[page] & EXEC != 0
                && self
                    .code
                    .get(&page)
                    .is_some_and(|bits| bit_words(bytes).any(|(word, mask)| bits[word] & mask != 0))
        })
    }

    /// Whether a store of `len` bytes at `addr` would change observed code.
    pub fn stores_code(&self, addr: u32, len: usize) -> bool {
        self.range(addr, len)
            .is_ok_and(|range| self.writes_code(&range))
    }

    /// Search the existing page table in one bounded metadata slice pass. This
    /// does not issue a guest access/protection query for every empty page.
    pub fn next_mapped_page(&self, addr: u32, limit: u64) -> Option<u32> {
        let begin = u64::from(addr).max(u64::from(self.base));
        let end = limit.min(u64::from(self.base) + self.len as u64);
        if begin >= end {
            return None;
        }
        let first = ((begin - u64::from(self.base)) / PAGE_SIZE as u64) as usize;
        let last = ((end - u64::from(self.base)) / PAGE_SIZE as u64) as usize;
        self.mapped[first..last]
            .iter()
            .position(|&mapped| mapped)
            .map(|offset| self.base + ((first + offset) * PAGE_SIZE) as u32)
    }

    #[cfg(windows)]
    fn native_permission(perms: u8) -> crate::virtual_memory::HostProtection {
        use crate::virtual_memory::HostProtection as P;
        if perms & EXEC != 0 || perms & READ != 0 && perms & WRITE == 0 {
            P::Read
        } else if perms & WRITE != 0 {
            P::ReadWrite
        } else {
            P::NoAccess
        }
    }

    #[cfg(windows)]
    fn native_read_exception(perms: u8) -> bool {
        perms & SHARED == 0 && perms & READ == 0 && perms & (WRITE | EXEC) != 0
    }

    #[cfg(windows)]
    fn host_no_access(&self, range: &std::ops::Range<usize>) -> bool {
        !range.is_empty()
            && self.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE]
                .iter()
                .any(|&p| Self::native_permission(p) as u32 == 1)
    }

    fn host_copy_required(&self, range: &std::ops::Range<usize>) -> bool {
        #[cfg(windows)]
        {
            self.native_protection && self.host_no_access(range)
        }
        #[cfg(not(windows))]
        {
            let _ = range;
            false
        }
    }

    #[cfg(windows)]
    fn host_scope<'a>(
        reservation: Option<&'a crate::virtual_memory::Reservation>,
        perms: &[u8],
        active: bool,
        range: &std::ops::Range<usize>,
        write: bool,
    ) -> Result<Vec<crate::virtual_memory::ProtectionScope<'a>>, String> {
        let mut scopes = Vec::new();
        if !active || range.is_empty() {
            return Ok(scopes);
        }
        let reservation = reservation.ok_or("native protection requires reserved memory")?;
        let desired = if write {
            crate::virtual_memory::HostProtection::ReadWrite
        } else {
            crate::virtual_memory::HostProtection::Read
        };
        let mut at = range.start / PAGE_SIZE;
        let end = (range.end - 1) / PAGE_SIZE + 1;
        while at < end {
            let needs = |page: usize| {
                let p = perms[page];
                (p & SHARED == 0 || p & AUDIO != 0)
                    && (if write {
                        Self::native_permission(p) as u32 != 4
                    } else {
                        Self::native_permission(p) as u32 == 1
                    })
            };
            if !needs(at) {
                at += 1;
                continue;
            }
            let start = at;
            at += 1;
            while at < end && needs(at) {
                at += 1;
            }
            scopes.push(unsafe {
                reservation.temporary_protection(
                    start * PAGE_SIZE,
                    (at - start) * PAGE_SIZE,
                    desired,
                )
            }?);
        }
        Ok(scopes)
    }

    /// Enable host permission enforcement for reserved Windows memory. This is
    /// opt-in until native sites carry complete precise recovery recipes.
    /// Host initialization remains possible through scoped read/write copies.
    pub fn enable_native_protection(&mut self) -> Result<(), String> {
        #[cfg(not(windows))]
        {
            Err("native host protection is not available on this host".into())
        }
        #[cfg(windows)]
        {
            if self.native_protection {
                return Ok(());
            }
            if !self.resident.is_empty() {
                return Err("native protection excludes resident file views".into());
            }
            let reservation = self
                .reservation
                .as_ref()
                .ok_or("native protection requires reserved memory")?;
            let mut scopes = Vec::new();
            let mut at = 0;
            while at < self.perms.len() {
                if !self.mapped[at] || self.perms[at] & SHARED != 0 && self.perms[at] & AUDIO == 0 {
                    at += 1;
                    continue;
                }
                let permission = Self::native_permission(self.perms[at]);
                let start = at;
                at += 1;
                while at < self.perms.len()
                    && self.mapped[at]
                    && (self.perms[at] & SHARED == 0 || self.perms[at] & AUDIO != 0)
                    && Self::native_permission(self.perms[at]) as u32 == permission as u32
                {
                    at += 1;
                }
                scopes.push(unsafe {
                    reservation.temporary_protection(
                        start * PAGE_SIZE,
                        (at - start) * PAGE_SIZE,
                        permission,
                    )
                }?);
            }
            for scope in scopes {
                scope.retain();
            }
            self.native_protection = true;
            // A trap cannot resume with a snapshot that still grants RWX data stores.
            self.epoch += 1;
            Ok(())
        }
    }

    /// Windows cannot express guest data-read denial on write-only or
    /// execute-only pages. Native sites must retain software READ checks while
    /// any ordinary page has that shape. Short address windows keep bounds.
    #[allow(dead_code)] // Used by verified native sites in the next compiler handoff.
    pub fn native_fault_reads_available(&self) -> bool {
        #[cfg(windows)]
        {
            self.native_protection
                && self.native_read_exceptions == 0
                && self.base == 0
                && self.len as u64 == 1 << 32
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    #[cfg(all(windows, target_arch = "x86_64", target_env = "msvc"))]
    pub fn native_fault_window(&self) -> Option<(usize, usize)> {
        self.reservation
            .as_ref()
            .map(|window| (window.ptr.as_ptr() as usize, window.len))
    }

    pub fn new(base: u32, len: usize) -> Result<Self, String> {
        if !(base as usize).is_multiple_of(PAGE_SIZE)
            || len == 0
            || !len.is_multiple_of(PAGE_SIZE)
            || u64::from(base) + len as u64 > 1 << 32
        {
            return Err("memory window must be page aligned and fit in x86-32".into());
        }
        let layout = Layout::from_size_align(len, PAGE_SIZE).map_err(|e| e.to_string())?;
        // SAFETY: nonzero, valid layout; Memory exclusively owns this allocation.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) }).ok_or("guest allocation failed")?;
        let origin = epoch_origin();
        Ok(Self {
            base,
            len,
            ptr,
            perms: Arc::new(vec![0; len / PAGE_SIZE]),
            fast: FastPermissions::new(len / PAGE_SIZE),
            epoch: origin,
            versions: vec![origin; len / PAGE_SIZE],
            watched: vec![false; len / PAGE_SIZE],
            code: BTreeMap::new(),
            mapped: Arc::new(vec![true; len / PAGE_SIZE]),
            dirty_pages: BTreeMap::new(),
            dirty_ranges: Some(Vec::new()),
            reservation: None,
            shared_pages: BTreeMap::new(),
            private_copy_pages: BTreeMap::new(),
            resident: BTreeMap::new(),
            locked_pages: BTreeSet::new(),
            #[cfg(windows)]
            native_protection: false,
            #[cfg(windows)]
            native_read_exceptions: 0,
            #[cfg(unix)]
            window: None,
        })
    }

    pub fn reserve(base: u32, len: usize) -> Result<Self, String> {
        if !base.is_multiple_of(4096)
            || len == 0
            || !len.is_multiple_of(4096)
            || u64::from(base) + len as u64 > 1 << 32
        {
            return Err("invalid reservation".into());
        }
        // A full x86 window retains a final inaccessible tail for unaligned
        // native operands crossing the 4GB limit. Small windows keep guards.
        #[cfg(windows)]
        let reserved_len = if base == 0 && len as u64 == 1 << 32 {
            len + 16384
        } else {
            len
        };
        #[cfg(not(windows))]
        let reserved_len = len;
        let reservation = crate::virtual_memory::Reservation::new(reserved_len)?;
        let origin = epoch_origin();
        Ok(Self {
            base,
            len,
            ptr: reservation.ptr,
            perms: Arc::new(vec![0; len / PAGE_SIZE]),
            fast: FastPermissions::new(len / PAGE_SIZE),
            epoch: origin,
            versions: vec![origin; len / PAGE_SIZE],
            watched: vec![false; len / PAGE_SIZE],
            code: BTreeMap::new(),
            mapped: Arc::new(vec![false; len / PAGE_SIZE]),
            dirty_pages: BTreeMap::new(),
            dirty_ranges: Some(Vec::new()),
            reservation: Some(reservation),
            shared_pages: BTreeMap::new(),
            private_copy_pages: BTreeMap::new(),
            resident: BTreeMap::new(),
            locked_pages: BTreeSet::new(),
            #[cfg(windows)]
            native_protection: false,
            #[cfg(windows)]
            native_read_exceptions: 0,
            #[cfg(unix)]
            window: None,
        })
    }

    /// Guest memory placed in a 4 GiB-aligned `GuestWindow` at its guest
    /// addresses (A64-MEM-2). `ptr` is the always accessible alias, so
    /// every Rust and interpreter access is unchanged; the window view's host
    /// protection follows the guest's for translated code. Pages the host
    /// cannot protect exactly stay inaccessible there as soft pages.
    #[cfg(unix)]
    pub fn window(base: u32, len: usize) -> Result<Self, String> {
        Self::window_at_host_size(base, len, crate::virtual_memory::host_page_size()?)
    }

    #[cfg(unix)]
    fn window_at_host_size(base: u32, len: usize, host_size: usize) -> Result<Self, String> {
        if !len.is_multiple_of(PAGE_SIZE) || len == 0 || u64::from(base) + len as u64 > 1 << 32 {
            return Err("invalid guest window".into());
        }
        let mut memory = Self::reserve(base, PAGE_SIZE)?;
        let window = crate::virtual_memory::GuestWindow::new()?;
        // The placement starts on a host page; `slack` guest pages below
        // `base` belong to it but never to the guest.
        let slack = base as usize % host_size;
        let reservation = crate::virtual_memory::Reservation::new_direct_in_at_host_size(
            &window,
            base as usize - slack,
            slack + len,
            host_size,
        )?;
        let origin = epoch_origin();
        memory.len = len;
        let alias = reservation
            .alias()
            .ok_or("direct reservation without alias")?;
        // SAFETY: the alias spans `slack + len` bytes.
        memory.ptr = unsafe { alias.add(slack) };
        memory.perms = Arc::new(vec![0; len / PAGE_SIZE]);
        memory.fast = FastPermissions::new(len / PAGE_SIZE);
        memory.versions = vec![origin; len / PAGE_SIZE];
        memory.watched = vec![false; len / PAGE_SIZE];
        memory.mapped = Arc::new(vec![false; len / PAGE_SIZE]);
        memory.reservation = Some(reservation);
        memory.window = Some(window);
        Ok(memory)
    }

    /// Host address of guest address 0 for memory made by `window`.
    #[cfg(unix)]
    pub fn window_base(&self) -> Option<usize> {
        self.window.as_ref().map(|window| window.base())
    }

    /// Reservation offset of guest offset 0: nonzero only for a window whose
    /// base is not host page aligned.
    #[cfg(unix)]
    fn window_slack(&self) -> usize {
        let reservation = self.reservation.as_ref().unwrap();
        self.window_base().unwrap() + self.base as usize - reservation.ptr.as_ptr() as usize
    }

    fn commit_pages(&self, offset: usize, len: usize) -> Result<(), String> {
        let reservation = self
            .reservation
            .as_ref()
            .ok_or("mapping requires reserved address space")?;
        #[cfg(unix)]
        if self.window.is_some() {
            return reservation.commit_direct(
                self.window_slack() + offset,
                len,
                crate::virtual_memory::GuestPageProtection::NONE,
            );
        }
        reservation.commit(offset, len)
    }

    fn decommit_pages(&self, offset: usize, len: usize) -> Result<(), String> {
        let reservation = self
            .reservation
            .as_ref()
            .ok_or("unmapping requires reserved address space")?;
        #[cfg(unix)]
        if self.window.is_some() {
            return reservation.decommit_direct(self.window_slack() + offset, len);
        }
        reservation.decommit(offset, len)
    }

    /// Makes the window view's protection of `pages` follow the guest's.
    /// Only committed pages count: overlays (shared and file pages) live
    /// outside the reservation and stay inaccessible in the window.
    fn sync_window(&self, pages: std::ops::Range<usize>) -> Result<(), String> {
        #[cfg(unix)]
        if self.window.is_some() {
            use crate::virtual_memory::GuestPageProtection;
            let reservation = self.reservation.as_ref().unwrap();
            let committed =
                |page: usize| self.mapped[page] && !self.shared_pages.contains_key(&page);
            // Owned private copies read linearly, as the native permission
            // snapshot says; stores still trap so the first one retires COPY.
            let protection = |page: usize| {
                let perms = self.perms[page];
                if perms & SHARED == 0 {
                    GuestPageProtection::new(perms & 7, self.watched[page])
                } else if self.private_copy_pages.get(&page) == Some(&true) {
                    GuestPageProtection::new(perms & (READ | EXEC), self.watched[page])
                } else {
                    Ok(GuestPageProtection::NONE)
                }
                .expect("guest permissions")
            };
            let mut page = pages.start;
            while page < pages.end {
                if !committed(page) {
                    page += 1;
                    continue;
                }
                let wanted = protection(page);
                let start = page;
                while page < pages.end && committed(page) && protection(page) == wanted {
                    page += 1;
                }
                reservation.protect_direct(
                    self.window_slack() + start * PAGE_SIZE,
                    (page - start) * PAGE_SIZE,
                    wanted,
                )?;
            }
        }
        #[cfg(not(unix))]
        let _ = pages;
        Ok(())
    }

    pub fn pages(&self, addr: u32, len: usize) -> Result<std::ops::Range<usize>, String> {
        if !addr.is_multiple_of(4096) || len == 0 || !len.is_multiple_of(4096) {
            return Err("unaligned page range".into());
        }
        let r = self.range(addr, len)?;
        Ok(r.start / PAGE_SIZE..r.end / PAGE_SIZE)
    }

    pub fn committed(&self, addr: u32, len: usize) -> Result<std::ops::Range<usize>, String> {
        let r = self.range(addr, len)?;
        if !r.is_empty()
            && self.mapped[r.start / PAGE_SIZE..=(r.end - 1) / PAGE_SIZE]
                .iter()
                .any(|&v| !v)
        {
            return Err(format!("unmapped guest memory at {addr:#x}"));
        }
        Ok(r)
    }

    pub fn map(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        let pages = self.pages(addr, len)?;
        if perms & !7 != 0 || self.mapped[pages.clone()].iter().any(|&v| v) {
            return Err("invalid or overlapping mapping".into());
        }
        self.commit_pages(pages.start * PAGE_SIZE, len)?;
        Arc::make_mut(&mut self.mapped)[pages.clone()].fill(true);
        if let Err(error) = self.protect(addr, len, perms) {
            self.decommit_pages(pages.start * PAGE_SIZE, len)?;
            Arc::make_mut(&mut self.mapped)[pages].fill(false);
            return Err(error);
        }
        Ok(())
    }

    pub fn unmap(&mut self, addr: u32, len: usize) -> Result<(), String> {
        let pages = self.pages(addr, len)?;
        self.committed(addr, len)?;
        // Retire pins while their original committed host addresses still exist.
        // Failure keeps the remaining ledger and mapping, so reuse cannot inherit it.
        self.retire_page_locks(pages.clone())
            .map_err(|error| format!("unpin before unmapping: {error:?}"))?;
        if self.reservation.is_none() {
            return Err("unmapping requires reserved address space".into());
        }
        for page in pages.clone() {
            if let Some(file) = self.resident.remove(&page) {
                file.evict();
            }
        }
        // File overlays are not linear. PCM pages have their own section views
        // in the reservation, whose removal cannot invalidate a callback alias.
        let mut start = pages.start;
        while start < pages.end {
            if self
                .shared_pages
                .get(&start)
                .is_some_and(SharedPage::file_page)
            {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < pages.end
                && !self
                    .shared_pages
                    .get(&end)
                    .is_some_and(SharedPage::file_page)
            {
                end += 1;
            }
            self.decommit_pages(start * PAGE_SIZE, (end - start) * PAGE_SIZE)?;
            start = end;
        }
        for page in pages.clone() {
            #[cfg(windows)]
            if Self::native_read_exception(self.perms[page]) {
                self.native_read_exceptions -= 1;
            }
            self.shared_pages.remove(&page);
            self.private_copy_pages.remove(&page);
        }
        Arc::make_mut(&mut self.mapped)[pages.clone()].fill(false);
        self.fast.invalidate(pages.clone());
        Arc::make_mut(&mut self.perms)[pages].fill(0);
        self.touch(addr, len);
        Ok(())
    }

    /// Distinguishes live windows; epoch overflow only reads as a new one.
    pub fn identity(&self) -> u64 {
        self.epoch >> EPOCH_BITS
    }

    /// Epochs this window has counted, comparable across windows.
    #[cfg(any(test, feature = "testing"))]
    pub fn epochs(&self) -> u64 {
        self.epoch & ((1 << EPOCH_BITS) - 1)
    }

    pub fn touch(&mut self, addr: u32, len: usize) {
        if len == 0 {
            return;
        }
        let start = (addr - self.base) as usize / PAGE_SIZE;
        let last = ((addr - self.base) as usize + len - 1) / PAGE_SIZE;
        for page in start..=last {
            self.dirty_pages
                .entry(self.base + page as u32 * PAGE_SIZE as u32)
                .or_insert(self.versions[page]);
        }
        match &mut self.dirty_ranges {
            Some(ranges) if ranges.len() < DIRTY_RANGES => ranges.push((addr, len)),
            ranges => *ranges = None,
        }
        self.epoch += 1;
        for version in &mut self.versions[start..=last] {
            *version = self.epoch;
        }
    }

    pub fn range(&self, addr: u32, len: usize) -> Result<std::ops::Range<usize>, String> {
        let start = addr
            .checked_sub(self.base)
            .ok_or("address below memory window")? as usize;
        let end = start
            .checked_add(len)
            .filter(|&end| end <= self.len)
            .ok_or("address outside memory window")?;
        Ok(start..end)
    }

    /// Borrow ordinary exclusively owned storage. Shared device pages require
    /// read_into/read_data so no ordinary reference escapes over atomic bytes.
    pub fn read(&self, addr: u32, len: usize) -> Result<&[u8], String> {
        let range = self.committed(addr, len)?;
        if self.overlays(&range) {
            return Err("shared bytes require an atomic copy".into());
        }
        #[cfg(windows)]
        if self.native_protection && self.host_no_access(&range) {
            return Err("protected host bytes require read_into or read_data".into());
        }
        // SAFETY: committed ordinary storage remains immutable for this borrow.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(range.start), range.len()) })
    }

    pub fn read_data(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        let range = self.committed(addr, len)?;
        if self.overlays(&range) || self.host_copy_required(&range) {
            let mut bytes = vec![0; len];
            self.read_into(addr, &mut bytes)?;
            return Ok(Cow::Owned(bytes));
        }
        // SAFETY: bounds checked, allocation alive, shared borrow prevents executor writes.
        Ok(Cow::Borrowed(unsafe {
            std::slice::from_raw_parts(self.ptr.as_ptr().add(range.start), range.len())
        }))
    }

    /// Whether committed guest bytes at `addr` equal `expected`. Ordinary
    /// storage is compared in place; overlays go through a stack buffer, so
    /// code validation never allocates.
    pub fn matches(&self, addr: u32, expected: &[u8]) -> bool {
        let Ok(range) = self.committed(addr, expected.len()) else {
            return false;
        };
        if !self.overlays(&range) && !self.host_copy_required(&range) {
            // SAFETY: bounds checked, allocation alive, shared borrow prevents executor writes.
            let bytes = unsafe {
                std::slice::from_raw_parts(self.ptr.as_ptr().add(range.start), range.len())
            };
            return bytes == expected;
        }
        let mut buffer = [0; PAGE_SIZE];
        let mut at = addr;
        expected.chunks(PAGE_SIZE).all(|chunk| {
            let bytes = &mut buffer[..chunk.len()];
            let same = self.read_into(at, bytes).is_ok() && bytes == chunk;
            at = at.wrapping_add(chunk.len() as u32);
            same
        })
    }

    pub fn read_into(&self, addr: u32, destination: &mut [u8]) -> Result<(), String> {
        let range = self.committed(addr, destination.len())?;
        #[cfg(windows)]
        let _host = Self::host_scope(
            self.reservation.as_ref(),
            &self.perms,
            self.native_protection,
            &range,
            false,
        )?;
        if !self.overlays(&range) {
            // SAFETY: committed ordinary storage and non-aliasing caller output.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.ptr.as_ptr().add(range.start),
                    destination.as_mut_ptr(),
                    range.len(),
                )
            };
            return Ok(());
        }
        let mut at = range.start;
        while at < range.end {
            let page = at / PAGE_SIZE;
            let len = (PAGE_SIZE - at % PAGE_SIZE).min(range.end - at);
            let output = &mut destination[at - range.start..at - range.start + len];
            if self.perms[page] & AUDIO != 0 {
                for (index, byte) in output.iter_mut().enumerate() {
                    // SAFETY: committed linear PCM views stay live for this
                    // memory borrow. Guest and callback use matching AtomicU8.
                    *byte = unsafe {
                        (&*self.ptr.as_ptr().add(at + index).cast::<AtomicU8>())
                            .load(Ordering::Relaxed)
                    };
                }
            } else if let Some(storage) = self.shared_pages.get(&page) {
                storage.read_into(at % PAGE_SIZE, output)?;
            } else {
                // SAFETY: committed ordinary page, valid non-aliasing output.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.ptr.as_ptr().add(at),
                        output.as_mut_ptr(),
                        len,
                    )
                };
            }
            at += len;
        }
        Ok(())
    }

    /// Whether `page` holds its bytes in the linear storage: it is not
    /// shared, or it is an owned private copy, whose SHARED bit is only a
    /// store barrier (as `native_permissions` treats it).
    fn linear(&self, page: usize) -> bool {
        self.perms[page] & SHARED == 0 || self.private_copy_pages.get(&page) == Some(&true)
    }

    /// Like `read`, but owned private copies are read too.
    pub fn read_linear(&self, addr: u32, len: usize) -> Result<&[u8], String> {
        let range = self.committed(addr, len)?;
        if !range.is_empty()
            && !(range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE)
                .all(|page| self.linear(page))
        {
            return Err("shared bytes require an atomic copy".into());
        }
        #[cfg(windows)]
        if self.native_protection && self.host_no_access(&range) {
            return Err("protected host bytes require read_into or read_data".into());
        }
        // SAFETY: committed linear storage remains immutable for this borrow.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(range.start), range.len()) })
    }

    /// Like `direct` for a read, but owned private copies are read too.
    #[inline]
    pub fn direct_read(&self, addr: u32, len: usize) -> Option<*const u8> {
        if let Some(p) = self.direct(addr, len, false) {
            return Some(p);
        }
        // Private copies may be host-protected under native protection.
        if cfg!(windows) {
            return None;
        }
        let offset = addr.wrapping_sub(self.base) as usize;
        let page = offset / PAGE_SIZE;
        let last = offset.checked_add(len.checked_sub(1)?)?;
        let readable = last < self.len
            && page == last / PAGE_SIZE
            && self.mapped[page]
            && self.perms[page] & READ != 0
            && self.linear(page);
        // SAFETY: offset + len lies inside the owned allocation.
        readable.then(|| unsafe { self.ptr.as_ptr().add(offset).cast_const() })
    }

    /// Host address of `len` bytes within one ordinary page that guest code
    /// may access directly: mapped, permitted, not shared, and for writes not
    /// executable, so no overlay, code invalidation or host protection scope
    /// is involved. Such pages are host-accessible under native protection.
    #[inline]
    pub fn direct(&self, addr: u32, len: usize, write: bool) -> Option<*mut u8> {
        let offset = addr.wrapping_sub(self.base) as usize;
        let last = offset.checked_add(len.checked_sub(1)?)?;
        if last >= self.len || offset / PAGE_SIZE != last / PAGE_SIZE {
            return None;
        }
        let page = offset / PAGE_SIZE;
        let perms = self.perms[page];
        let allowed = if write {
            perms & WRITE != 0 && perms & (EXEC | SHARED) == 0
        } else {
            perms & READ != 0 && perms & SHARED == 0
        };
        // SAFETY: offset + len lies inside the owned allocation.
        (allowed && self.mapped[page]).then(|| unsafe { self.ptr.as_ptr().add(offset) })
    }

    fn overlays(&self, range: &std::ops::Range<usize>) -> bool {
        !range.is_empty()
            && self.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE]
                .iter()
                .any(|p| p & SHARED != 0)
    }

    pub fn shared_range(&self, address: u32, len: usize) -> bool {
        self.range(address, len)
            .is_ok_and(|range| self.overlays(&range))
    }

    /// One writable PCM page, with no code or fallible file storage involved.
    pub(crate) fn writable_audio(&self, address: u32, len: usize) -> bool {
        let Ok(range) = self.range(address, len) else {
            return false;
        };
        if range.is_empty() || range.start / PAGE_SIZE != (range.end - 1) / PAGE_SIZE {
            return false;
        }
        let page = range.start / PAGE_SIZE;
        self.perms[page] & (WRITE | EXEC) == WRITE
            && matches!(self.shared_pages.get(&page), Some(SharedPage::Audio { .. }))
    }

    /// Distinct guest views can overlap physically. REP must retain its element
    /// order (including propagation) instead of snapshotting such bytes.
    pub fn rep_alias_hazard(&self, source: u32, destination: u32, len: usize) -> bool {
        let (Ok(src), Ok(dst)) = (self.range(source, len), self.range(destination, len)) else {
            return true;
        };
        let aliased = |range: &std::ops::Range<usize>| {
            self.overlays(range)
                || (!range.is_empty()
                    && self
                        .resident
                        .range(range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE)
                        .next()
                        .is_some())
        };
        if !aliased(&src) || !aliased(&dst) {
            return false;
        }
        if len == 0 {
            return false;
        }
        let mut source_spans = std::collections::HashMap::<(u8, usize), Vec<(usize, usize)>>::new();
        for (page, (kind, id, base)) in self.identities(&src) {
            let begin = src.start.max(page * PAGE_SIZE) - page * PAGE_SIZE + base;
            let end = src.end.min((page + 1) * PAGE_SIZE) - page * PAGE_SIZE + base;
            source_spans
                .entry((kind, id))
                .or_default()
                .push((begin, end));
        }
        for (page, (kind, id, base)) in self.identities(&dst) {
            let begin = dst.start.max(page * PAGE_SIZE) - page * PAGE_SIZE + base;
            let end = dst.end.min((page + 1) * PAGE_SIZE) - page * PAGE_SIZE + base;
            if source_spans
                .get(&(kind, id))
                .is_some_and(|spans| spans.iter().any(|&(a, b)| begin < b && a < end))
            {
                return true;
            }
        }
        false
    }

    /// The storage behind each overlay or resident file page of `range`.
    fn identities(
        &self,
        range: &std::ops::Range<usize>,
    ) -> impl Iterator<Item = (usize, (u8, usize, usize))> + '_ {
        let pages = range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE;
        let shared = self
            .shared_pages
            .range(pages.clone())
            .map(|(&page, storage)| (page, storage.identity()));
        let resident = self
            .resident
            .range(pages)
            .map(|(&page, file)| (page, (1, std::rc::Rc::as_ptr(file) as usize, 0)));
        shared.chain(resident)
    }

    pub fn map_shared_audio(
        &mut self,
        addr: u32,
        len: usize,
    ) -> Result<Arc<SharedAllocation>, String> {
        if self.reservation.is_none() {
            return Err("shared audio requires reserved guest memory".into());
        }
        let pages = self.pages(addr, len)?;
        if self.mapped[pages.clone()].iter().any(|&mapped| mapped) {
            return Err("overlapping shared audio mapping".into());
        }
        #[cfg(windows)]
        for range in self
            .reservation
            .as_ref()
            .unwrap()
            .section_promotion_ranges(pages.start * PAGE_SIZE, len)?
        {
            if self
                .locked_pages
                .range(range.start / PAGE_SIZE..range.end / PAGE_SIZE)
                .next()
                .is_some()
            {
                log::warn!(
                    "unsupported PCM mapping at guest {addr:#x} (+{len}): private host region {}..{} contains neighbouring physical page pins that promotion would retire",
                    range.start,
                    range.end,
                );
                return Err("PCM section promotion would replace neighbouring physically pinned private pages".into());
            }
        }
        let storage = SharedAllocation::new(len)?;
        #[cfg(windows)]
        storage.map_linear(self.reservation.as_ref().unwrap(), pages.start * PAGE_SIZE)?;
        for (index, page) in pages.clone().enumerate() {
            self.shared_pages.insert(
                page,
                SharedPage::Audio {
                    storage: storage.clone(),
                    offset: index * PAGE_SIZE,
                },
            );
        }
        Arc::make_mut(&mut self.mapped)[pages.clone()].fill(true);
        self.fast.invalidate(pages.clone());
        Arc::make_mut(&mut self.perms)[pages]
            .fill(READ | WRITE | SHARED | if cfg!(windows) { AUDIO } else { 0 });
        self.touch(addr, len);
        Ok(storage)
    }

    /// Prepared private bytes may execute without externally mutable code.
    pub fn map_private_copy_pages(
        &mut self,
        addr: u32,
        bytes: &[u8],
        perms: u8,
    ) -> Result<(), String> {
        let pages = self.pages(addr, bytes.len())?;
        if perms & !7 != 0 {
            return Err("invalid private snapshot protection".into());
        }
        self.map(addr, bytes.len(), READ | WRITE)?;
        if let Err(error) = self.write(addr, bytes) {
            let _ = self.unmap(addr, bytes.len());
            return Err(error);
        }
        for page in pages.clone() {
            self.private_copy_pages.insert(page, true);
            #[cfg(windows)]
            if Self::native_read_exception(self.perms[page]) {
                self.native_read_exceptions -= 1;
            }
            Arc::make_mut(&mut self.perms)[page] |= SHARED;
        }
        self.fast.invalidate(pages);
        if let Err(error) = self.protect(addr, bytes.len(), perms) {
            let _ = self.unmap(addr, bytes.len());
            return Err(error);
        }
        Ok(())
    }

    pub fn privatize_copy_pages(&mut self, addr: u32, len: usize) -> Result<(), String> {
        let pages = self.pages(addr, len)?;
        let range = self.committed(addr, len)?;
        if pages
            .clone()
            .any(|page| !self.private_copy_pages.contains_key(&page))
        {
            return Err("only owned private snapshots can be privatized".into());
        }
        self.prepare_private_copy_range(&range)?;
        Ok(())
    }
    fn prepare_private_copy_range(&mut self, range: &std::ops::Range<usize>) -> Result<(), String> {
        if range.is_empty() {
            return Ok(());
        }
        let begin = range.start / PAGE_SIZE;
        let end = (range.end - 1) / PAGE_SIZE + 1;
        let changed: Vec<_> = self
            .private_copy_pages
            .range(begin..end)
            .filter_map(|(&page, &copy)| copy.then_some(page))
            .collect();
        // Restore hardware permissions before removing the execution barrier.
        // A failure leaves all COPY state and inline grants unchanged.
        #[cfg(windows)]
        if self.native_protection {
            let reservation = self.reservation.as_ref().unwrap();
            let mut scopes = Vec::new();
            for &page in &changed {
                scopes.push(unsafe {
                    reservation.temporary_protection(
                        page * PAGE_SIZE,
                        PAGE_SIZE,
                        Self::native_permission(self.perms[page] & !SHARED),
                    )
                }?);
            }
            for scope in scopes {
                scope.retain()
            }
        }
        for &page in &changed {
            *self.private_copy_pages.get_mut(&page).unwrap() = false;
            Arc::make_mut(&mut self.perms)[page] &= !SHARED;
            #[cfg(windows)]
            if Self::native_read_exception(self.perms[page]) {
                self.native_read_exceptions += 1;
            }
            self.fast.invalidate(page..page + 1);
        }
        if !changed.is_empty() {
            self.epoch += 1;
        }
        for &page in &changed {
            self.sync_window(page..page + 1)?;
        }
        Ok(())
    }

    pub fn map_file_pages(
        &mut self,
        addr: u32,
        backing: &[std::rc::Rc<crate::backend::FilePage>],
        perms: u8,
        copy_on_write: bool,
    ) -> Result<(), String> {
        let len = backing
            .len()
            .checked_mul(PAGE_SIZE)
            .ok_or("file view length overflow")?;
        let pages = self.pages(addr, len)?;
        if self.reservation.is_none()
            || perms & !(READ | WRITE) != 0
            || self.mapped[pages.clone()].iter().any(|&mapped| mapped)
        {
            return Err("invalid or overlapping data mapping".into());
        }
        for (page, storage) in pages.clone().zip(backing) {
            self.shared_pages.insert(
                page,
                SharedPage::File {
                    page: storage.clone(),
                    copy_on_write,
                },
            );
        }
        Arc::make_mut(&mut self.mapped)[pages.clone()].fill(true);
        self.fast.invalidate(pages.clone());
        Arc::make_mut(&mut self.perms)[pages].fill(perms | SHARED);
        self.touch(addr, len);
        Ok(())
    }

    /// Moves a prepared, non-copy-on-write file page into this window while
    /// no other view holds it there. Native code then accesses it as ordinary
    /// memory, and every other accessor reaches the same bytes through the
    /// page. Dropping SHARED never invalidates a native assumption: code
    /// still holding the old permission takes the helper path to these bytes.
    fn make_resident(&mut self, page: usize) {
        #[cfg(windows)]
        if self.native_protection {
            return;
        }
        if self.reservation.is_none() {
            return;
        }
        let Some(SharedPage::File {
            page: storage,
            copy_on_write: false,
        }) = self.shared_pages.get(&page)
        else {
            return;
        };
        if storage.resident() || self.commit_pages(page * PAGE_SIZE, PAGE_SIZE).is_err() {
            return;
        }
        let storage = storage.clone();
        // SAFETY: the committed window page stays valid until unmap or drop
        // evicts it, and FilePage access is confined to this worker.
        if unsafe { storage.make_resident(self.ptr.as_ptr().add(page * PAGE_SIZE)) }.is_err() {
            let _ = self.decommit_pages(page * PAGE_SIZE, PAGE_SIZE);
            return;
        }
        self.shared_pages.remove(&page);
        self.resident.insert(page, storage);
        Arc::make_mut(&mut self.perms)[page] &= !SHARED;
        if self.sync_window(page..page + 1).is_err() {
            // Translated code then reaches the page as a soft page.
            log::warn!("cannot open resident file page {page:#x} in the guest window");
        }
        self.fast.invalidate(page..page + 1);
        self.touch(self.base + (page * PAGE_SIZE) as u32, PAGE_SIZE);
    }
    pub fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        let page = addr.checked_sub(self.base)? as usize / PAGE_SIZE;
        if let Some(copy) = self.private_copy_pages.get(&page) {
            return Some(*copy);
        }
        if self.resident.contains_key(&page) {
            return Some(false);
        }
        match self.shared_pages.get(&page)? {
            SharedPage::File { copy_on_write, .. } => Some(*copy_on_write),
            _ => None,
        }
    }

    /// Prepare a native guest memory operand before changing its registers or
    /// flags. Validate the full range first, then resolve lazy file reads and
    /// stage every COW snapshot before installing any private page. Prepared
    /// file reads/stores cannot encounter a later first-touch IO failure while
    /// the same execution owner retains this memory borrow.
    ///
    /// This does not expose an ordinary shared Rust slice. PCM still requires
    /// matching byte atomics; executable-page stores stay in the cold lane.
    pub fn prepare_native_access(
        &mut self,
        addr: u32,
        len: usize,
        write: bool,
    ) -> Result<(), String> {
        let range = self.committed(addr, len)?;
        if !range.is_empty() {
            let required = if write { WRITE } else { READ };
            let permissions = &self.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE];
            if permissions.iter().any(|p| p & required == 0) {
                return Err("native guest access permission fault".into());
            }
            // The same rule as the native permission snapshot: only observed
            // code pages keep the store barrier in selective mode. Rejecting
            // every EXEC page here turned unobserved RWX shared-page stores,
            // which the snapshot admits, into guest write faults.
            if write && self.writes_code(&range) {
                return Err("native executable-page stores require the cold lane".into());
            }
        }
        self.prepare_file_range(&range, write)
    }

    fn prepare_file_range(
        &mut self,
        range: &std::ops::Range<usize>,
        write: bool,
    ) -> Result<(), String> {
        if range.is_empty() {
            return Ok(());
        }
        let mut private = Vec::new();
        for (&index, storage) in self
            .shared_pages
            .range(range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE)
        {
            if let SharedPage::File {
                page,
                copy_on_write,
            } = storage
            {
                if write && *copy_on_write {
                    private.push((index, crate::backend::FilePage::new(page.snapshot()?)));
                } else {
                    page.prepare()?;
                }
            }
        }
        if write {
            self.prepare_private_copy_range(range)?;
        }
        for (index, prepared) in private {
            let Some(SharedPage::File {
                page,
                copy_on_write,
            }) = self.shared_pages.get_mut(&index)
            else {
                unreachable!()
            };
            *page = prepared;
            *copy_on_write = false;
        }
        // Snapshot mappings already own their initialized linear bytes. The
        // barrier guarantees that no inline store can bypass this first write.
        // All permissions and fallible file snapshots were checked above.
        for page in range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE {
            self.make_resident(page);
        }
        Ok(())
    }

    pub fn copy(&mut self, source: u32, destination: u32, len: usize) -> Result<(), String> {
        let src = self.committed(source, len)?;
        let dst = self.committed(destination, len)?;
        if self.overlays(&src) || self.overlays(&dst) {
            let mut scratch = [0; PAGE_SIZE];
            let backwards =
                destination > source && u64::from(destination) < u64::from(source) + len as u64;
            let mut left = len;
            let mut at = 0;
            while left != 0 {
                let count = left.min(PAGE_SIZE);
                let offset = if backwards { left - count } else { at };
                self.read_into(source + offset as u32, &mut scratch[..count])?;
                self.write(destination + offset as u32, &scratch[..count])?;
                left -= count;
                at += count;
            }
            return Ok(());
        }
        if self.writes_code(&dst) {
            self.touch(destination, len);
        }
        #[cfg(windows)]
        let _source_host = Self::host_scope(
            self.reservation.as_ref(),
            &self.perms,
            self.native_protection,
            &src,
            false,
        )?;
        #[cfg(windows)]
        let _destination_host = Self::host_scope(
            self.reservation.as_ref(),
            &self.perms,
            self.native_protection,
            &dst,
            true,
        )?;
        // SAFETY: committed ordinary ranges, exclusively borrowed memory, and
        // ptr::copy preserves memmove overlap semantics.
        unsafe {
            std::ptr::copy(
                self.ptr.as_ptr().add(src.start),
                self.ptr.as_ptr().add(dst.start),
                len,
            )
        };
        Ok(())
    }

    pub fn fill(&mut self, address: u32, pattern: &[u8], len: usize) -> Result<(), String> {
        self.committed(address, len)?;
        if pattern.is_empty() {
            return Err("empty fill pattern".into());
        }
        let mut scratch = [0; PAGE_SIZE];
        let mut at = 0;
        while at < len {
            let count = (len - at).min(PAGE_SIZE);
            for (index, byte) in scratch[..count].iter_mut().enumerate() {
                *byte = pattern[(at + index) % pattern.len()];
            }
            self.write(address + at as u32, &scratch[..count])?;
            at += count;
        }
        Ok(())
    }

    /// Host-side initialization or patch write. Bypasses guest permissions but
    /// validates mappings and updates executable-page versions. Guest stores
    /// must validate WRITE permission before using this operation.
    pub fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
        let range = self.committed(addr, bytes.len())?;
        let overlays = self.overlays(&range);
        // Keep ordinary stores allocation-free. Overlay stores resolve every
        // fallible page read and private COW snapshot before committing bytes,
        // so a later I/O failure cannot leave a restarted scalar store half done.
        if overlays && !range.is_empty() {
            self.prepare_file_range(&range, true)?;
        }
        if self.writes_code(&range) {
            self.touch(addr, bytes.len());
        }
        #[cfg(windows)]
        let _host = Self::host_scope(
            self.reservation.as_ref(),
            &self.perms,
            self.native_protection,
            &range,
            true,
        )?;
        if overlays {
            let mut at = range.start;
            while at < range.end {
                let page = at / PAGE_SIZE;
                let len = (PAGE_SIZE - at % PAGE_SIZE).min(range.end - at);
                let input = &bytes[at - range.start..at - range.start + len];
                if self.perms[page] & AUDIO != 0 {
                    for (index, &byte) in input.iter().enumerate() {
                        // SAFETY: validated live writable OS view and only
                        // byte-atomic stores, including split word/SIMD writes.
                        unsafe {
                            (&*self.ptr.as_ptr().add(at + index).cast::<AtomicU8>())
                                .store(byte, Ordering::Relaxed)
                        };
                    }
                } else if let Some(storage) = self.shared_pages.get_mut(&page) {
                    storage.write(at % PAGE_SIZE, input)?;
                } else {
                    // SAFETY: committed ordinary subrange and exclusive borrow.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            input.as_ptr(),
                            self.ptr.as_ptr().add(at),
                            len,
                        )
                    };
                }
                at += len;
            }
            return Ok(());
        }
        // SAFETY: checked range and exclusive borrow; source cannot alias this allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr.as_ptr().add(range.start),
                bytes.len(),
            )
        };
        Ok(())
    }

    /// Set guest permissions and invalidate dependent decoded/compiled code.
    pub fn protect(&mut self, addr: u32, len: usize, perms: u8) -> Result<(), String> {
        if !(addr as usize).is_multiple_of(PAGE_SIZE)
            || len == 0
            || !len.is_multiple_of(PAGE_SIZE)
            || perms & !7 != 0
        {
            return Err("invalid page permissions or alignment".into());
        }
        let range = self.committed(addr, len)?;
        if perms & EXEC != 0
            && (self
                .shared_pages
                .range(range.start / PAGE_SIZE..range.end / PAGE_SIZE)
                .any(|(_, page)| page.file_page())
                || self
                    .resident
                    .range(range.start / PAGE_SIZE..range.end / PAGE_SIZE)
                    .next()
                    .is_some())
        {
            return Err("executable file views are not supported".into());
        }
        // Native NOACCESS removes pages from the working set and ends their pin.
        // Do the same when host protection is disabled; restore prior pins if the
        // subsequent protection transaction fails.
        let retired = if perms == 0 {
            self.retire_page_locks(range.start / PAGE_SIZE..range.end / PAGE_SIZE)
                .map_err(|error| format!("unpin before NOACCESS: {error:?}"))?
        } else {
            Vec::new()
        };
        #[cfg(windows)]
        let host_result = (|| {
            if self.native_protection {
                let reservation = self.reservation.as_ref().unwrap();
                let pages = range.start / PAGE_SIZE..range.end / PAGE_SIZE;
                let mut scopes = Vec::new();
                for page in pages.clone() {
                    if self.perms[page] & SHARED != 0 && self.perms[page] & AUDIO == 0 {
                        continue;
                    }
                    scopes.push(unsafe {
                        reservation.temporary_protection(
                            page * PAGE_SIZE,
                            PAGE_SIZE,
                            Self::native_permission(perms),
                        )
                    }?);
                }
                for scope in scopes {
                    scope.retain();
                }
            }
            Ok::<(), String>(())
        })();
        #[cfg(windows)]
        if let Err(error) = host_result {
            let restoration = self.restore_page_locks(&retired);
            return Err(format!("{error}; pin restoration: {restoration:?}"));
        }
        #[cfg(not(windows))]
        {
            debug_assert!(retired.is_empty());
        }
        self.fast
            .invalidate(range.start / PAGE_SIZE..range.end / PAGE_SIZE);
        for page in
            &mut Arc::make_mut(&mut self.perms)[range.start / PAGE_SIZE..range.end / PAGE_SIZE]
        {
            #[cfg(windows)]
            {
                self.native_read_exceptions -= usize::from(Self::native_read_exception(*page));
                self.native_read_exceptions += usize::from(Self::native_read_exception(
                    perms | (*page & (SHARED | AUDIO)),
                ));
            }
            *page = perms | (*page & (SHARED | AUDIO));
        }
        self.sync_window(range.start / PAGE_SIZE..range.end / PAGE_SIZE)?;
        self.touch(addr, len);
        Ok(())
    }

    pub fn executable(&self, addr: u32, len: usize) -> bool {
        let Ok(r) = self.range(addr, len) else {
            return false;
        };
        !r.is_empty()
            && self.perms[r.start / PAGE_SIZE..=(r.end - 1) / PAGE_SIZE]
                .iter()
                .all(|p| p & EXEC != 0)
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        // The Reservation release also retires any pin whose host unlock failed.
        // Heap-backed test windows can never acquire a pin.
        let _ = self.retire_page_locks(0..self.len / PAGE_SIZE);
        // Sections outlive their views; give them their bytes back.
        for file in std::mem::take(&mut self.resident).into_values() {
            file.evict();
        }
        if self.reservation.is_some() {
            return;
        }
        // SAFETY: same layout as allocation; executors are dropped before Memory.
        unsafe {
            dealloc(
                self.ptr.as_ptr(),
                Layout::from_size_align(self.len, PAGE_SIZE).unwrap(),
            )
        };
    }
}

/// Host protection queries for native-protection tests in this and engine crates.
#[cfg(all(windows, any(test, feature = "testing")))]
#[doc(hidden)]
pub mod testing {
    use super::Memory;

    #[repr(C)]
    #[derive(Default)]
    pub struct MemoryInfo {
        pub base: usize,
        pub allocation: usize,
        pub allocation_protection: u32,
        pub partition: u16,
        pub region: usize,
        pub state: u32,
        pub protection: u32,
        pub kind: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualQuery(
            address: *const std::ffi::c_void,
            info: *mut std::ffi::c_void,
            len: usize,
        ) -> usize;
    }
    pub fn query(memory: &Memory, address: u32) -> MemoryInfo {
        let mut info = MemoryInfo::default();
        let offset = memory.range(address, 1).unwrap().start;
        // SAFETY: query-only inspection of this live reserved address window.
        assert_ne!(
            unsafe {
                VirtualQuery(
                    memory.ptr.as_ptr().add(offset).cast(),
                    (&mut info as *mut MemoryInfo).cast(),
                    std::mem::size_of::<MemoryInfo>(),
                )
            },
            0
        );
        info
    }
}

#[cfg(all(test, windows))]
mod linear_audio_tests {
    use super::testing::query;
    use super::*;
    #[test]
    fn private_copy_privatization_restores_native_noaccess_permissions() {
        for enable_first in [false, true] {
            let mut memory = Memory::reserve(0x10000, PAGE_SIZE).unwrap();
            if enable_first {
                memory.enable_native_protection().unwrap();
            }
            memory
                .map_private_copy_pages(0x10000, &[0x19; PAGE_SIZE], 0)
                .unwrap();
            if !enable_first {
                memory.enable_native_protection().unwrap();
            }
            memory.privatize_copy_pages(0x10000, PAGE_SIZE).unwrap();
            assert_eq!(memory.file_page_copy_on_write(0x10000), Some(false));
            assert_eq!(query(&memory, 0x10000).protection, 1);
            memory.write(0x10000, &[0x27]).unwrap();
            assert_eq!(query(&memory, 0x10000).protection, 1);
            let mut actual = [0; 1];
            memory.read_into(0x10000, &mut actual).unwrap();
            assert_eq!(actual, [0x27]);
            assert_eq!(query(&memory, 0x10000).protection, 1);
        }
    }

    #[test]
    fn native_policy_host_copies_restore_readonly_noaccess_and_code_versions() {
        let mut memory = Memory::reserve(0x10000, 0x8000).unwrap();
        memory.map(0x10000, 0x8000, READ | WRITE).unwrap();
        memory.write(0x10000, &[0x91, 0x37]).unwrap();
        memory
            .protect(0x10000, PAGE_SIZE, READ | WRITE | EXEC)
            .unwrap();
        memory.protect(0x11000, PAGE_SIZE, 0).unwrap();
        memory.enable_native_protection().unwrap();
        assert_eq!(query(&memory, 0x10000).protection, 2);
        assert_eq!(query(&memory, 0x11000).protection, 1);
        let version = memory.versions[0];
        memory.write(0x10000, &[0x12, 0x34]).unwrap();
        assert!(memory.versions[0] > version);
        assert_eq!(query(&memory, 0x10000).protection, 2);
        memory.write(0x11000, &[0x56, 0x78]).unwrap();
        assert_eq!(query(&memory, 0x11000).protection, 1);
        assert!(memory.read(0x11000, 2).is_err());
        assert_eq!(&*memory.read_data(0x11000, 2).unwrap(), &[0x56, 0x78]);
        assert_eq!(query(&memory, 0x11000).protection, 1);
        memory.copy(0x11000, 0x10000, 2).unwrap();
        assert_eq!(memory.read(0x10000, 2).unwrap(), &[0x56, 0x78]);
        assert_eq!(query(&memory, 0x10000).protection, 2);
        assert_eq!(query(&memory, 0x11000).protection, 1);
        assert_eq!(memory.perms[0], READ | WRITE | EXEC);
        assert_eq!(memory.perms[1], 0);
    }

    #[test]
    fn native_policy_tracks_mapping_changes_and_unrepresentable_read_denials() {
        let mut memory = Memory::reserve(0, 1usize << 32).unwrap();
        memory.map(0x10000, PAGE_SIZE, READ | WRITE).unwrap();
        memory.enable_native_protection().unwrap();
        assert!(memory.native_fault_reads_available());
        memory.protect(0x10000, PAGE_SIZE, WRITE).unwrap();
        assert!(!memory.native_fault_reads_available());
        assert_eq!(query(&memory, 0x10000).protection, 4);
        memory.protect(0x10000, PAGE_SIZE, EXEC).unwrap();
        assert!(!memory.native_fault_reads_available());
        assert_eq!(query(&memory, 0x10000).protection, 2);
        memory.unmap(0x10000, PAGE_SIZE).unwrap();
        assert!(memory.native_fault_reads_available());
        memory.map(0x20000, PAGE_SIZE, READ).unwrap();
        assert_eq!(query(&memory, 0x20000).protection, 2);
        memory.write(0x20000, &[0xab]).unwrap();
        assert_eq!(query(&memory, 0x20000).protection, 2);
        assert_eq!(query(&memory, u32::MAX).state, 0x2000);
        let reservation = memory.reservation.as_ref().unwrap();
        assert_eq!(reservation.len, (1usize << 32) + 16384);
    }

    #[test]
    fn native_policy_pcm_callbacks_and_lazy_file_views_keep_independent_contracts() {
        let mut memory = Memory::reserve(0x10000, 0x8000).unwrap();
        let pcm = memory.map_shared_audio(0x10000, PAGE_SIZE).unwrap();
        let file = crate::backend::FilePage::new([0x37; PAGE_SIZE]);
        memory
            .map_file_pages(0x11000, std::slice::from_ref(&file), READ | WRITE, true)
            .unwrap();
        memory.enable_native_protection().unwrap();
        memory.protect(0x10000, PAGE_SIZE, READ).unwrap();
        memory.write(0x10000, &[0x91]).unwrap();
        assert_eq!(pcm.load_u8(0), 0x91);
        assert_eq!(query(&memory, 0x10000).protection, 2);
        memory.prepare_native_access(0x11000, 1, true).unwrap();
        memory.write(0x11000, &[0x12]).unwrap();
        assert_eq!(file.snapshot().unwrap()[0], 0x37);
        assert_eq!(memory.read_data(0x11000, 1).unwrap()[0], 0x12);
        assert_eq!(query(&memory, 0x11000).state, 0x2000);
        drop(memory);
        assert_eq!(pcm.load_u8(0), 0x91);
    }

    #[test]
    fn native_policy_private_hole_conversion_preserves_neighbour_host_permissions() {
        let mut memory = Memory::reserve(0x10000, 0x8000).unwrap();
        memory.map(0x10000, 0x8000, READ | WRITE).unwrap();
        memory.write(0x10000, &[0x91]).unwrap();
        memory.write(0x12000, &[0x37]).unwrap();
        memory.unmap(0x11000, PAGE_SIZE).unwrap();
        memory.protect(0x10000, PAGE_SIZE, READ | EXEC).unwrap();
        memory.protect(0x12000, PAGE_SIZE, 0).unwrap();
        memory.enable_native_protection().unwrap();
        let pcm = memory.map_shared_audio(0x11000, PAGE_SIZE).unwrap();
        assert_eq!(query(&memory, 0x10000).protection, 2);
        assert_eq!(query(&memory, 0x12000).protection, 1);
        assert_eq!(memory.read_data(0x10000, 1).unwrap()[0], 0x91);
        assert_eq!(memory.read_data(0x12000, 1).unwrap()[0], 0x37);
        memory.write(0x11000, &[0x12]).unwrap();
        assert_eq!(pcm.load_u8(0), 0x12);
    }

    #[test]
    fn linear_pcm_alias_is_stable_page_aligned_atomic_and_independently_owned() {
        let mut memory = Memory::reserve(0x10000, 0x8000).unwrap();
        let ptr = memory.ptr;
        // Neither the guest address nor its offset is 64KB aligned.
        let storage = memory.map_shared_audio(0x12000, 0x3000).unwrap();
        assert_eq!(memory.ptr, ptr);
        let info = query(&memory, 0x12000);
        assert_eq!(info.state, 0x1000); // MEM_COMMIT
        assert_eq!(info.kind, 0x40000); // MEM_MAPPED
        assert_eq!(info.protection, 4); // PAGE_READWRITE CPU alias
        assert_eq!(memory.perms[2], READ | WRITE | SHARED | AUDIO);
        memory.write(0x12000, &[0x37, 0x91]).unwrap();
        assert_eq!(storage.load_u8(0), 0x37);
        // Same operations the native lowering may perform: only AtomicU8, and
        // all bounds/permissions are checked before obtaining a linear pointer.
        unsafe {
            (&*ptr.as_ptr().add(0x2001).cast::<AtomicU8>()).store(0xaa, Ordering::Relaxed);
        }
        assert_eq!(storage.load_u8(1), 0xaa);
        let mut bytes = [0; 2];
        memory.read_into(0x12000, &mut bytes).unwrap();
        assert_eq!(bytes, [0x37, 0xaa]);
        assert!(memory.read(0x12000, 2).is_err());
        memory.protect(0x12000, 0x3000, 0).unwrap();
        assert_eq!(memory.perms[2], SHARED | AUDIO);
        assert_eq!(query(&memory, 0x12000).protection, 4); // guest protection is software-owned
        assert_eq!(storage.load_u8(0), 0x37);
        memory.protect(0x12000, 0x3000, READ | WRITE).unwrap();
        memory.write(0x13000, &[0x55]).unwrap();
        memory.unmap(0x13000, 4096).unwrap();
        assert_eq!(query(&memory, 0x13000).state, 0x2000); // placeholder remains reserved
        assert_eq!(query(&memory, 0x12000).kind, 0x40000);
        memory.map(0x13000, 4096, READ | WRITE).unwrap();
        memory.write(0x13000, &[0xee]).unwrap();
        assert_eq!(storage.load_u8(4096), 0x55);
        drop(memory);
        assert_eq!(storage.load_u8(0), 0x37);
        assert_eq!(storage.load_u8(1), 0xaa);
        assert_eq!(storage.load_u8(4096), 0x55);
    }

    #[test]
    fn audio_reuses_a_private_hole_without_moving_or_losing_its_neighbours() {
        let mut memory = Memory::reserve(0x10000, 0x8000).unwrap();
        memory.map(0x10000, 0x8000, READ | WRITE).unwrap();
        let pattern: Vec<_> = (0..0x8000).map(|i| (i % 251) as u8).collect();
        memory.write(0x10000, &pattern).unwrap();
        memory.protect(0x10000, 0x2000, READ | EXEC).unwrap();
        let ptr = memory.ptr;
        let version = memory.versions[0];
        memory.unmap(0x12000, 0x2000).unwrap();
        let alias = memory.map_shared_audio(0x12000, 0x2000).unwrap();
        assert_eq!(memory.ptr, ptr);
        assert_eq!(memory.read(0x10000, 0x2000).unwrap(), &pattern[..0x2000]);
        assert_eq!(memory.read(0x14000, 0x4000).unwrap(), &pattern[0x4000..]);
        assert_eq!(memory.perms[0], READ | EXEC);
        assert_eq!(memory.versions[0], version); // intact code need not be invalidated
        assert!(query(&memory, 0x10000).kind == 0x20000); // private
        assert_eq!(query(&memory, 0x12000).kind, 0x40000);
        memory.write(0x12000, &[0x7a]).unwrap();
        assert_eq!(alias.load_u8(0), 0x7a);
        memory.unmap(0x12000, 0x2000).unwrap();
        memory.map(0x12000, 0x2000, READ | WRITE).unwrap();
        assert_eq!(memory.read(0x12000, 0x2000).unwrap(), &[0; 0x2000]);
        assert_eq!(memory.read(0x10000, 0x2000).unwrap(), &pattern[..0x2000]);
        assert_eq!(memory.read(0x14000, 0x4000).unwrap(), &pattern[0x4000..]);
        assert_eq!(alias.load_u8(0), 0x7a);
    }

    #[test]
    fn failed_section_mapping_keeps_the_reservation_available_and_neighbours_intact() {
        let mut memory = Memory::reserve(0x10000, 0x5000).unwrap();
        memory.map(0x10000, 0x5000, READ | WRITE).unwrap();
        memory.write(0x10000, &[0x17]).unwrap();
        memory.write(0x14000, &[0x73]).unwrap();
        memory.unmap(0x11000, 0x3000).unwrap();
        let reservation = memory.reservation.as_ref().unwrap();
        assert!(
            reservation
                .map_section(0x1000, 0x3000, (-3isize) as *mut std::ffi::c_void)
                .is_err()
        );
        assert_eq!(memory.read(0x10000, 1).unwrap(), &[0x17]);
        assert_eq!(memory.read(0x14000, 1).unwrap(), &[0x73]);
        memory.map(0x11000, 0x3000, READ | WRITE).unwrap();
        assert_eq!(memory.read(0x11000, 0x3000).unwrap(), &[0; 0x3000]);
    }
}

#[cfg(test)]
mod file_page_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn private_copy_pages_keep_execution_cache_and_retire_barriers_per_write() {
        let mut memory = Memory::reserve(0x10000, 2 * PAGE_SIZE).unwrap();
        memory
            .map_private_copy_pages(0x10000, &[0x37; 2 * PAGE_SIZE], READ | EXEC)
            .unwrap();
        assert!(memory.executable(0x10000, 2 * PAGE_SIZE));
        assert_eq!(memory.read_data(0x10000, 4).unwrap().as_ref(), [0x37; 4]);
        memory.protect(0x10000, PAGE_SIZE, READ | WRITE).unwrap();
        assert!(memory.prepare_native_access(0x10ffe, 4, true).is_err());
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(true));
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(true));
        memory.prepare_native_access(0x10000, 4, false).unwrap();
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(true));
        memory.write(0x10000, &[1, 2, 3, 4]).unwrap();
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(false));
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(true));
        assert_eq!(memory.perms[0], READ | WRITE);
        assert!(memory.direct(0x10000, 4, true).is_some());
        memory
            .protect(0x11000, PAGE_SIZE, READ | WRITE | EXEC)
            .unwrap();
        memory.watch_code(0x11000, 4);
        let version = memory.versions[1];
        memory.copy(0x10000, 0x11000, 4).unwrap();
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(false));
        assert_eq!(memory.read(0x11000, 4).unwrap(), [1, 2, 3, 4]);
        assert!(memory.versions[1] > version);
        memory.unmap(0x10000, PAGE_SIZE).unwrap();
        assert_eq!(memory.file_page_copy_on_write(0x10000), None);
        memory.map(0x10000, PAGE_SIZE, READ | WRITE).unwrap();
        assert_eq!(memory.file_page_copy_on_write(0x10000), None);
        memory.fill(0x10000, &[0x63], PAGE_SIZE).unwrap();
        assert_eq!(memory.read(0x10000, 4).unwrap(), [0x63; 4]);
    }

    #[test]
    fn owned_private_copies_read_linearly_unlike_shared_pages() {
        let mut memory = Memory::reserve(0x10000, 2 * PAGE_SIZE).unwrap();
        memory
            .map_private_copy_pages(0x10000, &[0x37; PAGE_SIZE], READ | EXEC)
            .unwrap();
        assert!(memory.read(0x10ff0, 4).is_err());
        assert_eq!(memory.read_linear(0x10ff0, 4).unwrap(), [0x37; 4]);
        assert_eq!(memory.direct_read(0x10ff0, 4).is_some(), !cfg!(windows));
        assert!(memory.direct_read(0x10ffe, 4).is_none(), "crosses a page");
        memory.map_shared_audio(0x11000, PAGE_SIZE).unwrap();
        assert!(memory.read_linear(0x10ffe, 4).is_err());
        assert!(memory.read_linear(0x11000, 4).is_err());
        assert!(memory.direct_read(0x11000, 4).is_none());
        // A privatized copy is an ordinary page.
        memory.protect(0x10000, PAGE_SIZE, READ | WRITE).unwrap();
        memory.write(0x10000, &[1]).unwrap();
        assert!(memory.read(0x10ff0, 4).is_ok());
    }

    #[test]
    fn code_bytes_match_across_ordinary_and_copy_pages() {
        let mut memory = Memory::reserve(0, 0x30000).unwrap();
        memory.map(0x10000, PAGE_SIZE, READ | EXEC).unwrap();
        memory
            .map_private_copy_pages(0x11000, &[0x29; PAGE_SIZE], READ | EXEC)
            .unwrap();
        memory.write(0x10ffe, &[0x17; 2]).unwrap();
        assert!(memory.matches(0x10ffe, &[0x17, 0x17, 0x29, 0x29]));
        assert!(memory.matches(0x11000, &[0x29; PAGE_SIZE]));
        assert!(!memory.matches(0x10ffe, &[0x17, 0x17, 0x29, 0x2a]));
        assert!(!memory.matches(0x11ffe, &[0x29; 4]));
    }

    #[test]
    fn native_preparation_checks_all_permissions_before_io_or_cow_detach() {
        let calls = Rc::new(Cell::new(0));
        let reads = calls.clone();
        let first = crate::backend::FilePage::lazy(move || {
            reads.set(reads.get() + 1);
            Ok([0x17; PAGE_SIZE])
        });
        let second = crate::backend::FilePage::new([0x29; PAGE_SIZE]);
        let mut memory = Memory::reserve(0, 0x30000).unwrap();
        memory
            .map_file_pages(
                0x10000,
                &[first.clone(), second.clone()],
                READ | WRITE,
                true,
            )
            .unwrap();
        memory.protect(0x11000, PAGE_SIZE, READ).unwrap();
        assert!(memory.prepare_native_access(0x10ffe, 4, true).is_err());
        assert_eq!(calls.get(), 0);
        assert!(!first.initialized());
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(true));
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(true));
        assert!(!first.dirty());
        assert!(!second.dirty());
        // A read must not detach a COPY page or mark either original page dirty.
        memory.prepare_native_access(0x10ffe, 4, false).unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(true));
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(true));
        assert!(!first.dirty());
        assert!(!second.dirty());
    }

    #[test]
    fn native_preparation_io_failure_preserves_all_cow_views_and_prepared_stores_do_no_io() {
        let fail = Rc::new(Cell::new(true));
        let injected = fail.clone();
        let calls = Rc::new(Cell::new(0));
        let reads = calls.clone();
        let first = crate::backend::FilePage::new([0x17; PAGE_SIZE]);
        let second = crate::backend::FilePage::lazy(move || {
            reads.set(reads.get() + 1);
            if injected.get() {
                Err("injected native prepare IO failure".into())
            } else {
                Ok([0x29; PAGE_SIZE])
            }
        });
        let mut memory = Memory::reserve(0, 0x30000).unwrap();
        memory
            .map_file_pages(
                0x10000,
                &[first.clone(), second.clone()],
                READ | WRITE,
                true,
            )
            .unwrap();
        memory
            .map_file_pages(0x20000, &[first.clone(), second.clone()], READ, false)
            .unwrap();
        assert!(memory.prepare_native_access(0x10ffe, 4, true).is_err());
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(true));
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(true));
        assert_eq!(&first.snapshot().unwrap()[PAGE_SIZE - 2..], &[0x17; 2]);
        assert!(!first.dirty());
        assert!(!second.initialized());
        fail.set(false);
        memory.prepare_native_access(0x10ffe, 4, true).unwrap();
        assert_eq!(calls.get(), 2);
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(false));
        assert_eq!(memory.file_page_copy_on_write(0x11000), Some(false));
        assert!(!first.dirty());
        assert!(!second.dirty());
        fail.set(true); // A prepared store cannot call the fallible initializer.
        memory.write(0x10ffe, &[1, 2, 3, 4]).unwrap();
        assert_eq!(calls.get(), 2);
        let mut changed = [0; 4];
        let mut original = [0; 4];
        memory.read_into(0x10ffe, &mut changed).unwrap();
        memory.read_into(0x20ffe, &mut original).unwrap();
        assert_eq!(changed, [1, 2, 3, 4]);
        assert_eq!(original, [0x17, 0x17, 0x29, 0x29]);
        assert!(!first.dirty());
        assert!(!second.dirty());
    }

    #[test]
    fn file_aliases_are_lazy_cow_is_per_page_and_bulk_helpers_stay_coherent() {
        let calls = Rc::new(Cell::new(0));
        let pages: Vec<_> = (0..2)
            .map(|index| {
                let calls = calls.clone();
                crate::backend::FilePage::lazy(move || {
                    calls.set(calls.get() + 1);
                    Ok([index as u8 + 1; 4096])
                })
            })
            .collect();
        let mut memory = Memory::reserve(0, 0x50000).unwrap();
        memory
            .map_file_pages(0x10000, &pages, READ | WRITE, false)
            .unwrap();
        memory
            .map_file_pages(0x20000, &pages, READ | WRITE, true)
            .unwrap();
        memory.map_file_pages(0x30000, &pages, READ, false).unwrap();
        assert_eq!(calls.get(), 0);
        assert!(pages.iter().all(|page| !page.initialized()));
        let mut output = [0; 1];
        memory.read_into(0x30000, &mut output).unwrap();
        assert_eq!(output, [1]);
        assert_eq!(calls.get(), 1);
        memory.write(0x10000, &[7]).unwrap();
        memory.read_into(0x20000, &mut output).unwrap();
        assert_eq!(output, [7]);
        memory.write(0x20000, &[9]).unwrap();
        memory.write(0x10000, &[8]).unwrap();
        memory.read_into(0x20000, &mut output).unwrap();
        assert_eq!(output, [9]);
        assert_eq!(calls.get(), 1);
        assert!(!pages[1].initialized());
        memory.write(0x21000, &[0xab; 16]).unwrap();
        assert_eq!(calls.get(), 2);
        let mut original = [0; 16];
        memory.read_into(0x11000, &mut original).unwrap();
        assert_eq!(original, [2; 16]);
        assert!(!memory.rep_alias_hazard(0x10000, 0x20001, 32)); // private COPY page
        assert!(memory.rep_alias_hazard(0x10000, 0x30001, 32));
        assert!(!memory.rep_alias_hazard(0x10000, 0x30040, 32));
        assert!(memory.protect(0x10000, 4096, READ | EXEC).is_err());
        memory.map(0x40000, 8192, READ | WRITE).unwrap();
        memory.copy(0x10000, 0x40000, 8192).unwrap();
        let mut copied = [0; 8192];
        memory.read_into(0x40000, &mut copied).unwrap();
        assert_eq!(copied[0], 8);
        assert_eq!(copied[4096], 2);
        memory.fill(0x10000, &[0x55, 0xaa], 8192).unwrap();
        let mut shared = [0; 4];
        memory.read_into(0x31000, &mut shared).unwrap();
        assert_eq!(shared, [0x55, 0xaa, 0x55, 0xaa]);
        memory.unmap(0x10000, 8192).unwrap();
        memory.unmap(0x20000, 8192).unwrap();
        memory.read_into(0x31000, &mut shared).unwrap();
        assert_eq!(shared, [0x55, 0xaa, 0x55, 0xaa]);
    }

    #[test]
    fn failed_first_touch_is_a_memory_error_without_mutating_output() {
        let mut memory = Memory::reserve(0, 0x20000).unwrap();
        let page = crate::backend::FilePage::lazy(|| Err("disk read failure".into()));
        memory
            .map_file_pages(0x10000, &[page], READ | WRITE, false)
            .unwrap();
        let mut output = [0x55; 4];
        assert!(memory.read_into(0x10000, &mut output).is_err());
        assert_eq!(output, [0x55; 4]);
        assert!(memory.write(0x10000, &[0; 4]).is_err());
    }

    #[test]
    fn cross_page_store_prepares_all_io_and_cow_before_committing() {
        for copy in [false, true] {
            let fail = Rc::new(Cell::new(true));
            let injected = fail.clone();
            let first = crate::backend::FilePage::new([0x11; 4096]);
            let second = crate::backend::FilePage::lazy(move || {
                if injected.get() {
                    Err("transient second-page read failure".into())
                } else {
                    Ok([0x22; 4096])
                }
            });
            let mut memory = Memory::reserve(0, 0x30000).unwrap();
            memory
                .map_file_pages(
                    0x10000,
                    &[first.clone(), second.clone()],
                    READ | WRITE,
                    copy,
                )
                .unwrap();
            assert!(memory.write(0x10ffe, &[1, 2, 3, 4]).is_err());
            let mut prefix = [0; 2];
            memory.read_into(0x10ffe, &mut prefix).unwrap();
            assert_eq!(prefix, [0x11; 2]);
            assert_eq!(memory.file_page_copy_on_write(0x10000), Some(copy));
            assert_eq!(memory.file_page_copy_on_write(0x11000), Some(copy));
            assert!(!first.dirty());
            assert!(!second.dirty());
            fail.set(false);
            memory.write(0x10ffe, &[1, 2, 3, 4]).unwrap();
            let mut output = [0; 4];
            memory.read_into(0x10ffe, &mut output).unwrap();
            assert_eq!(output, [1, 2, 3, 4]);
            assert_eq!(memory.file_page_copy_on_write(0x10000), Some(false));
            assert_eq!(memory.file_page_copy_on_write(0x11000), Some(false));
            assert_eq!(
                &first.snapshot().unwrap()[4094..],
                if copy { &[0x11, 0x11] } else { &[1, 2] }
            );
        }
    }

    #[test]
    fn faulting_scalar_store_preserves_eip_and_retries_without_prefix_writes() {
        use crate::interpreter::{Interpreter, Step};
        let fail = Rc::new(Cell::new(true));
        let injected = fail.clone();
        let first = crate::backend::FilePage::new([0x55; 4096]);
        let second = crate::backend::FilePage::lazy(move || {
            if injected.get() {
                Err("transient read failure".into())
            } else {
                Ok([0x66; 4096])
            }
        });
        let mut memory = Memory::reserve(0, 0x30000).unwrap();
        memory.map(0x1000, 4096, READ | EXEC).unwrap();
        memory.write(0x1000, &[0xa3, 0xfe, 0x0f, 0x01, 0]).unwrap(); // mov [0x10ffe], eax
        memory
            .map_file_pages(0x10000, &[first, second], READ | WRITE, true)
            .unwrap();
        let mut state = crate::CpuState {
            eip: 0x1000,
            ..Default::default()
        };
        state.regs[0] = 0x04030201;
        let before = state.clone();
        let mut interpreter = Interpreter::new();
        assert_eq!(
            interpreter.step(&mut state, &mut memory),
            Step::WriteFault {
                pc: 0x1000,
                address: 0x10ffe
            }
        );
        assert_eq!(state, before);
        let mut prefix = [0; 2];
        memory.read_into(0x10ffe, &mut prefix).unwrap();
        assert_eq!(prefix, [0x55; 2]);
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(true));
        fail.set(false);
        assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
        assert_eq!(state.eip, 0x1005);
        let mut bytes = [0; 4];
        memory.read_into(0x10ffe, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 4]);
    }

    /// Stores a value through the native path, as compiled guest code does.
    fn native_store(memory: &Memory, address: u32, bytes: &[u8]) {
        let pointer = memory.direct(address, bytes.len(), true).unwrap();
        // SAFETY: `direct` checked the range lies in one ordinary page.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len()) };
    }

    // spec: A64-MEM-3
    #[test]
    fn first_native_access_makes_a_shared_view_resident_and_coherent() {
        let page = crate::backend::FilePage::lazy(|| Ok([0; PAGE_SIZE]));
        let mut memory = Memory::reserve(0, 0x30000).unwrap();
        for at in [0x10000, 0x20000] {
            memory
                .map_file_pages(at, &[page.clone()], READ | WRITE, false)
                .unwrap();
        }
        assert!(memory.shared_range(0x10000, 4));
        assert!(!page.initialized());

        memory.prepare_native_access(0x10000, 4, true).unwrap();
        assert!(!memory.shared_range(0x10000, PAGE_SIZE));
        assert!(memory.shared_range(0x20000, 4));
        assert_eq!(memory.file_page_copy_on_write(0x10000), Some(false));
        assert!(!page.dirty());
        // The other view reaches the bytes stored natively, and back.
        native_store(&memory, 0x10000, &[1, 2, 3, 4]);
        assert!(page.dirty());
        let mut bytes = [0; 5];
        memory.read_into(0x20000, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 4, 0]);
        memory.write(0x20004, &[5]).unwrap();
        memory.read_into(0x10000, &mut bytes).unwrap();
        assert_eq!(bytes, [1, 2, 3, 4, 5]);
        assert!(memory.rep_alias_hazard(0x10000, 0x20002, 4));
        assert!(!memory.rep_alias_hazard(0x10000, 0x20010, 4));
        page.clear_dirty();
        assert!(!page.dirty());
        native_store(&memory, 0x10006, &[6]);
        assert!(page.dirty());

        // Unmapping the resident view hands the bytes to the other one.
        memory.unmap(0x10000, PAGE_SIZE).unwrap();
        assert_eq!(&page.snapshot().unwrap()[..7], &[1, 2, 3, 4, 5, 0, 6]);
        memory.prepare_native_access(0x20000, 4, false).unwrap();
        assert!(!memory.shared_range(0x20000, PAGE_SIZE));
        native_store(&memory, 0x20007, &[7]);
        drop(memory);
        assert_eq!(&page.snapshot().unwrap()[..8], &[1, 2, 3, 4, 5, 0, 6, 7]);
        assert!(page.dirty());
    }

    #[test]
    fn copy_on_write_views_stay_overlays_and_unchanged_resident_pages_stay_clean() {
        let page = crate::backend::FilePage::new([0x17; PAGE_SIZE]);
        let mut memory = Memory::reserve(0, 0x30000).unwrap();
        memory
            .map_file_pages(0x10000, &[page.clone()], READ | WRITE, true)
            .unwrap();
        memory
            .map_file_pages(0x20000, &[page.clone()], READ, false)
            .unwrap();
        memory.prepare_native_access(0x10000, 4, false).unwrap();
        assert!(memory.shared_range(0x10000, 4));
        memory.prepare_native_access(0x20000, 4, false).unwrap();
        assert!(!memory.shared_range(0x20000, 4));
        assert!(memory.direct(0x20000, 4, true).is_none());
        // A COPY store detaches a private page, which then is resident too.
        memory.prepare_native_access(0x10000, 4, true).unwrap();
        assert!(!memory.shared_range(0x10000, 4));
        native_store(&memory, 0x10000, &[1]);
        let mut byte = [0];
        memory.read_into(0x20000, &mut byte).unwrap();
        assert_eq!(byte, [0x17]);
        drop(memory);
        assert!(!page.dirty());
        assert_eq!(page.snapshot().unwrap(), [0x17; PAGE_SIZE]);
    }
}

#[cfg(all(test, unix))]
mod window_tests {
    use super::*;
    use crate::virtual_memory::GuestAccess::{Read, Write};

    // spec: A64-MEM-2, A64-MEM-4
    #[test]
    fn the_guest_window_view_follows_guest_permissions_on_4k_and_16k_hosts() {
        let native = crate::virtual_memory::host_page_size().unwrap();
        for host in [4096, 16384].into_iter().filter(|host| host % native == 0) {
            // [0x3000, 0x10000): on 16K hosts the first host page also holds
            // three slack pages below the guest base.
            let mut memory = Memory::window_at_host_size(0x3000, 0xd000, host).unwrap();
            memory.map(0x3000, 0xd000, 0).unwrap();
            memory.write(0x8000, b"window").unwrap();
            memory.protect(0x3000, 0x1000, 3).unwrap();
            memory.protect(0x4000, 0x4000, 3).unwrap();
            memory.protect(0x8000, 0x4000, 1).unwrap();
            memory.protect(0xc000, 0x4000, 3).unwrap();
            memory.watch_code(0xc000, 1);
            let window = memory.window_base().unwrap();
            let reservation = memory.reservation.as_ref().unwrap();
            let start = reservation.ptr.as_ptr() as usize - window;
            let view = reservation.direct_fault_view().unwrap();
            let allows = |addr: usize, access| view.window_allows(addr - start, 0x1000, access);
            assert_eq!(
                allows(0x3000, Write),
                host == 4096,
                "{host}: a mixed host page is soft"
            );
            assert!(allows(0x4000, Write) && allows(0x7000, Write), "{host}");
            assert!(allows(0x8000, Read) && !allows(0x8000, Write), "{host}");
            assert!(
                allows(0xc000, Read) && !allows(0xc000, Write),
                "{host}: watched code"
            );
            // SAFETY: the page is readable in the window view.
            let bytes = unsafe { std::slice::from_raw_parts((window + 0x8000) as *const u8, 6) };
            assert_eq!(bytes, b"window");
            drop(view);
            memory.protect(0x4000, 0x4000, 0).unwrap();
            memory.unmap(0x8000, 0x4000).unwrap();
            let reservation = memory.reservation.as_ref().unwrap();
            let view = reservation.direct_fault_view().unwrap();
            assert!(!view.window_allows(0x4000 - start, 0x1000, Read), "{host}");
            assert!(!view.window_allows(0x8000 - start, 0x1000, Read), "{host}");
            drop(view);
            memory.map(0x8000, 0x4000, 1).unwrap();
            assert_eq!(
                memory.read(0x8000, 6).unwrap(),
                [0; 6],
                "{host}: fresh pages are zero"
            );
        }
    }

    /// Native code takes a page directly only where its host page lets it:
    /// on 16K hosts a mixed or code-watched host page sends the accesses its
    /// window refuses through `Memory` instead of faulting on each one.
    // spec: A64-MEM-3, A64-MEM-8
    #[test]
    fn native_permissions_follow_the_window_host_page() {
        let native = crate::virtual_memory::host_page_size().unwrap();
        for host in [4096, 16384].into_iter().filter(|host| host % native == 0) {
            let mut memory = Memory::window_at_host_size(0x10000, 0x8000, host).unwrap();
            memory.map(0x10000, 0x8000, READ | WRITE).unwrap();
            // [0x10000, 0x14000): one guest page is read-only, one is inaccessible.
            memory.protect(0x10000, PAGE_SIZE, READ).unwrap();
            memory.protect(0x13000, PAGE_SIZE, 0).unwrap();
            // [0x14000, 0x18000): code is observed on one page.
            memory.watch_code(0x14000, 1);
            let classes = memory.native_permissions();
            let mixed = host == 16384;
            assert_eq!(
                classes[1] & 7,
                READ | WRITE | if mixed { EXEC } else { 0 },
                "{host}"
            );
            assert_eq!(
                classes[1] & SHARED != 0,
                mixed,
                "{host}: unreadable host page"
            );
            assert_eq!(
                classes[5] & 7,
                READ | WRITE | if mixed { EXEC } else { 0 },
                "{host}"
            );
            assert_eq!(classes[5] & SHARED, 0, "{host}");
            // Restoring the neighbour refreshes every guest page of its host page.
            memory.protect(0x13000, PAGE_SIZE, READ | WRITE).unwrap();
            let classes = memory.native_permissions();
            assert_eq!(classes[1] & SHARED, 0, "{host}");
            assert_eq!(
                classes[2] & 7,
                READ | WRITE | if mixed { EXEC } else { 0 },
                "{host}"
            );
        }
    }

    /// The native permission snapshot lets translated code read owned
    /// private copies directly, so the window must too; stores still trap
    /// until the copy is privatized.
    #[test]
    fn the_window_reads_owned_private_copies_and_traps_their_stores() {
        let native = crate::virtual_memory::host_page_size().unwrap();
        for host in [4096, 16384].into_iter().filter(|host| host % native == 0) {
            // One host page of copies, so only their own state sets its protection.
            let mut memory = Memory::window_at_host_size(0x10000, 0x4000, host).unwrap();
            memory
                .map_private_copy_pages(0x10000, &vec![0x37; host], READ | WRITE)
                .unwrap();
            let window = memory.window_base().unwrap();
            let reservation = memory.reservation.as_ref().unwrap();
            let start = reservation.ptr.as_ptr() as usize - window;
            let allows = |memory: &Memory, addr: usize, access| {
                let reservation = memory.reservation.as_ref().unwrap();
                let view = reservation.direct_fault_view().unwrap();
                view.window_allows(addr - start, PAGE_SIZE, access)
            };
            assert!(
                allows(&memory, 0x10000, Read) && !allows(&memory, 0x10000, Write),
                "{host}"
            );
            memory.privatize_copy_pages(0x10000, PAGE_SIZE).unwrap();
            // A host page with a copy left still traps every store.
            assert_eq!(allows(&memory, 0x10000, Write), host == 4096, "{host}");
            assert!(
                allows(&memory, 0x11000, Read) && !allows(&memory, 0x11000, Write),
                "{host}"
            );
            memory.privatize_copy_pages(0x10000, host).unwrap();
            assert!(allows(&memory, 0x10000, Write), "{host}");
        }
    }
}
