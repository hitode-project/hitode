//! Executable view ownership and platform preparation boundary.
use std::ptr::NonNull;

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

#[cfg(target_os = "ios")]
pub(super) use os::{debugged, finish_preparation};
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

/// iOS 26+ with TXM: the app cannot create executable memory. The attached
/// debugger (StikDebug's universal protocol) allocates the read-execute
/// region, and a vm_remap alias of the same pages is the write view. Every
/// region must exist before the debugger detaches (IOS-JIT-1).
#[cfg(target_os = "ios")]
mod os {
    use std::ptr::NonNull;

    unsafe extern "C" {
        static mach_task_self_: libc::mach_port_t;
        fn csops(
            pid: libc::pid_t,
            ops: libc::c_uint,
            out: *mut libc::c_void,
            len: libc::size_t,
        ) -> libc::c_int;
        fn vm_remap(
            task: libc::mach_port_t,
            address: *mut libc::vm_address_t,
            size: libc::vm_size_t,
            mask: libc::vm_address_t,
            flags: libc::c_int,
            source_task: libc::mach_port_t,
            source: libc::vm_address_t,
            copy: libc::boolean_t,
            current: *mut libc::vm_prot_t,
            maximum: *mut libc::vm_prot_t,
            inheritance: libc::c_uint,
        ) -> libc::kern_return_t;
        fn vm_protect(
            task: libc::mach_port_t,
            address: libc::vm_address_t,
            size: libc::vm_size_t,
            set_maximum: libc::boolean_t,
            protection: libc::vm_prot_t,
        ) -> libc::kern_return_t;
        fn sys_icache_invalidate(start: *mut libc::c_void, len: libc::size_t);
    }
    const CS_OPS_STATUS: libc::c_uint = 0;
    const CS_DEBUGGED: u32 = 0x1000_0000;
    const VM_FLAGS_ANYWHERE: libc::c_int = 1;
    const VM_INHERIT_NONE: libc::c_uint = 2;
    /// TXM marks executable pages at this granularity.
    const PAGE: usize = 16 << 10;

    /// x16 = 1: the debugger prepares an RX region of x1 bytes; x0 = null
    /// asks it to allocate one. The region returns in x0.
    #[unsafe(naked)]
    unsafe extern "C" fn prepare_region(_: *mut u8, _: usize) -> *mut u8 {
        core::arch::naked_asm!("mov x16, #1", "brk #0xf00d", "ret")
    }

    /// x16 = 0: the debugger detaches; prepared regions stay executable.
    #[unsafe(naked)]
    unsafe extern "C" fn detach() {
        core::arch::naked_asm!("mov x16, #0", "brk #0xf00d", "ret")
    }

    fn task() -> libc::mach_port_t {
        // SAFETY: libSystem initializes the task port before any Rust code.
        unsafe { mach_task_self_ }
    }

    /// A breakpoint without a debugger would kill the process.
    pub fn debugged() -> bool {
        let mut status = 0u32;
        // SAFETY: queries this process's own code-signing status word.
        let result = unsafe {
            csops(
                libc::getpid(),
                CS_OPS_STATUS,
                (&raw mut status).cast(),
                size_of::<u32>(),
            )
        };
        result == 0 && status & CS_DEBUGGED != 0
    }

    pub fn map(len: usize) -> Result<(NonNull<u8>, NonNull<u8>), String> {
        if !debugged() {
            return Err(
                "JIT unavailable: no debugger is attached (launch through StikDebug)".into(),
            );
        }
        if len == 0 || !len.is_multiple_of(PAGE) {
            return Err(format!(
                "code pool of {len} bytes is not whole 16 KiB pages"
            ));
        }
        // A busy debugger leaves the brk encoding in x0, which is never aligned.
        let exec = (0..20)
            .find_map(|attempt| {
                if attempt > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                // SAFETY: the debugger is attached and services the breakpoint.
                let region = unsafe { prepare_region(std::ptr::null_mut(), len) };
                NonNull::new(region).filter(|r| r.as_ptr() as usize % PAGE == 0)
            })
            .ok_or("the debugger did not prepare an executable region")?;
        let mut write: libc::vm_address_t = 0;
        let (mut current, mut maximum) = (0, 0);
        // SAFETY: shares (copy = false) the prepared pages at a new address.
        let mut result = unsafe {
            vm_remap(
                task(),
                &mut write,
                len,
                0,
                VM_FLAGS_ANYWHERE,
                task(),
                exec.as_ptr() as libc::vm_address_t,
                0,
                &mut current,
                &mut maximum,
                VM_INHERIT_NONE,
            )
        };
        if result == 0 {
            // SAFETY: the alias is ours; the executable view keeps read-execute.
            result = unsafe {
                vm_protect(
                    task(),
                    write,
                    len,
                    0,
                    libc::VM_PROT_READ | libc::VM_PROT_WRITE,
                )
            };
            if result != 0 {
                // SAFETY: unused alias of this call.
                unsafe { libc::munmap(write as *mut libc::c_void, len) };
            }
        }
        if result != 0 {
            // SAFETY: the prepared region is unused.
            unsafe { libc::munmap(exec.as_ptr().cast(), len) };
            return Err(format!("alias prepared code region: kern_return {result}"));
        }
        let write = NonNull::new(write as *mut u8).ok_or("code alias at null")?;
        Ok((write, exec))
    }

    /// Ends the debugger's part once the pool exists; nothing executable can
    /// be added afterwards.
    pub fn finish_preparation() {
        if debugged() {
            // SAFETY: the debugger is attached and services the breakpoint.
            unsafe { detach() };
        }
    }

    pub fn writable(_: bool) {}

    /// # Safety
    /// Every range must be mapped in both views.
    pub unsafe fn synchronize(_: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
        for &(offset, len) in ranges {
            // SAFETY: forwarded; Darwin cleans data lines as the core requires.
            unsafe { sys_icache_invalidate(exec.add(offset).cast_mut().cast(), len) };
        }
    }

    /// # Safety
    /// The views must come from `map(len)` and be unused.
    pub unsafe fn unmap(write: NonNull<u8>, exec: NonNull<u8>, len: usize) {
        // SAFETY: forwarded.
        unsafe {
            libc::munmap(write.as_ptr().cast(), len);
            libc::munmap(exec.as_ptr().cast(), len);
        }
    }
}
