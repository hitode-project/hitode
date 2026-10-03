use crate::shared_memory::SharedAllocation;
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ptr::NonNull;
use std::sync::Arc;

pub const PAGE_SIZE: usize = 4096;
pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const EXEC: u8 = 4;
/// Internal execution barrier; never exposed as a guest protection flag.
pub(crate) const SHARED: u8 = 8;

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
    shared_pages: BTreeMap<usize, (Arc<SharedAllocation>, usize)>,
}

impl Memory {
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
        let reservation = crate::virtual_memory::Reservation::new(len)?;
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
        self.protect(addr, len, perms)
    }

    pub fn unmap(&mut self, addr: u32, len: usize) -> Result<(), String> {
        let pages = self.pages(addr, len)?;
        self.committed(addr, len)?;
        let reservation = self
            .reservation
            .as_ref()
            .ok_or("unmapping requires reserved address space")?;
        // Overlay pages never committed a page in the linear reservation. Only
        // decommit ordinary spans; removing an overlay leaves its Arc alias live.
        let mut start = pages.start;
        while start < pages.end {
            if self.shared_pages.contains_key(&start) {
                start += 1;
                continue;
            }
            let mut end = start + 1;
            while end < pages.end && !self.shared_pages.contains_key(&end) {
                end += 1;
            }
            reservation.decommit(start * PAGE_SIZE, (end - start) * PAGE_SIZE)?;
            start = end;
        }
        for page in pages.clone() {
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
        // SAFETY: committed ordinary storage remains immutable for this borrow.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(range.start), range.len()) })
    }

    pub fn read_data(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        let range = self.committed(addr, len)?;
        if self.overlays(&range) {
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
            if let Some((storage, offset)) = self.shared_pages.get(&page) {
                for (index, byte) in output.iter_mut().enumerate() {
                    *byte = storage.load_u8(offset + at % PAGE_SIZE + index);
                }
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

    #[cfg_attr(not(feature = "pal"), allow(dead_code))]
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
        for (index, page) in pages.clone().enumerate() {
            self.shared_pages
                .insert(page, (storage.clone(), index * PAGE_SIZE));
        }
        self.mapped[pages.clone()].fill(true);
        self.perms[pages].fill(READ | WRITE | SHARED);
        self.touch(addr, len);
        Ok(storage)
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
        if !range.is_empty()
            && self.perms[range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE]
                .iter()
                .any(|p| p & EXEC != 0)
        {
            self.touch(addr, bytes.len());
        }
        if self.overlays(&range) {
            let mut at = range.start;
            while at < range.end {
                let page = at / PAGE_SIZE;
                let len = (PAGE_SIZE - at % PAGE_SIZE).min(range.end - at);
                let input = &bytes[at - range.start..at - range.start + len];
                if let Some((storage, offset)) = self.shared_pages.get(&page) {
                    for (index, &byte) in input.iter().enumerate() {
                        storage.store_u8(offset + at % PAGE_SIZE + index, byte);
                    }
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
        for page in &mut self.perms[range.start / PAGE_SIZE..range.end / PAGE_SIZE] {
            *page = perms | (*page & SHARED);
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
