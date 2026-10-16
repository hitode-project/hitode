//! Backing interface for coherent OS aliases (code, devices and guest windows).
//! Views retain the object after Backing drops. Owners must stop all users and
//! unmap dependent window/executable views before releasing writable aliases.
#[cfg(all(unix, not(target_os = "ios")))]
use std::{
    ffi::CStr,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr::NonNull,
};

#[cfg(target_os = "ios")]
pub use mach::Backing;

#[cfg(all(unix, not(target_os = "ios")))]
pub struct Backing {
    fd: OwnedFd,
    len: usize,
}

#[cfg(all(unix, not(target_os = "ios")))]
impl Backing {
    pub fn new(name: &CStr, len: usize) -> Result<Self, String> {
        let size = libc::off_t::try_from(len).map_err(|_| "dual-view backing too large")?;
        if size <= 0 {
            return Err("dual-view backing must be nonempty".into());
        }
        let fd = create_backing(name)?;
        // SAFETY: fresh private backing, sized before any view exists.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
            return Err(format!(
                "size dual-view backing: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { fd, len })
    }

    /// # Safety
    /// `place`, when present, must be an exclusively owned reservation of len
    /// bytes. The caller owns the returned mapping and must unmap it once unused.
    pub unsafe fn map(
        &self,
        place: Option<NonNull<u8>>,
        protection: i32,
    ) -> Result<NonNull<u8>, String> {
        // SAFETY: forwarded exclusive reservation contract, or a new OS address.
        let ptr = unsafe {
            libc::mmap(
                place.map_or(std::ptr::null_mut(), |p| p.as_ptr().cast()),
                self.len,
                protection,
                libc::MAP_SHARED | if place.is_some() { libc::MAP_FIXED } else { 0 },
                self.fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!(
                "map dual-view alias: {}",
                std::io::Error::last_os_error()
            ));
        }
        NonNull::new(ptr.cast()).ok_or_else(|| {
            // SAFETY: release successful but unusable address-zero mapping.
            unsafe { libc::munmap(ptr, self.len) };
            "dual-view alias mapped at null".into()
        })
    }
}

/// A Mach named memory entry: the iOS sandbox has no POSIX shm, and each
/// `vm_map` of the entry is a coherent alias. macOS builds it for tests.
#[cfg(any(target_os = "ios", all(test, target_os = "macos")))]
pub mod mach {
    use std::ptr::NonNull;

    unsafe extern "C" {
        static mach_task_self_: libc::mach_port_t;
        fn mach_make_memory_entry_64(
            task: libc::mach_port_t,
            size: *mut u64,
            offset: u64,
            permission: libc::vm_prot_t,
            entry: *mut libc::mach_port_t,
            parent: libc::mach_port_t,
        ) -> libc::kern_return_t;
        fn vm_map(
            task: libc::mach_port_t,
            address: *mut libc::vm_address_t,
            size: libc::vm_size_t,
            mask: libc::vm_address_t,
            flags: libc::c_int,
            entry: libc::mach_port_t,
            offset: libc::vm_offset_t,
            copy: libc::boolean_t,
            current: libc::vm_prot_t,
            maximum: libc::vm_prot_t,
            inheritance: libc::c_uint,
        ) -> libc::kern_return_t;
        fn mach_port_deallocate(
            task: libc::mach_port_t,
            name: libc::mach_port_t,
        ) -> libc::kern_return_t;
    }
    /// Create a new anonymous object instead of naming an existing range.
    const MAP_MEM_NAMED_CREATE: libc::vm_prot_t = 0x02_0000;
    /// Child processes never inherit an alias.
    const VM_INHERIT_NONE: libc::c_uint = 2;
    const VM_FLAGS_ANYWHERE: libc::c_int = 0x0001;
    const VM_FLAGS_FIXED_OVERWRITE: libc::c_int = 0x4000;

    fn task() -> libc::mach_port_t {
        // SAFETY: libSystem initializes the task port before any Rust code.
        unsafe { mach_task_self_ }
    }

    pub struct Backing {
        entry: libc::mach_port_t,
        len: usize,
    }

    impl Backing {
        pub fn new(_: &std::ffi::CStr, len: usize) -> Result<Self, String> {
            if len == 0 || isize::try_from(len).is_err() {
                return Err("dual-view backing size must be nonempty and addressable".into());
            }
            let mut size = len as u64;
            let mut entry = 0;
            // Execute stays within the entry's maximum so code views can map it.
            let all = libc::VM_PROT_READ | libc::VM_PROT_WRITE | libc::VM_PROT_EXECUTE;
            // SAFETY: creates a fresh object and returns its send right.
            let result = unsafe {
                mach_make_memory_entry_64(
                    task(),
                    &mut size,
                    0,
                    MAP_MEM_NAMED_CREATE | all,
                    &mut entry,
                    0,
                )
            };
            if result != 0 || size < len as u64 {
                return Err(format!(
                    "create dual-view memory entry: kern_return {result}"
                ));
            }
            Ok(Self { entry, len })
        }

        /// # Safety
        /// `place`, when present, must be an exclusively owned reservation of len
        /// bytes. The caller owns the returned mapping and must unmap it once unused.
        pub unsafe fn map(
            &self,
            place: Option<NonNull<u8>>,
            protection: i32,
        ) -> Result<NonNull<u8>, String> {
            // A view never gains both write and execute: code views stay
            // read-execute, every other view at most read-write.
            let maximum = if protection & libc::PROT_EXEC != 0 {
                libc::VM_PROT_READ | libc::VM_PROT_EXECUTE
            } else {
                libc::VM_PROT_READ | libc::VM_PROT_WRITE
            };
            let mut address = place.map_or(0, |p| p.as_ptr() as libc::vm_address_t);
            let flags = if place.is_some() {
                VM_FLAGS_FIXED_OVERWRITE
            } else {
                VM_FLAGS_ANYWHERE
            };
            // SAFETY: forwarded exclusive reservation contract, or a new address.
            let result = unsafe {
                vm_map(
                    task(),
                    &mut address,
                    self.len,
                    0,
                    flags,
                    self.entry,
                    0,
                    0,
                    protection,
                    maximum,
                    VM_INHERIT_NONE,
                )
            };
            if result != 0 {
                return Err(format!("map dual-view alias: kern_return {result}"));
            }
            NonNull::new(address as *mut u8).ok_or_else(|| "dual-view alias mapped at null".into())
        }
    }

    impl Drop for Backing {
        fn drop(&mut self) {
            // SAFETY: releases this handle's own send right; views keep the object.
            unsafe { mach_port_deallocate(task(), self.entry) };
        }
    }
}

/// A fresh, unnamed, zero-length memfd. The a64 code cache maps one twice,
/// as a writable and an executable view.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn create_backing(name: &std::ffi::CStr) -> Result<OwnedFd, String> {
    // Android's libc memfd_create symbol requires API 30, while this project
    // also supports API 29. The kernel syscall avoids that loader dependency.
    // SAFETY: the name is NUL terminated and CLOEXEC is a supported flag.
    #[cfg(target_os = "android")]
    let fd = unsafe {
        libc::syscall(libc::SYS_memfd_create, name.as_ptr(), libc::MFD_CLOEXEC) as libc::c_int
    };
    #[cfg(target_os = "linux")]
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(format!(
            "create {name:?} memfd: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: success returns a fresh descriptor owned solely by this call.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "macos")]
pub fn create_backing(_: &std::ffi::CStr) -> Result<OwnedFd, String> {
    use std::ffi::CString;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_NAME: AtomicU64 = AtomicU64::new(0);

    // Names fit Darwin's short SHM_NAME_MAX. O_EXCL prevents attaching to
    // another object's storage, including a stale name after process death.
    for _ in 0..128 {
        let id = NEXT_NAME.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("/ap-{:x}-{id:x}", std::process::id())).unwrap();
        // SAFETY: valid C name, exclusive creation and owner-only access.
        // Darwin marks shm_open descriptors close-on-exec.
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
            return Err(format!("create shared PCM shm object: {error}"));
        }
        // SAFETY: this descriptor is fresh and has not escaped.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        loop {
            // SAFETY: remove the newly created name immediately. The fd,
            // and later the mappings, retain the otherwise anonymous object.
            if unsafe { libc::shm_unlink(name.as_ptr()) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINTR) {
                return Err(format!("unlink shared PCM shm object: {error}"));
            }
        }
        return Ok(fd);
    }
    Err("create shared PCM shm object: exhausted exclusive names".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    // spec: A64-MEM-12
    #[test]
    fn aliases_retain_backing_after_its_handle_closes() {
        let len = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let backing = Backing::new(c"adv-alias-test", len).unwrap();
        let write = unsafe { backing.map(None, libc::PROT_READ | libc::PROT_WRITE) }.unwrap();
        let read = unsafe { backing.map(None, libc::PROT_READ) }.unwrap();
        drop(backing);
        assert_ne!(write, read);
        unsafe {
            write.as_ptr().write(23);
            write.as_ptr().add(len - 1).write(71);
            assert_eq!(read.as_ptr().read(), 23);
            assert_eq!(read.as_ptr().add(len - 1).read(), 71);
            assert_eq!(libc::munmap(read.as_ptr().cast(), len), 0);
            assert_eq!(libc::munmap(write.as_ptr().cast(), len), 0);
        }
        assert!(Backing::new(c"invalid", 0).is_err());
        assert!(Backing::new(c"invalid", usize::MAX).is_err());
    }

    // spec: A64-MEM-12
    #[cfg(target_os = "macos")]
    #[test]
    fn mach_entries_alias_views_and_keep_code_views_read_execute() {
        use super::mach::Backing;
        let len = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let backing = Backing::new(c"adv-alias-test", len).unwrap();
        let write = unsafe { backing.map(None, libc::PROT_READ | libc::PROT_WRITE) }.unwrap();
        let read = unsafe { backing.map(None, libc::PROT_READ) }.unwrap();
        let code = unsafe { backing.map(None, libc::PROT_READ | libc::PROT_EXEC) }.unwrap();
        drop(backing);
        unsafe {
            write.as_ptr().add(len - 1).write(71);
            assert_eq!(read.as_ptr().add(len - 1).read(), 71);
            assert_eq!(code.as_ptr().add(len - 1).read(), 71);
            // mov w0, #42; ret — written through one view, run through the other.
            write
                .as_ptr()
                .cast::<[u32; 2]>()
                .write([0x5280_0540, 0xd65f_03c0]);
            unsafe extern "C" {
                fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
            }
            sys_icache_invalidate(code.as_ptr().cast(), 8);
            let run: extern "C" fn() -> u32 = std::mem::transmute(code.as_ptr());
            assert_eq!(run(), 42);
            // The code view's maximum excludes write; the window view's excludes execute.
            let rw = libc::PROT_READ | libc::PROT_WRITE;
            assert_ne!(libc::mprotect(code.as_ptr().cast(), len, rw), 0);
            assert_ne!(
                libc::mprotect(read.as_ptr().cast(), len, libc::PROT_EXEC),
                0
            );
            assert_eq!(libc::mprotect(read.as_ptr().cast(), len, rw), 0);
            for view in [write, read, code] {
                assert_eq!(libc::munmap(view.as_ptr().cast(), len), 0);
            }
        }
        assert!(Backing::new(c"invalid", 0).is_err());
        assert!(Backing::new(c"invalid", usize::MAX).is_err());
    }
}
