//! Physical pins remain with the existing private page owner.
use std::ops::Range;

use crate::backend::PageLockError;

use super::{Memory, PAGE_SIZE};

impl Memory {
    pub fn retire_locked_pages(&mut self) -> Result<(), PageLockError> {
        if !self.locked_pages.is_empty() {
            self.quiesce_workers();
        }
        self.retire_page_locks(0..self.len / PAGE_SIZE).map(|_| ())
    }

    pub fn has_locked_pages(&self, addr: u32, len: u32) -> bool {
        self.range(addr, len as usize).is_ok_and(|range| {
            !range.is_empty()
                && self
                    .locked_pages
                    .range(range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE)
                    .next()
                    .is_some()
        })
    }

    #[cfg(windows)]
    fn pinnable_pages(&self, addr: u32, len: u32) -> Result<Range<usize>, PageLockError> {
        if self.parallel_view || self.reservation.is_none() {
            return Err(PageLockError::Unsupported);
        }
        let pages = self
            .pages(addr, len as usize)
            .map_err(|_| PageLockError::Win32(87))?;
        // Resident file bytes can move to another view; SHARED alone is not
        // sufficient to prove an ordinary private allocation.
        if self.shared_pages.range(pages.clone()).next().is_some()
            || self.resident.range(pages.clone()).next().is_some()
        {
            return Err(PageLockError::Unsupported);
        }
        Ok(pages)
    }

    /// Lock only committed, accessible private Windows reservation pages.
    pub fn lock_pages(&mut self, addr: u32, len: u32) -> Result<(), PageLockError> {
        #[cfg(windows)]
        {
            let pages = self.pinnable_pages(addr, len)?;
            self.quiesce_workers();
            if self.committed(addr, len as usize).is_err()
                || self.perms[pages.clone()].iter().any(|p| p & 7 == 0)
            {
                return Err(PageLockError::Win32(998));
            }
            // Host VirtualLock may fail partway through a large request. Only
            // newly acquired pages belong to this transaction's rollback.
            let mut acquired = Vec::new();
            for page in pages {
                if self.locked_pages.contains(&page) {
                    continue;
                }
                match self
                    .reservation
                    .as_ref()
                    .unwrap()
                    .lock_pages(page * PAGE_SIZE, PAGE_SIZE)
                {
                    Ok(()) => {
                        self.locked_pages.insert(page);
                        acquired.push(page);
                    }
                    Err(error) => {
                        for page in acquired {
                            if self
                                .reservation
                                .as_ref()
                                .unwrap()
                                .unlock_pages(page * PAGE_SIZE, PAGE_SIZE)
                                .is_ok()
                            {
                                self.locked_pages.remove(&page);
                            }
                            // An unsuccessful rollback remains owned and will
                            // be retired by later unlock/unmap/window release.
                        }
                        return Err(PageLockError::Win32(error));
                    }
                }
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (addr, len);
            Err(PageLockError::Unsupported)
        }
    }

    pub fn unlock_pages(&mut self, addr: u32, len: u32) -> Result<(), PageLockError> {
        #[cfg(windows)]
        {
            let pages = self.pinnable_pages(addr, len)?;
            self.quiesce_workers();
            // Do not require READ access: NOACCESS/uncommitted and unlocked
            // pages report NOT_LOCKED. The host owns this entire address range.
            match self.reservation.as_ref().unwrap().unlock_pages(
                pages.start * PAGE_SIZE,
                (pages.end - pages.start) * PAGE_SIZE,
            ) {
                Ok(()) => {
                    self.locked_pages.retain(|page| !pages.contains(page));
                    Ok(())
                }
                // Native mixed locked/unlocked failure preserves the locked
                // members, while evicting only the unlocked working-set pages.
                Err(error) => Err(PageLockError::Win32(error)),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (addr, len);
            Err(PageLockError::Unsupported)
        }
    }

    pub(super) fn retire_page_locks(
        &mut self,
        pages: Range<usize>,
    ) -> Result<Vec<usize>, PageLockError> {
        let owned: Vec<_> = self.locked_pages.range(pages).copied().collect();
        #[cfg(windows)]
        {
            let mut retired = Vec::new();
            for page in owned {
                let result = self
                    .reservation
                    .as_ref()
                    .ok_or(PageLockError::Unsupported)?
                    .unlock_pages(page * PAGE_SIZE, PAGE_SIZE);
                if let Err(error) = result {
                    let _ = self.restore_page_locks(&retired);
                    return Err(PageLockError::Win32(error));
                }
                self.locked_pages.remove(&page);
                retired.push(page);
            }
            Ok(retired)
        }
        #[cfg(not(windows))]
        {
            debug_assert!(owned.is_empty());
            Ok(Vec::new())
        }
    }

    #[cfg(windows)]
    pub(super) fn restore_page_locks(&mut self, pages: &[usize]) -> Result<(), PageLockError> {
        let mut failure = None;
        for &page in pages {
            let result = self
                .reservation
                .as_ref()
                .ok_or(PageLockError::Unsupported)?
                .lock_pages(page * PAGE_SIZE, PAGE_SIZE);
            match result {
                Ok(()) => {
                    self.locked_pages.insert(page);
                }
                Err(error) => {
                    failure.get_or_insert(PageLockError::Win32(error));
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::c_void;

    #[repr(C)]
    struct WorkingSet {
        address: usize,
        attributes: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn K32QueryWorkingSetEx(process: *mut c_void, data: *mut WorkingSet, size: u32) -> i32;
    }
    fn physically_locked(memory: &Memory) -> bool {
        let mut page = WorkingSet {
            address: memory.ptr.as_ptr() as usize,
            attributes: 0,
        };
        assert_ne!(
            unsafe {
                K32QueryWorkingSetEx(
                    GetCurrentProcess(),
                    &mut page,
                    std::mem::size_of::<WorkingSet>() as u32,
                )
            },
            0
        );
        page.attributes & 1 != 0 && page.attributes & (1 << 22) != 0
    }

    #[test]
    fn pcm_private_promotion_refuses_pinned_neighbours_without_changing_bytes() {
        let mut memory = Memory::reserve(0, 65536).unwrap();
        memory
            .map(0, 8192, super::super::READ | super::super::WRITE)
            .unwrap();
        memory.write(0, &[0x5a, 0xa5]).unwrap();
        memory.lock_pages(0, 4096).unwrap();
        assert!(physically_locked(&memory));
        memory.unmap(4096, 4096).unwrap();
        assert!(memory.map_shared_audio(4096, 4096).is_err());
        assert!(!memory.mapped[1]);
        assert!(memory.has_locked_pages(0, 4096));
        assert!(physically_locked(&memory));
        assert_eq!(memory.read(0, 2).unwrap(), &[0x5a, 0xa5]);
        memory.unlock_pages(0, 4096).unwrap();
        memory.map_shared_audio(4096, 4096).unwrap();
        assert_eq!(
            memory.lock_pages(4096, 4096),
            Err(PageLockError::Unsupported)
        );
        assert_eq!(memory.read(0, 2).unwrap(), &[0x5a, 0xa5]);
    }
}
