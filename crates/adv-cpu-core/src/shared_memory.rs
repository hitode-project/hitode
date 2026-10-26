//! Device storage has two independent OS views. Guest accesses use byte atomics
//! through the writable view; callbacks can only load the read-only alias.
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub struct SharedAllocation {
    mapping: os::Mapping,
    len: usize,
    /// Host stores are allowed; a read-only file view refuses them.
    writable: bool,
}

impl crate::backend::CounterStorage for Arc<SharedAllocation> {
    fn load(&self, offset: usize) -> u32 {
        u32::from_le_bytes(std::array::from_fn(|i| self.load_u8(offset + i)))
    }
    fn store(&self, offset: usize, value: u32) {
        self.store_bytes(offset, &value.to_le_bytes());
    }
}

// SAFETY: mapping ownership and addresses are immutable. Every exposed data
// access is a byte-width atomic. No ordinary shared Rust slice is produced.
unsafe impl Send for SharedAllocation {}
unsafe impl Sync for SharedAllocation {}

impl SharedAllocation {
    pub fn new(len: usize) -> Result<Arc<Self>, String> {
        if len == 0 {
            return Err("shared PCM length must be nonzero".into());
        }
        Ok(Arc::new(Self {
            mapping: os::Mapping::new(len)?,
            len,
            writable: true,
        }))
    }

    /// `len` bytes of the file `fd` from `offset`, shared with every other
    /// mapping of the file, such as another process's or the in-process
    /// Wine server's.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    pub fn map_file(
        fd: std::os::fd::BorrowedFd<'_>,
        offset: u64,
        len: usize,
        writable: bool,
    ) -> Result<Arc<Self>, String> {
        if len == 0 {
            return Err("shared file view length must be nonzero".into());
        }
        Ok(Arc::new(Self {
            mapping: os::Mapping::file(fd, offset, len, writable)?,
            len,
            writable,
        }))
    }

    /// Whether host stores may reach the storage.
    pub fn writable(&self) -> bool {
        self.writable
    }

    /// The host pages of the storage's own writable (or, for a read-only
    /// view, readable) mapping that hold `len` bytes from `offset`, when
    /// they start a host page `page` bytes long: mapping them elsewhere
    /// shares the bytes.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    pub(crate) fn host_pages(&self, offset: usize, len: usize, page: usize) -> Option<NonNull<u8>> {
        if offset.checked_add(len)? > self.len.next_multiple_of(page) {
            return None;
        }
        self.mapping.write.host_pages(offset, len, page)
    }

    pub fn load_u8(&self, index: usize) -> u8 {
        if index >= self.len {
            return 0;
        }
        // SAFETY: byte alignment is one, the mapping stays alive through self,
        // and AtomicU8 relaxed loads work on OS read-only memory (Rust atomic
        // module's documented small-atomic read-only exception). Only matching
        // byte-width atomic stores through the other physical alias can race.
        unsafe {
            (&*self.mapping.read.as_ptr().add(index).cast::<AtomicU8>()).load(Ordering::Relaxed)
        }
    }

    #[cfg(any(test, feature = "testing"))]
    pub fn store_u8(&self, index: usize, value: u8) {
        assert!(self.writable && index < self.len);
        // SAFETY: same stable storage and byte-width access as load_u8; this
        // alias is writable. No mixed-width or non-atomic stores are permitted.
        unsafe {
            (&*self.mapping.write.as_ptr().add(index).cast::<AtomicU8>())
                .store(value, Ordering::Relaxed)
        };
    }

    #[cfg(windows)]
    pub fn map_linear(
        &self,
        reservation: &crate::virtual_memory::Reservation,
        offset: usize,
    ) -> Result<(), String> {
        reservation.map_section(offset, self.len, self.mapping.handle.as_ptr())
    }

    pub fn store_bytes(&self, offset: usize, input: &[u8]) {
        assert!(self.writable, "store to a read-only shared view");
        assert!(
            offset
                .checked_add(input.len())
                .is_some_and(|end| end <= self.len)
        );
        // SAFETY: the range is checked once and every write remains a matching
        // AtomicU8 store through the independently retained writable OS view.
        for (index, &byte) in input.iter().enumerate() {
            unsafe {
                (&*self
                    .mapping
                    .write
                    .as_ptr()
                    .add(offset + index)
                    .cast::<AtomicU8>())
                    .store(byte, Ordering::Relaxed)
            };
        }
    }
}

impl crate::backend::SharedReadView for SharedAllocation {
    fn len(&self) -> usize {
        self.len
    }
    fn load_u8(&self, index: usize) -> u8 {
        self.load_u8(index)
    }
    fn copy_into(&self, offset: usize, destination: &mut [u8]) {
        let count = self.len.saturating_sub(offset).min(destination.len());
        for (index, byte) in destination[..count].iter_mut().enumerate() {
            // SAFETY: checked in-range byte-atomic access through the read-only
            // OS alias. No ordinary Rust reference to shared data is produced.
            *byte = unsafe {
                (&*self
                    .mapping
                    .read
                    .as_ptr()
                    .add(offset + index)
                    .cast::<AtomicU8>())
                    .load(Ordering::Relaxed)
            };
        }
        destination[count..].fill(0);
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub use crate::dual_view::create_backing;

#[cfg(windows)]
mod os {
    use super::*;
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileMappingW(
            file: *mut c_void,
            attributes: *const c_void,
            protection: u32,
            size_high: u32,
            size_low: u32,
            name: *const u16,
        ) -> *mut c_void;
        fn MapViewOfFile(
            mapping: *mut c_void,
            access: u32,
            offset_high: u32,
            offset_low: u32,
            bytes: usize,
        ) -> *mut c_void;
        fn UnmapViewOfFile(address: *const c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    pub(super) struct Mapping {
        pub read: NonNull<u8>,
        pub write: NonNull<u8>,
        pub(super) handle: NonNull<c_void>,
    }
    impl Mapping {
        pub fn new(len: usize) -> Result<Self, String> {
            // SAFETY: page-file-backed mapping with no inherited handle/name.
            let handle = NonNull::new(unsafe {
                CreateFileMappingW(
                    (-1isize) as *mut c_void,
                    std::ptr::null(),
                    4,
                    ((len as u64) >> 32) as u32,
                    len as u32,
                    std::ptr::null(),
                )
            })
            .ok_or_else(|| format!("create shared PCM: {}", std::io::Error::last_os_error()))?;
            let write =
                NonNull::new(unsafe { MapViewOfFile(handle.as_ptr(), 2, 0, 0, len).cast::<u8>() });
            let read =
                NonNull::new(unsafe { MapViewOfFile(handle.as_ptr(), 4, 0, 0, len).cast::<u8>() });
            if let (Some(write), Some(read)) = (write, read) {
                Ok(Self {
                    read,
                    write,
                    handle,
                })
            } else {
                let error = std::io::Error::last_os_error();
                unsafe {
                    if let Some(p) = write {
                        UnmapViewOfFile(p.as_ptr().cast());
                    }
                    if let Some(p) = read {
                        UnmapViewOfFile(p.as_ptr().cast());
                    }
                    CloseHandle(handle.as_ptr());
                }
                Err(format!("map shared PCM aliases: {error}"))
            }
        }
    }
    impl Drop for Mapping {
        fn drop(&mut self) {
            // SAFETY: Arc retains both views; destruction occurs only after the
            // guest page owners and independently held read aliases are gone.
            unsafe {
                UnmapViewOfFile(self.read.as_ptr().cast());
                UnmapViewOfFile(self.write.as_ptr().cast());
                CloseHandle(self.handle.as_ptr());
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
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
    #[test]
    fn callback_alias_is_os_read_only_and_reads_the_same_physical_bytes() {
        let storage = SharedAllocation::new(8192).unwrap();
        let mut read = MemoryInfo::default();
        let mut write = MemoryInfo::default();
        // SAFETY: query-only OS operation on the two live views.
        unsafe {
            assert_ne!(
                VirtualQuery(
                    storage.mapping.read.as_ptr().cast(),
                    (&mut read as *mut MemoryInfo).cast(),
                    std::mem::size_of::<MemoryInfo>()
                ),
                0
            );
            assert_ne!(
                VirtualQuery(
                    storage.mapping.write.as_ptr().cast(),
                    (&mut write as *mut MemoryInfo).cast(),
                    std::mem::size_of::<MemoryInfo>()
                ),
                0
            );
        }
        assert_eq!(read.protection, 2); // PAGE_READONLY
        assert_eq!(write.protection, 4); // PAGE_READWRITE
        assert_ne!(read.base, write.base);
        storage.store_u8(0, 0x91);
        storage.store_u8(8191, 0x37);
        assert_eq!(storage.load_u8(0), 0x91);
        assert_eq!(storage.load_u8(8191), 0x37);
        assert_eq!(storage.load_u8(8192), 0);
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
mod os {
    use super::*;
    use crate::dual_view::Backing;
    #[cfg(test)]
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    pub(super) struct Mapping {
        pub read: View,
        pub write: View,
    }

    pub(super) struct View {
        ptr: NonNull<u8>,
        len: usize,
        /// Where the logical bytes start in the host-page-aligned mapping.
        skip: usize,
    }

    fn host_page() -> Result<usize, String> {
        // SAFETY: sysconf only queries the running host.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        usize::try_from(page)
            .ok()
            .filter(|&page| page != 0)
            .ok_or_else(|| format!("query host page size: {page}"))
    }

    /// Each view owns an independent mapping of the same backing object. The
    /// descriptor is closed after construction; the mappings retain the object.
    impl Mapping {
        pub fn new(len: usize) -> Result<Self, String> {
            // Hosts may have 16 KiB pages; guest pages remain 4 KiB.
            let len = mapping_len(len, host_page()?)?;
            let backing = Backing::new(c"adv-pcm", len)?;
            let write = View::new(&backing, len, libc::PROT_READ | libc::PROT_WRITE)?;
            let read = View::new(&backing, len, libc::PROT_READ)?;
            // OwnedFd closes here. On errors it also closes, and View drops any
            // completed mapping if creating the second view failed.
            Ok(Self { read, write })
        }

        /// Two views of `len` bytes of `fd` from `offset`; the write view
        /// is read-only unless `writable`. The views start at the host page
        /// that holds `offset`.
        pub fn file(
            fd: std::os::fd::BorrowedFd<'_>,
            offset: u64,
            len: usize,
            writable: bool,
        ) -> Result<Self, String> {
            use std::os::fd::AsRawFd;
            let page = host_page()?;
            let skip = (offset % page as u64) as usize;
            let span = mapping_len(
                len.checked_add(skip).ok_or("shared file view overflow")?,
                page,
            )?;
            let start = libc::off_t::try_from(offset - skip as u64)
                .map_err(|_| "shared file view offset overflow")?;
            let view = |protection| {
                // SAFETY: a fresh shared mapping at an address the host picks,
                // owned by the View until its Drop.
                let ptr = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        span,
                        protection,
                        libc::MAP_SHARED,
                        fd.as_raw_fd(),
                        start,
                    )
                };
                if ptr == libc::MAP_FAILED {
                    return Err(format!(
                        "map shared file view: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(View {
                    ptr: NonNull::new(ptr.cast()).ok_or("map shared file view: null")?,
                    len: span,
                    skip,
                })
            };
            let write = view(if writable {
                libc::PROT_READ | libc::PROT_WRITE
            } else {
                libc::PROT_READ
            })?;
            let read = view(libc::PROT_READ)?;
            Ok(Self { read, write })
        }
    }

    fn mapping_len(len: usize, page: usize) -> Result<usize, String> {
        if len == 0 || page == 0 {
            return Err("shared PCM length and host page size must be nonzero".into());
        }
        let len = len
            .checked_add(page - 1)
            .ok_or("shared PCM host page rounding overflow")?
            / page
            * page;
        if len > isize::MAX as usize {
            return Err("shared PCM mapping exceeds pointer offset limits".into());
        }
        Ok(len)
    }

    impl View {
        fn new(backing: &Backing, len: usize, protection: i32) -> Result<Self, String> {
            // SAFETY: independent view, owned by this View until its Drop.
            let ptr = unsafe { backing.map(None, protection)? };
            Ok(Self { ptr, len, skip: 0 })
        }

        pub(super) fn as_ptr(&self) -> *mut u8 {
            // SAFETY: skip lies inside the mapping.
            unsafe { self.ptr.as_ptr().add(self.skip) }
        }

        /// Where `len` bytes from logical `offset` lie in this view, when
        /// they start a host page `page` bytes long and stay inside it.
        pub(super) fn host_pages(
            &self,
            offset: usize,
            len: usize,
            page: usize,
        ) -> Option<NonNull<u8>> {
            let at = self.skip.checked_add(offset)?;
            (at.is_multiple_of(page) && at.checked_add(len)? <= self.len)
                // SAFETY: checked to lie inside the mapping.
                .then(|| unsafe { self.ptr.add(at) })
        }
    }

    impl Drop for View {
        fn drop(&mut self) {
            // SAFETY: this view uniquely owns its whole host-page-rounded range.
            // Arc retains both aliases until every guest/callback owner is gone.
            unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn host_page_rounding_handles_small_allocations_and_rejects_overflow() {
            for page in [4096, 16384] {
                assert_eq!(mapping_len(1, page).unwrap(), page);
                assert_eq!(mapping_len(4096, page).unwrap(), page);
                assert_eq!(mapping_len(page, page).unwrap(), page);
                assert_eq!(mapping_len(page + 1, page).unwrap(), page * 2);
            }
            assert!(mapping_len(0, 4096).is_err());
            assert!(mapping_len(4096, 0).is_err());
            assert!(mapping_len(usize::MAX, 4096).is_err());
            assert!(mapping_len(isize::MAX as usize, 4096).is_err());
            assert!(SharedAllocation::new(0).is_err());
        }

        // spec: A64-MEM-12
        #[test]
        fn callback_alias_is_os_read_only_and_reads_the_same_physical_bytes() {
            let storage = SharedAllocation::new(8192).unwrap();
            assert_ne!(
                storage.mapping.read.as_ptr(),
                storage.mapping.write.as_ptr()
            );
            storage.store_u8(0, 0x91);
            storage.store_u8(8191, 0x37);
            assert_eq!(storage.load_u8(0), 0x91);
            assert_eq!(storage.load_u8(8191), 0x37);
            assert_eq!(storage.load_u8(8192), 0);

            let mut pipe = [-1; 2];
            // SAFETY: output storage contains room for both returned descriptors.
            assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
            // SAFETY: successful pipe returned fresh, independently owned fds.
            let input = unsafe { OwnedFd::from_raw_fd(pipe[0]) };
            let output = unsafe { OwnedFd::from_raw_fd(pipe[1]) };
            let byte = [0xa5u8];
            // SAFETY: the source is a live byte. This does not touch shared data.
            assert_eq!(
                unsafe { libc::write(output.as_raw_fd(), byte.as_ptr().cast(), byte.len()) },
                1
            );
            // A kernel copy validates the destination mapping and returns EFAULT
            // for the RO alias. No illegal user-space store or signal handler is
            // involved, and the test runner process cannot be faulted by the probe.
            // SAFETY: both fd and mapping are live; the OS rejects the RO write.
            assert_eq!(
                unsafe { libc::read(input.as_raw_fd(), storage.mapping.read.as_ptr().cast(), 1) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::EFAULT)
            );
            assert_eq!(storage.load_u8(0), 0x91);
        }

        #[test]
        fn rounded_backing_is_zeroed_and_keeps_the_logical_read_bounds() {
            // SAFETY: query only; Mapping::new validates the same runtime value.
            let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
            assert_ne!(page, 0);
            for len in [1, 4096, page, page + 1] {
                let storage = SharedAllocation::new(len).unwrap();
                let rounded = mapping_len(len, page).unwrap();
                assert_eq!(storage.mapping.read.len, rounded);
                assert_eq!(storage.mapping.write.len, rounded);
                assert_eq!(storage.mapping.read.as_ptr() as usize % page, 0);
                assert_eq!(storage.mapping.write.as_ptr() as usize % page, 0);
                let mut initial = vec![0xa5; len];
                crate::backend::SharedReadView::copy_into(&*storage, 0, &mut initial);
                assert_eq!(initial, vec![0; len]);
                storage.store_u8(len - 1, 0x37);
                assert_eq!(storage.load_u8(len - 1), 0x37);
                if len < rounded {
                    // SAFETY: padding is backed by both live OS mappings. Even
                    // internal test access uses matching byte atomics; no public
                    // read operation may expose these out-of-logical-range bytes.
                    unsafe {
                        (&*storage.mapping.write.as_ptr().add(len).cast::<AtomicU8>())
                            .store(0xa5, Ordering::Relaxed);
                        assert_eq!(
                            (&*storage.mapping.read.as_ptr().add(len).cast::<AtomicU8>())
                                .load(Ordering::Relaxed),
                            0xa5
                        );
                    }
                }
                assert_eq!(storage.load_u8(len), 0);
                let mut tail = [0xa5; 4];
                crate::backend::SharedReadView::copy_into(&*storage, len - 1, &mut tail);
                assert_eq!(tail, [0x37, 0, 0, 0]);
                crate::backend::SharedReadView::copy_into(&*storage, usize::MAX, &mut tail);
                assert_eq!(tail, [0; 4]);
            }
        }
    }
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
mod os {
    use super::*;
    pub(super) struct Mapping {
        pub read: NonNull<u8>,
        pub write: NonNull<u8>,
    }
    impl Mapping {
        pub fn new(_: usize) -> Result<Self, String> {
            Err("shared PCM OS aliases are unsupported on this platform".into())
        }
    }
}
