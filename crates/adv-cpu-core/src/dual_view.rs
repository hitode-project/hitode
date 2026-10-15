//! Backing interface for coherent OS aliases (code, devices and guest windows).
//! Views retain the object after Backing drops. Owners must stop all users and
//! unmap dependent window/executable views before releasing writable aliases.
#[cfg(target_os = "ios")]
compile_error!("iOS dual-view backend is not implemented: Mach memory entry is required");

#[cfg(unix)]
use std::{
    ffi::CStr,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    ptr::NonNull,
};

#[cfg(unix)]
pub struct Backing {
    fd: OwnedFd,
    len: usize,
}

#[cfg(unix)]
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

// This is the sole platform backing selection point. The iOS compile error
// above prevents accidentally using Darwin POSIX shm inside the iOS sandbox.
#[cfg(target_os = "ios")]
fn create_backing(_: &CStr) -> Result<OwnedFd, String> {
    unreachable!()
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
}
