//! Device storage has two independent OS views. Guest accesses use byte atomics
//! through the writable view; callbacks can only load the read-only alias.
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) struct SharedAllocation {
    mapping: os::Mapping,
    len: usize,
}

// SAFETY: mapping ownership and addresses are immutable. Every exposed data
// access is a byte-width atomic. No ordinary shared Rust slice is produced.
unsafe impl Send for SharedAllocation {}
unsafe impl Sync for SharedAllocation {}

impl SharedAllocation {
    pub(crate) fn new(len: usize) -> Result<Arc<Self>, String> {
        Ok(Arc::new(Self {
            mapping: os::Mapping::new(len)?,
            len,
        }))
    }

    pub(crate) fn load_u8(&self, index: usize) -> u8 {
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

    pub(crate) fn store_u8(&self, index: usize, value: u8) {
        assert!(index < self.len);
        // SAFETY: same stable storage and byte-width access as load_u8; this
        // alias is writable. No mixed-width or non-atomic stores are permitted.
        unsafe {
            (&*self.mapping.write.as_ptr().add(index).cast::<AtomicU8>())
                .store(value, Ordering::Relaxed)
        };
    }
}

impl crate::backend::SharedReadView for SharedAllocation {
    fn len(&self) -> usize {
        self.len
    }
    fn load_u8(&self, index: usize) -> u8 {
        self.load_u8(index)
    }
}

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
        handle: NonNull<c_void>,
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
        fn VirtualQuery(address: *const u8, info: *mut MemoryInfo, len: usize) -> usize;
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
                    storage.mapping.read.as_ptr(),
                    &mut read,
                    std::mem::size_of::<MemoryInfo>()
                ),
                0
            );
            assert_ne!(
                VirtualQuery(
                    storage.mapping.write.as_ptr(),
                    &mut write,
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

#[cfg(not(windows))]
mod os {
    use super::*;
    pub(super) struct Mapping {
        pub read: NonNull<u8>,
        pub write: NonNull<u8>,
    }
    impl Mapping {
        pub fn new(_: usize) -> Result<Self, String> {
            Err("shared PCM OS aliases are currently supported on Windows only".into())
        }
    }
}
