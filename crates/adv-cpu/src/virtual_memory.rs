//! Stable host virtual address reservation. Guest permissions remain in our page table;
//! committed host pages are RW so loaders can populate guest RX pages safely.
use std::ptr::NonNull;
#[cfg(windows)]
use std::{cell::RefCell, collections::BTreeMap};

#[cfg(windows)]
mod os {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn VirtualAlloc(
            address: *mut c_void,
            size: usize,
            kind: u32,
            protection: u32,
        ) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32;
    }
    #[link(name = "onecore")]
    unsafe extern "system" {
        fn VirtualAlloc2(
            process: *mut c_void,
            address: *mut c_void,
            size: usize,
            kind: u32,
            protection: u32,
            parameters: *const c_void,
            count: u32,
        ) -> *mut c_void;
        fn MapViewOfFile3(
            mapping: *mut c_void,
            process: *mut c_void,
            address: *mut c_void,
            offset: u64,
            size: usize,
            kind: u32,
            protection: u32,
            parameters: *const c_void,
            count: u32,
        ) -> *mut c_void;
        fn UnmapViewOfFile2(process: *mut c_void, address: *const c_void, flags: u32) -> i32;
    }
    pub unsafe fn reserve(size: usize) -> *mut u8 {
        unsafe {
            VirtualAlloc2(
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                size,
                0x2000 | 0x40000,
                1,
                std::ptr::null(),
                0,
            )
            .cast()
        }
    }
    pub unsafe fn commit(p: *mut u8, size: usize) -> bool {
        unsafe { !VirtualAlloc(p.cast(), size, 0x1000, 4).is_null() }
    }
    pub unsafe fn decommit(p: *mut u8, size: usize) -> bool {
        unsafe { VirtualFree(p.cast(), size, 0x4000) != 0 }
    }
    pub unsafe fn release(p: *mut u8, _: usize) {
        unsafe {
            VirtualFree(p.cast(), 0, 0x8000);
        }
    }
    pub unsafe fn split(p: *mut u8, size: usize) -> bool {
        unsafe { VirtualFree(p.cast(), size, 0x8000 | 2) != 0 }
    }
    pub unsafe fn coalesce(p: *mut u8, size: usize) -> bool {
        unsafe { VirtualFree(p.cast(), size, 0x8000 | 1) != 0 }
    }
    pub unsafe fn private(p: *mut u8, size: usize, commit: bool) -> bool {
        unsafe {
            VirtualAlloc2(
                std::ptr::null_mut(),
                p.cast(),
                size,
                0x2000 | 0x4000 | if commit { 0x1000 } else { 0 },
                4,
                std::ptr::null(),
                0,
            ) == p.cast()
        }
    }
    pub unsafe fn section(p: *mut u8, handle: *mut c_void, offset: usize, size: usize) -> bool {
        unsafe {
            MapViewOfFile3(
                handle,
                std::ptr::null_mut(),
                p.cast(),
                offset as u64,
                size,
                0x4000,
                4,
                std::ptr::null(),
                0,
            ) == p.cast()
        }
    }
    pub unsafe fn unmap_section(p: *mut u8) -> bool {
        unsafe { UnmapViewOfFile2((-1isize) as *mut c_void, p.cast(), 2) != 0 }
    }
}
#[cfg(unix)]
mod os {
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
        unsafe {
            libc::mprotect(p.cast(), size, libc::PROT_NONE) == 0
                && libc::madvise(p.cast(), size, libc::MADV_DONTNEED) == 0
        }
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
    #[cfg(windows)]
    regions: RefCell<BTreeMap<usize, Region>>,
}
#[cfg(windows)]
struct Region {
    len: usize,
    kind: Kind,
}
#[cfg(windows)]
enum Kind {
    Placeholder,
    Private(Vec<bool>),
    Section,
}
impl Reservation {
    pub fn new(len: usize) -> Result<Self, String> {
        if len == 0 || !len.is_multiple_of(4096) {
            return Err("reservation must be page aligned and nonempty".into());
        }
        let ptr = NonNull::new(unsafe { os::reserve(len) })
            .ok_or_else(|| format!("reserve guest memory: {}", std::io::Error::last_os_error()))?;
        Ok(Self {
            ptr,
            len,
            #[cfg(windows)]
            regions: RefCell::new(BTreeMap::from([(
                0,
                Region {
                    len,
                    kind: Kind::Placeholder,
                },
            )])),
        })
    }
    pub fn commit(&self, offset: usize, len: usize) -> Result<(), String> {
        #[cfg(windows)]
        return self.commit_windows(offset, len);
        #[cfg(not(windows))]
        {
            if offset.checked_add(len).is_none_or(|n| n > self.len) {
                return Err("commit outside reservation".into());
            }
            if unsafe { os::commit(self.ptr.as_ptr().add(offset), len) } {
                Ok(())
            } else {
                Err(format!(
                    "commit guest memory: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }
    }
    pub fn decommit(&self, offset: usize, len: usize) -> Result<(), String> {
        #[cfg(windows)]
        return self.decommit_windows(offset, len);
        #[cfg(not(windows))]
        {
            if offset.checked_add(len).is_none_or(|n| n > self.len) {
                return Err("decommit outside reservation".into());
            }
            if unsafe { os::decommit(self.ptr.as_ptr().add(offset), len) } {
                Ok(())
            } else {
                Err(format!(
                    "decommit guest memory: {}",
                    std::io::Error::last_os_error()
                ))
            }
        }
    }
}

#[cfg(windows)]
impl Reservation {
    fn bounds(&self, offset: usize, len: usize) -> Result<usize, String> {
        if !offset.is_multiple_of(4096) || len == 0 || !len.is_multiple_of(4096) {
            return Err("unaligned reservation operation".into());
        }
        offset
            .checked_add(len)
            .filter(|&end| end <= self.len)
            .ok_or_else(|| "outside reservation".into())
    }

    fn split_at(&self, regions: &mut BTreeMap<usize, Region>, at: usize) -> Result<(), String> {
        if at == self.len {
            return Ok(());
        }
        let (&start, region) = regions
            .range(..=at)
            .next_back()
            .ok_or("missing reservation region")?;
        let end = start + region.len;
        if at == start {
            return Ok(());
        }
        if at >= end || !matches!(region.kind, Kind::Placeholder) {
            return Err("cannot split an occupied reservation".into());
        }
        if !unsafe { os::split(self.ptr.as_ptr().add(start), at - start) } {
            return Err(format!(
                "split guest placeholder: {}",
                std::io::Error::last_os_error()
            ));
        }
        regions.get_mut(&start).unwrap().len = at - start;
        regions.insert(
            at,
            Region {
                len: end - at,
                kind: Kind::Placeholder,
            },
        );
        Ok(())
    }

    fn spans(
        regions: &BTreeMap<usize, Region>,
        start: usize,
        end: usize,
    ) -> Vec<(usize, usize, usize)> {
        let mut spans = Vec::new();
        let mut at = start;
        while at < end {
            let (&base, region) = regions.range(..=at).next_back().unwrap();
            let next = end.min(base + region.len);
            spans.push((base, at, next));
            at = next;
        }
        spans
    }

    fn commit_regions(
        &self,
        regions: &mut BTreeMap<usize, Region>,
        offset: usize,
        end: usize,
    ) -> Result<(), String> {
        for (base, start, stop) in Self::spans(regions, offset, end) {
            match &regions[&base].kind {
                Kind::Placeholder => {}
                Kind::Private(pages)
                    if pages[(start - base) / 4096..(stop - base) / 4096]
                        .iter()
                        .all(|p| !p) => {}
                _ => return Err("overlapping reservation commit".into()),
            }
        }
        if matches!(
            regions.range(..=offset).next_back().unwrap().1.kind,
            Kind::Placeholder
        ) {
            self.split_at(regions, offset)?;
        }
        if end != self.len
            && matches!(
                regions.range(..=end).next_back().unwrap().1.kind,
                Kind::Placeholder
            )
        {
            self.split_at(regions, end)?;
        }
        let mut completed = Vec::new();
        for (base, start, stop) in Self::spans(regions, offset, end) {
            let placeholder = matches!(regions[&base].kind, Kind::Placeholder);
            let ok = unsafe {
                let ptr = self.ptr.as_ptr().add(start);
                if placeholder {
                    os::private(ptr, stop - start, true)
                } else {
                    os::commit(ptr, stop - start)
                }
            };
            if !ok {
                let error = std::io::Error::last_os_error();
                for (base, start, stop, placeholder) in completed {
                    let ptr = unsafe { self.ptr.as_ptr().add(start) };
                    if placeholder {
                        assert!(
                            unsafe { os::split(ptr, stop - start) },
                            "failed to roll back private commit"
                        );
                        regions.get_mut(&base).unwrap().kind = Kind::Placeholder;
                    } else {
                        assert!(
                            unsafe { os::decommit(ptr, stop - start) },
                            "failed to roll back page commit"
                        );
                        if let Kind::Private(pages) = &mut regions.get_mut(&base).unwrap().kind {
                            pages[(start - base) / 4096..(stop - base) / 4096].fill(false);
                        }
                    }
                }
                return Err(format!("commit guest memory: {error}"));
            }
            if placeholder {
                regions.get_mut(&base).unwrap().kind =
                    Kind::Private(vec![true; (stop - start) / 4096]);
            } else if let Kind::Private(pages) = &mut regions.get_mut(&base).unwrap().kind {
                pages[(start - base) / 4096..(stop - base) / 4096].fill(true);
            }
            completed.push((base, start, stop, placeholder));
        }
        Ok(())
    }

    fn commit_windows(&self, offset: usize, len: usize) -> Result<(), String> {
        let end = self.bounds(offset, len)?;
        self.commit_regions(&mut self.regions.borrow_mut(), offset, end)
    }

    fn decommit_windows(&self, offset: usize, len: usize) -> Result<(), String> {
        let end = self.bounds(offset, len)?;
        let mut regions = self.regions.borrow_mut();
        for (base, start, stop) in Self::spans(&regions, offset, end) {
            let ptr = unsafe { self.ptr.as_ptr().add(start) };
            match &mut regions.get_mut(&base).unwrap().kind {
                Kind::Placeholder => {}
                Kind::Private(pages) => {
                    if !unsafe { os::decommit(ptr, stop - start) } {
                        return Err(format!(
                            "decommit guest memory: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    pages[(start - base) / 4096..(stop - base) / 4096].fill(false);
                }
                Kind::Section => {
                    if start != base || stop - start != 4096 {
                        return Err("partial section page unmap".into());
                    }
                    if !unsafe { os::unmap_section(ptr) } {
                        return Err(format!(
                            "unmap guest PCM alias: {}",
                            std::io::Error::last_os_error()
                        ));
                    }
                    regions.get_mut(&base).unwrap().kind = Kind::Placeholder;
                }
            }
        }
        Ok(())
    }

    // A decommitted hole in a private allocation remains reserved by that
    // allocation. Return it to placeholders without releasing its addresses or
    // losing still-committed neighbours; this is only needed on address reuse.
    fn promote_private(
        &self,
        regions: &mut BTreeMap<usize, Region>,
        base: usize,
    ) -> Result<(), String> {
        let region = &regions[&base];
        let Kind::Private(pages) = &region.kind else {
            return Ok(());
        };
        let len = region.len;
        let original = pages.clone();
        let mut saved = Vec::new();
        let mut at = 0;
        while at < pages.len() {
            if !pages[at] {
                at += 1;
                continue;
            }
            let start = at;
            while at < pages.len() && pages[at] {
                at += 1;
            }
            let offset = base + start * 4096;
            // SAFETY: private RW host pages tracked as committed, execution is
            // stopped, and no callback can refer to ordinary private storage.
            let bytes = unsafe {
                std::slice::from_raw_parts(self.ptr.as_ptr().add(offset), (at - start) * 4096)
            }
            .to_vec();
            saved.push((offset, bytes));
        }
        if !unsafe { os::split(self.ptr.as_ptr().add(base), len) } {
            return Err(format!(
                "preserve guest reservation: {}",
                std::io::Error::last_os_error()
            ));
        }
        regions.get_mut(&base).unwrap().kind = Kind::Placeholder;
        for (offset, bytes) in &saved {
            if let Err(error) = self.commit_regions(regions, *offset, offset + bytes.len()) {
                // Undo the replacements, merge the original placeholder, then
                // restore its original private reservation and committed spans.
                let keys: Vec<_> = regions.range(base..base + len).map(|(&at, _)| at).collect();
                for &at in &keys {
                    if matches!(regions[&at].kind, Kind::Private(_)) {
                        assert!(
                            unsafe { os::split(self.ptr.as_ptr().add(at), regions[&at].len) },
                            "cannot roll back guest reservation"
                        );
                    }
                }
                if keys.len() > 1 {
                    assert!(
                        unsafe { os::coalesce(self.ptr.as_ptr().add(base), len) },
                        "cannot merge rollback placeholders"
                    );
                }
                for at in keys {
                    regions.remove(&at);
                }
                assert!(
                    unsafe { os::private(self.ptr.as_ptr().add(base), len, false) },
                    "cannot restore guest reservation"
                );
                regions.insert(
                    base,
                    Region {
                        len,
                        kind: Kind::Private(original),
                    },
                );
                for (offset, bytes) in &saved {
                    assert!(
                        unsafe { os::commit(self.ptr.as_ptr().add(*offset), bytes.len()) },
                        "cannot restore guest pages"
                    );
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            bytes.as_ptr(),
                            self.ptr.as_ptr().add(*offset),
                            bytes.len(),
                        )
                    };
                }
                return Err(error);
            }
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    self.ptr.as_ptr().add(*offset),
                    bytes.len(),
                )
            };
        }
        Ok(())
    }

    /// Add page-sized section views into the reserved linear guest addresses.
    /// 4 KB views let one guest page be unmapped without disturbing other pages
    /// or the independently retained callback mapping of the same section.
    pub(crate) fn map_section(
        &self,
        offset: usize,
        len: usize,
        handle: *mut std::ffi::c_void,
    ) -> Result<(), String> {
        let end = self.bounds(offset, len)?;
        let mut regions = self.regions.borrow_mut();
        let spans = Self::spans(&regions, offset, end);
        for &(base, start, stop) in &spans {
            match &regions[&base].kind {
                Kind::Placeholder => {}
                Kind::Private(pages)
                    if pages[(start - base) / 4096..(stop - base) / 4096]
                        .iter()
                        .all(|p| !p) => {}
                _ => return Err("overlapping shared section mapping".into()),
            }
        }
        for (base, _, _) in spans {
            self.promote_private(&mut regions, base)?;
        }
        let mut mapped = Vec::new();
        for at in (offset..end).step_by(4096) {
            self.split_at(&mut regions, at)?;
            self.split_at(&mut regions, at + 4096)?;
        }
        for at in (offset..end).step_by(4096) {
            if !unsafe { os::section(self.ptr.as_ptr().add(at), handle, at - offset, 4096) } {
                let error = std::io::Error::last_os_error();
                for at in mapped {
                    assert!(
                        unsafe { os::unmap_section(self.ptr.as_ptr().add(at)) },
                        "cannot roll back PCM views"
                    );
                    regions.get_mut(&at).unwrap().kind = Kind::Placeholder;
                }
                return Err(format!("map linear guest PCM: {error}"));
            }
            regions.get_mut(&at).unwrap().kind = Kind::Section;
            mapped.push(at);
        }
        Ok(())
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            for (&offset, region) in self.regions.get_mut().iter() {
                let ptr = unsafe { self.ptr.as_ptr().add(offset) };
                match region.kind {
                    Kind::Section => unsafe {
                        os::unmap_section(ptr);
                        os::release(ptr, region.len);
                    },
                    _ => unsafe { os::release(ptr, region.len) },
                }
            }
        }
        #[cfg(not(windows))]
        unsafe {
            os::release(self.ptr.as_ptr(), self.len)
        }
    }
}
