use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::BTreeSet;
use std::ptr::NonNull;

pub const PAGE_SIZE: usize = 4096;
pub const READ: u8 = 1;
pub const WRITE: u8 = 2;
pub const EXEC: u8 = 4;

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
        self.reservation
            .as_ref()
            .ok_or("unmapping requires reserved address space")?
            .decommit(pages.start * PAGE_SIZE, len)?;
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

    pub fn read(&self, addr: u32, len: usize) -> Result<&[u8], String> {
        let range = self.committed(addr, len)?;
        // SAFETY: bounds checked, allocation alive, shared borrow prevents executor writes.
        Ok(unsafe { std::slice::from_raw_parts(self.ptr.as_ptr().add(range.start), range.len()) })
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
        self.perms[range.start / PAGE_SIZE..range.end / PAGE_SIZE].fill(perms);
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
