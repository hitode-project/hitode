//! Stable host virtual address reservation. Guest permissions remain in our page table;
//! committed host pages are RW so loaders can populate guest RX pages safely.
use std::ptr::NonNull;

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
    pub unsafe fn reserve(size: usize) -> *mut u8 {
        unsafe { VirtualAlloc(std::ptr::null_mut(), size, 0x2000, 1).cast() }
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
}
impl Reservation {
    pub fn new(len: usize) -> Result<Self, String> {
        let ptr = NonNull::new(unsafe { os::reserve(len) })
            .ok_or_else(|| format!("reserve guest memory: {}", std::io::Error::last_os_error()))?;
        Ok(Self { ptr, len })
    }
    pub fn commit(&self, offset: usize, len: usize) -> Result<(), String> {
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
    pub fn decommit(&self, offset: usize, len: usize) -> Result<(), String> {
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
impl Drop for Reservation {
    fn drop(&mut self) {
        unsafe { os::release(self.ptr.as_ptr(), self.len) }
    }
}
