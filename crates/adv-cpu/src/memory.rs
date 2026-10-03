use crate::shared_memory::SharedAllocation;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub const PAGE_SIZE: usize = 4096;
pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const EXEC: u8 = 4;
/// Internal execution barrier; never exposed as a guest protection flag.
pub(crate) const SHARED: u8 = 8;
/// A shared PCM page has a stable alias at base + guest offset. Access must
/// still use only matching byte atomics, even for guest word/vector operations.
pub(crate) const AUDIO: u8 = 16;

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
/// Resizing and host worker writes are deliberately excluded from this prototype.
pub struct Memory {
    pub(crate) base: u32,
    pub(crate) len: usize,
    pub(crate) ptr: NonNull<u8>,
    pub(crate) perms: Vec<u8>,
    pub(crate) epoch: u64,
    pub(crate) versions: Vec<u64>,
    pub(crate) mapped: Vec<bool>,
    pub(crate) dirty_pages: BTreeSet<u32>,
    reservation: Option<crate::virtual_memory::Reservation>,
    shared_pages: BTreeMap<usize, SharedPage>,
    #[cfg(windows)]
    native_protection: bool,
    #[cfg(windows)]
    native_read_exceptions: usize,
}

impl Memory {
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
            Ok(())
        }
    }

    /// Windows cannot express guest data-read denial on write-only or
    /// execute-only pages. Native sites must retain software READ checks while
    /// any ordinary page has that shape. Short address windows keep bounds.
    #[allow(dead_code)] // Used by verified native sites in the next compiler handoff.
    pub(crate) fn native_fault_reads_available(&self) -> bool {
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
        Ok(Self {
            base,
            len,
            ptr,
            perms: vec![0; len / PAGE_SIZE],
            epoch: 0,
            versions: vec![0; len / PAGE_SIZE],
            mapped: vec![true; len / PAGE_SIZE],
            dirty_pages: BTreeSet::new(),
            reservation: None,
            shared_pages: BTreeMap::new(),
            #[cfg(windows)]
            native_protection: false,
            #[cfg(windows)]
            native_read_exceptions: 0,
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
        Ok(Self {
            base,
            len,
            ptr: reservation.ptr,
            perms: vec![0; len / PAGE_SIZE],
            epoch: 0,
            versions: vec![0; len / PAGE_SIZE],
            mapped: vec![false; len / PAGE_SIZE],
            dirty_pages: BTreeSet::new(),
            reservation: Some(reservation),
            shared_pages: BTreeMap::new(),
            #[cfg(windows)]
            native_protection: false,
            #[cfg(windows)]
            native_read_exceptions: 0,
        })
    }

    pub(crate) fn pages(&self, addr: u32, len: usize) -> Result<std::ops::Range<usize>, String> {
        if !addr.is_multiple_of(4096) || len == 0 || !len.is_multiple_of(4096) {
            return Err("unaligned page range".into());
        }
        let r = self.range(addr, len)?;
        Ok(r.start / PAGE_SIZE..r.end / PAGE_SIZE)
    }

    pub(crate) fn committed(
        &self,
        addr: u32,
        len: usize,
    ) -> Result<std::ops::Range<usize>, String> {
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
        self.reservation
            .as_ref()
            .ok_or("mapping requires reserved address space")?
            .commit(pages.start * PAGE_SIZE, len)?;
        self.mapped[pages.clone()].fill(true);
        if let Err(error) = self.protect(addr, len, perms) {
            self.reservation
                .as_ref()
                .unwrap()
                .decommit(pages.start * PAGE_SIZE, len)?;
            self.mapped[pages].fill(false);
            return Err(error);
        }
        Ok(())
    }

    pub fn unmap(&mut self, addr: u32, len: usize) -> Result<(), String> {
        let pages = self.pages(addr, len)?;
        self.committed(addr, len)?;
        let reservation = self
            .reservation
            .as_ref()
            .ok_or("unmapping requires reserved address space")?;
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
            reservation.decommit(start * PAGE_SIZE, (end - start) * PAGE_SIZE)?;
            start = end;
        }
        for page in pages.clone() {
            #[cfg(windows)]
            if Self::native_read_exception(self.perms[page]) {
                self.native_read_exceptions -= 1;
            }
            self.shared_pages.remove(&page);
        }
        self.mapped[pages.clone()].fill(false);
        self.perms[pages].fill(0);
        self.touch(addr, len);
        Ok(())
    }

    pub(crate) fn touch(&mut self, addr: u32, len: usize) {
        if len == 0 {
            return;
        }
        let start = (addr - self.base) as usize / PAGE_SIZE;
        let last = ((addr - self.base) as usize + len - 1) / PAGE_SIZE;
        self.epoch += 1;
        for version in &mut self.versions[start..=last] {
            *version = self.epoch;
        }
        for page in start..=last {
            self.dirty_pages
                .insert(self.base + page as u32 * PAGE_SIZE as u32);
        }
    }

    pub(crate) fn range(&self, addr: u32, len: usize) -> Result<std::ops::Range<usize>, String> {
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

    fn overlays(&self, range: &std::ops::Range<usize>) -> bool {
        !range.is_empty()
            && self.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE]
                .iter()
                .any(|p| p & SHARED != 0)
    }

    pub(crate) fn shared_range(&self, address: u32, len: usize) -> bool {
        self.range(address, len)
            .is_ok_and(|range| self.overlays(&range))
    }

    /// Distinct guest views can overlap physically. REP must retain its element
    /// order (including propagation) instead of snapshotting such bytes.
    pub(crate) fn rep_alias_hazard(&self, source: u32, destination: u32, len: usize) -> bool {
        let (Ok(src), Ok(dst)) = (self.range(source, len), self.range(destination, len)) else {
            return true;
        };
        if !self.overlays(&src) || !self.overlays(&dst) {
            return false;
        }
        if len == 0 {
            return false;
        }
        let mut source_spans = std::collections::HashMap::<(u8, usize), Vec<(usize, usize)>>::new();
        for (page, storage) in self
            .shared_pages
            .range(src.start / PAGE_SIZE..=(src.end - 1) / PAGE_SIZE)
        {
            let (kind, id, base) = storage.identity();
            let begin = src.start.max(page * PAGE_SIZE) - page * PAGE_SIZE + base;
            let end = src.end.min((page + 1) * PAGE_SIZE) - page * PAGE_SIZE + base;
            source_spans
                .entry((kind, id))
                .or_default()
                .push((begin, end));
        }
        for (page, storage) in self
            .shared_pages
            .range(dst.start / PAGE_SIZE..=(dst.end - 1) / PAGE_SIZE)
        {
            let (kind, id, base) = storage.identity();
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

    pub(crate) fn map_shared_audio(
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
        self.mapped[pages.clone()].fill(true);
        self.perms[pages].fill(READ | WRITE | SHARED | if cfg!(windows) { AUDIO } else { 0 });
        self.touch(addr, len);
        Ok(storage)
    }

    pub(crate) fn map_file_pages(
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
        self.mapped[pages.clone()].fill(true);
        self.perms[pages].fill(perms | SHARED);
        self.touch(addr, len);
        Ok(())
    }
    pub(crate) fn file_page_copy_on_write(&self, addr: u32) -> Option<bool> {
        let page = addr.checked_sub(self.base)? as usize / PAGE_SIZE;
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
            if write && permissions.iter().any(|p| p & EXEC != 0) {
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
        Ok(())
    }

    pub(crate) fn copy(&mut self, source: u32, destination: u32, len: usize) -> Result<(), String> {
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
        if !dst.is_empty()
            && self.perms[dst.start / PAGE_SIZE..=(dst.end - 1) / PAGE_SIZE]
                .iter()
                .any(|p| p & EXEC != 0)
        {
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

    pub(crate) fn fill(&mut self, address: u32, pattern: &[u8], len: usize) -> Result<(), String> {
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
        if !range.is_empty()
            && self.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE]
                .iter()
                .any(|p| p & EXEC != 0)
        {
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
            && self
                .shared_pages
                .range(range.start / PAGE_SIZE..range.end / PAGE_SIZE)
                .any(|(_, page)| page.file_page())
        {
            return Err("executable file views are not supported".into());
        }
        #[cfg(windows)]
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
        for page in &mut self.perms[range.start / PAGE_SIZE..range.end / PAGE_SIZE] {
            #[cfg(windows)]
            {
                self.native_read_exceptions -= usize::from(Self::native_read_exception(*page));
                self.native_read_exceptions += usize::from(Self::native_read_exception(
                    perms | (*page & (SHARED | AUDIO)),
                ));
            }
            *page = perms | (*page & (SHARED | AUDIO));
        }
        self.touch(addr, len);
        Ok(())
    }

    pub(crate) fn executable(&self, addr: u32, len: usize) -> bool {
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

#[cfg(all(test, windows))]
mod linear_audio_tests {
    use super::*;
    #[repr(C)]
    #[derive(Default)]
    struct MemoryInfo {
        base: usize,
        allocation: usize,
        allocation_protection: u32,
        partition: u16,
        region: usize,
        state: u32,
        protection: u32,
        kind: u32,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualQuery(
            address: *const std::ffi::c_void,
            info: *mut std::ffi::c_void,
            len: usize,
        ) -> usize;
    }
    fn query(memory: &Memory, address: u32) -> MemoryInfo {
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
    fn audio_read_denial_keeps_software_read_check_in_production_native_code() {
        let mut executor = crate::Executor::address_space().unwrap();
        let code = [0xa1, 0, 0, 2, 0, 0xc3];
        executor
            .memory
            .map(0x10000, PAGE_SIZE, READ | EXEC)
            .unwrap();
        executor.memory.write(0x10000, &code).unwrap();
        let pcm = executor
            .memory
            .map_shared_audio(0x20000, PAGE_SIZE)
            .unwrap();
        executor.memory.enable_native_protection().unwrap();
        executor.memory.protect(0x20000, PAGE_SIZE, WRITE).unwrap();
        pcm.store_u8(0, 0x91);
        executor
            .add_region(crate::Region::new(0x10000, code.len()))
            .unwrap();
        executor.state.eip = 0x10000;
        executor.state.regs[0] = 0x12345678;
        assert_eq!(
            executor.run(0x11000, 1000).unwrap(),
            crate::RunOutcome::ReadFault {
                pc: 0x10000,
                address: 0x20000
            }
        );
        assert_eq!(executor.state.regs[0], 0x12345678);
        assert_eq!(pcm.load_u8(0), 0x91);
        // The capability only covers ordinary pages. SHARED classification and
        // its READ checks remain mandatory before choosing any raw host load.
        assert!(executor.memory.native_fault_reads_available());
    }

    #[test]
    fn production_jit_with_native_permissions_keeps_smc_and_precise_checked_faults() {
        use iced_x86::code_asm::*;
        let code_at = 0x10000;
        let data_at = 0x20000;
        let stack_at = 0x30ff0;
        let stop = 0x11000u32;
        let mut assembler = CodeAssembler::new(32).unwrap();
        assembler.mov(eax, dword_ptr(data_at)).unwrap();
        assembler.inc(eax).unwrap();
        assembler.mov(dword_ptr(code_at + 0x100), eax).unwrap();
        assembler.ret().unwrap();
        let code = assembler.assemble(code_at as u64).unwrap();
        let mut states = Vec::new();
        for native in [false, true] {
            let mut executor = crate::Executor::address_space().unwrap();
            executor
                .memory
                .map(code_at as u32, PAGE_SIZE * 2, READ | WRITE | EXEC)
                .unwrap();
            executor
                .memory
                .map(data_at as u32, PAGE_SIZE, READ | WRITE)
                .unwrap();
            executor
                .memory
                .map(stack_at & !4095, PAGE_SIZE, READ | WRITE)
                .unwrap();
            executor.memory.write(code_at as u32, &code).unwrap();
            executor
                .memory
                .write(data_at as u32, &0x1234u32.to_le_bytes())
                .unwrap();
            executor
                .memory
                .write(stack_at, &stop.to_le_bytes())
                .unwrap();
            executor.memory.enable_native_protection().unwrap();
            executor.state.eip = code_at as u32;
            executor.state.regs[4] = stack_at;
            if native {
                executor
                    .add_region(crate::Region::new(code_at as u32, code.len()))
                    .unwrap();
            }
            assert_eq!(
                executor.run(stop, 1000).unwrap(),
                crate::RunOutcome::ReachedStop
            );
            assert_eq!(
                executor.memory.read(code_at as u32 + 0x100, 4).unwrap(),
                &0x1235u32.to_le_bytes()
            );
            assert_eq!(query(&executor.memory, code_at as u32).protection, 2);
            states.push((
                executor.state.regs,
                executor.state.eflags,
                executor.state.eip,
            ));
            executor
                .memory
                .protect(data_at as u32, PAGE_SIZE, 0)
                .unwrap();
            executor.state.eip = code_at as u32;
            let before = executor.state.regs;
            assert_eq!(
                executor.run(stop, 1000).unwrap(),
                crate::RunOutcome::ReadFault {
                    pc: code_at as u32,
                    address: data_at as u32
                }
            );
            assert_eq!(executor.state.eip, code_at as u32);
            assert_eq!(executor.state.regs, before);
            assert_eq!(executor.state.fault_addr, data_at as u32);
        }
        assert_eq!(states[0], states[1]);
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
}
