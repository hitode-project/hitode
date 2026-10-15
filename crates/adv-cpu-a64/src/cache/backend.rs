//! Executable view ownership and platform preparation boundary.
use std::ptr::NonNull;

#[cfg(target_os = "ios")]
compile_error!("iOS code views are not implemented: Mach memory entry preparation is required");

pub(super) struct Mapping {
    pub write: NonNull<u8>,
    pub exec: NonNull<u8>,
    len: usize,
}

impl Mapping {
    pub fn new(len: usize) -> Result<Self, String> {
        let (write, exec) = os::map(len)?;
        Ok(Self { write, exec, len })
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: the pool owns all views and cannot drop with live leases.
        unsafe { os::unmap(self.write, self.exec, self.len) };
    }
}

pub(super) use os::{synchronize, writable};

#[cfg(any(target_os = "linux", target_os = "android"))]
fn each_line(base: *const u8, line: usize, ranges: &[(usize, usize)], mut each: impl FnMut(usize)) {
    for &(offset, len) in ranges {
        let start = base as usize + offset;
        let mut at = start & !(line - 1);
        while at < start + len {
            each(at);
            at += line;
        }
    }
}

/// Cleans data lines through the write view to the point of unification,
/// then invalidates instruction lines through the execute view, with one
/// set of barriers for all `ranges` (offsets and lengths). Cores with
/// CTR_EL0.IDC need no data clean.
///
/// # Safety
/// Every range must be mapped in both views.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn clean_and_invalidate(write: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
    use std::arch::asm;
    let ctr: u64;
    // SAFETY: CTR_EL0 is readable at EL0 on Linux.
    unsafe { asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
    if ctr >> 28 & 1 == 0 {
        let data_line = 4usize << (ctr >> 16 & 0xf);
        // SAFETY: cache maintenance on mapped addresses has no other effect.
        each_line(write, data_line, ranges, |at| unsafe {
            asm!("dc cvau, {}", in(reg) at, options(nostack))
        });
    }
    // SAFETY: barriers only.
    unsafe { asm!("dsb ish", options(nostack)) };
    let code_line = 4usize << (ctr & 0xf);
    // SAFETY: as above.
    each_line(exec, code_line, ranges, |at| unsafe {
        asm!("ic ivau, {}", in(reg) at, options(nostack))
    });
    // SAFETY: barriers only.
    unsafe { asm!("dsb ish", "isb", options(nostack)) };
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod os {
    use std::ptr::NonNull;

    pub fn map(len: usize) -> Result<(NonNull<u8>, NonNull<u8>), String> {
        let backing = adv_cpu_core::dual_view::Backing::new(c"adv-a64-code", len)?;
        // SAFETY: independent views, released together by Mapping after all users.
        let write = unsafe { backing.map(None, libc::PROT_READ | libc::PROT_WRITE)? };
        let exec = match unsafe { backing.map(None, libc::PROT_READ | libc::PROT_EXEC) } {
            Ok(exec) => exec,
            Err(error) => {
                unsafe { libc::munmap(write.as_ptr().cast(), len) };
                return Err(error);
            }
        };
        // The descriptor closes here; the two mappings keep the object.
        Ok((write, exec))
    }

    pub fn writable(_: bool) {}

    /// # Safety
    /// Every range must be mapped in both views.
    pub unsafe fn synchronize(write: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
        // SAFETY: forwarded.
        unsafe { super::clean_and_invalidate(write, exec, ranges) };
    }

    /// # Safety
    /// The views must come from `map(len)` and be unused.
    pub unsafe fn unmap(write: NonNull<u8>, exec: NonNull<u8>, len: usize) {
        // SAFETY: forwarded.
        unsafe {
            libc::munmap(exec.as_ptr().cast(), len);
            libc::munmap(write.as_ptr().cast(), len);
        }
    }
}

#[cfg(target_os = "macos")]
mod os {
    use std::ptr::NonNull;

    unsafe extern "C" {
        fn pthread_jit_write_protect_np(enabled: libc::c_int);
        fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
    }

    pub fn map(len: usize) -> Result<(NonNull<u8>, NonNull<u8>), String> {
        // SAFETY: a new private JIT mapping at an OS-chosen address.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_JIT,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(format!(
                "map JIT code cache: {}",
                std::io::Error::last_os_error()
            ));
        }
        let view = NonNull::new(address.cast::<u8>()).ok_or("code cache at null")?;
        Ok((view, view))
    }

    /// Flips this thread's view of every MAP_JIT range.
    pub fn writable(enabled: bool) {
        // SAFETY: affects only the calling thread's JIT permission.
        unsafe { pthread_jit_write_protect_np(libc::c_int::from(!enabled)) };
    }

    /// # Safety
    /// Every range must be mapped.
    pub unsafe fn synchronize(_: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
        for &(offset, len) in ranges {
            // SAFETY: forwarded.
            unsafe { sys_icache_invalidate(exec.add(offset).cast_mut().cast(), len) };
        }
    }

    /// # Safety
    /// The view must come from `map(len)` and be unused.
    pub unsafe fn unmap(view: NonNull<u8>, _: NonNull<u8>, len: usize) {
        // SAFETY: forwarded.
        unsafe { libc::munmap(view.as_ptr().cast(), len) };
    }
}
