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

#[cfg(any(target_os = "linux", target_os = "android", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CacheType {
    dminline: usize,
    iminline: usize,
    idc: bool,
    dic: bool,
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
impl CacheType {
    const fn decode(ctr: u64) -> Self {
        Self {
            dminline: 4usize << ((ctr >> 16) & 0xf),
            iminline: 4usize << (ctr & 0xf),
            idc: (ctr >> 28) & 1 != 0,
            dic: (ctr >> 29) & 1 != 0,
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn cache_type() -> CacheType {
    static CACHE: std::sync::OnceLock<CacheType> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        let ctr: u64;
        // SAFETY: CTR_EL0 is readable at EL0 on Linux and Android; caching it
        // avoids repeated EL1 traps on heterogeneous big.LITTLE SoCs.
        unsafe { std::arch::asm!("mrs {}, ctr_el0", out(reg) ctr, options(nomem, nostack)) };
        CacheType::decode(ctr)
    })
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
fn each_line(base: *const u8, line: usize, ranges: &[(usize, usize)], mut each: impl FnMut(usize)) {
    for &(offset, len) in ranges {
        if len == 0 {
            continue;
        }
        let start = base as usize + offset;
        let mut at = start & !(line - 1);
        while at < start + len {
            each(at);
            at += line;
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "android", test))]
trait CacheOps {
    fn dc_cvau(&mut self, addr: usize);
    fn dsb_ish(&mut self);
    fn ic_ivau(&mut self, addr: usize);
    fn isb(&mut self);
}

/// Runs ARMv8 data-to-instruction cache maintenance for `ranges` using the
/// decoded `CTR_EL0` capabilities:
/// - `IDC == 0`: clean data lines with `dc cvau`, then `dsb ish`;
/// - `IDC == 1`: `dsb ish` alone orders writes to the point of unification;
/// - `DIC == 0`: invalidate instruction lines with `ic ivau`, then `dsb ish`;
/// - `DIC == 1`: skip `ic ivau` and the second `dsb ish`;
/// - always finish with `isb`.
#[cfg(any(target_os = "linux", target_os = "android", test))]
fn synchronize_with(
    cache: CacheType,
    write: *const u8,
    exec: *const u8,
    ranges: &[(usize, usize)],
    ops: &mut impl CacheOps,
) {
    if !cache.idc {
        each_line(write, cache.dminline, ranges, |at| ops.dc_cvau(at));
    }
    ops.dsb_ish();
    if !cache.dic {
        each_line(exec, cache.iminline, ranges, |at| ops.ic_ivau(at));
        ops.dsb_ish();
    }
    ops.isb();
}

/// Cleans data lines through the write view to the point of unification,
/// then invalidates instruction lines through the execute view, with one
/// set of barriers for all `ranges` (offsets and lengths). Cores with
/// CTR_EL0.IDC need no data clean; cores with CTR_EL0.DIC need no
/// instruction invalidate.
///
/// # Safety
/// Every range must be mapped in both views.
#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn clean_and_invalidate(write: *const u8, exec: *const u8, ranges: &[(usize, usize)]) {
    use std::arch::asm;

    struct Arm64Ops;
    impl CacheOps for Arm64Ops {
        #[inline(always)]
        fn dc_cvau(&mut self, addr: usize) {
            // SAFETY: caller guarantees every range is mapped in the write view.
            unsafe { asm!("dc cvau, {}", in(reg) addr, options(nostack)) };
        }
        #[inline(always)]
        fn dsb_ish(&mut self) {
            // SAFETY: barrier only.
            unsafe { asm!("dsb ish", options(nostack)) };
        }
        #[inline(always)]
        fn ic_ivau(&mut self, addr: usize) {
            // SAFETY: caller guarantees every range is mapped in the execute view.
            unsafe { asm!("ic ivau, {}", in(reg) addr, options(nostack)) };
        }
        #[inline(always)]
        fn isb(&mut self) {
            // SAFETY: barrier only.
            unsafe { asm!("isb", options(nostack)) };
        }
    }

    synchronize_with(cache_type(), write, exec, ranges, &mut Arm64Ops);
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

/// iOS: the app cannot create executable memory on its own; a debugger
/// grants it (IOS-JIT-1).
///
/// - With StikDebug's universal script the debugger stays attached and
///   allocates the read-execute region, which TXM (iOS 26+) requires; a
///   vm_remap alias of the same pages is the write view. Every region must
///   exist before it detaches.
/// - A debugger without a script only attaches and detaches, which leaves
///   CS_DEBUGGED set. Before iOS 26 that is enough: the app maps the pages
///   read-write and makes a read-execute alias itself.
#[cfg(target_os = "ios")]
mod os {
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

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
    const P_TRACED: i32 = 0x800;
    const VM_FLAGS_ANYWHERE: libc::c_int = 1;
    const VM_INHERIT_NONE: libc::c_uint = 2;
    /// TXM marks executable pages at this granularity.
    const PAGE: usize = 16 << 10;
    /// A debugger without a script detaches right after attaching; one that
    /// runs the script stays until told to detach.
    const SCRIPT_GRACE: Duration = Duration::from_millis(500);

    /// The script's debugger prepared the pool and still waits for detach.
    static SCRIPTED: AtomicBool = AtomicBool::new(false);

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

    /// JIT was granted: CS_DEBUGGED stays set after the debugger detaches.
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

    /// A debugger is attached right now (Apple QA1361).
    fn traced() -> bool {
        // kinfo_proc is 648 bytes; extern_proc.p_flag sits at offset 32.
        let mut info = [0u64; 96];
        let mut len = size_of_val(&info);
        let mut name = [
            libc::CTL_KERN,
            libc::KERN_PROC,
            libc::KERN_PROC_PID,
            unsafe { libc::getpid() },
        ];
        // SAFETY: the buffer outlives the call and `len` is its size.
        let result = unsafe {
            libc::sysctl(
                name.as_mut_ptr(),
                4,
                info.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        let flag = info[4] as u32 as i32;
        result == 0 && len >= 36 && flag & P_TRACED != 0
    }

    /// The major iOS version, from `kern.osproductversion`.
    fn ios_major() -> Option<u32> {
        let mut text = [0u8; 32];
        let mut len = text.len();
        // SAFETY: the buffer outlives the call and `len` is its size.
        let result = unsafe {
            libc::sysctlbyname(
                c"kern.osproductversion".as_ptr(),
                text.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return None;
        }
        let text = std::str::from_utf8(&text[..len.saturating_sub(1)]).ok()?;
        text.split('.').next()?.parse().ok()
    }

    /// Whether the debugger runs the script: it is still attached after a
    /// scriptless one would have left.
    fn scripted() -> bool {
        let deadline = Instant::now() + SCRIPT_GRACE;
        while traced() {
            if Instant::now() >= deadline {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Maps the pages of `source` again at a new address with `protection`.
    fn alias(source: usize, len: usize, protection: libc::vm_prot_t) -> Result<usize, String> {
        let mut address: libc::vm_address_t = 0;
        let (mut current, mut maximum) = (0, 0);
        // SAFETY: shares (copy = false) mapped pages at a new address.
        let mut result = unsafe {
            vm_remap(
                task(),
                &mut address,
                len,
                0,
                VM_FLAGS_ANYWHERE,
                task(),
                source,
                0,
                &mut current,
                &mut maximum,
                VM_INHERIT_NONE,
            )
        };
        if result == 0 {
            // SAFETY: the alias is ours.
            result = unsafe { vm_protect(task(), address, len, 0, protection) };
            if result != 0 {
                // SAFETY: unused alias of this call.
                unsafe { libc::munmap(address as *mut libc::c_void, len) };
            }
        }
        if result != 0 {
            return Err(format!("alias code region: kern_return {result}"));
        }
        Ok(address)
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
        if scripted() {
            prepared(len)
        } else if ios_major().is_some_and(|major| major < 26) {
            own(len)
        } else {
            Err("JIT unavailable: iOS 26 needs StikDebug's universal.js script for Hitode".into())
        }
    }

    /// The script's debugger allocates the executable view.
    fn prepared(len: usize) -> Result<(NonNull<u8>, NonNull<u8>), String> {
        // A busy debugger leaves the brk encoding in x0, which is never aligned.
        let exec = (0..20)
            // A failed preparation can detach while returning a null region.
            .take_while(|_| traced())
            .find_map(|attempt| {
                if attempt > 0 {
                    std::thread::sleep(Duration::from_millis(50));
                }
                // SAFETY: the script's debugger is attached and services the breakpoint.
                let region = unsafe { prepare_region(std::ptr::null_mut(), len) };
                NonNull::new(region).filter(|r| (r.as_ptr() as usize).is_multiple_of(PAGE))
            })
            .ok_or("the debugger did not prepare an executable region")?;
        SCRIPTED.store(true, Ordering::Relaxed);
        let write = alias(
            exec.as_ptr() as usize,
            len,
            libc::VM_PROT_READ | libc::VM_PROT_WRITE,
        )
        .inspect_err(|_| {
            // SAFETY: the prepared region is unused.
            unsafe { libc::munmap(exec.as_ptr().cast(), len) };
        })?;
        let write = NonNull::new(write as *mut u8).ok_or("code alias at null")?;
        Ok((write, exec))
    }

    /// Before TXM, CS_DEBUGGED lets the app make its own read-execute alias.
    fn own(len: usize) -> Result<(NonNull<u8>, NonNull<u8>), String> {
        // SAFETY: a new private anonymous mapping at an OS-chosen address.
        let write = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if write == libc::MAP_FAILED {
            return Err(format!(
                "map JIT code pool: {}",
                std::io::Error::last_os_error()
            ));
        }
        let exec = alias(
            write as usize,
            len,
            libc::VM_PROT_READ | libc::VM_PROT_EXECUTE,
        )
        .inspect_err(|_| {
            // SAFETY: the mapping is unused.
            unsafe { libc::munmap(write, len) };
        })?;
        let write = NonNull::new(write.cast::<u8>()).ok_or("code pool at null")?;
        let exec = NonNull::new(exec as *mut u8).ok_or("code alias at null")?;
        Ok((write, exec))
    }

    /// Ends the script's part once the pool exists; nothing executable can
    /// be added afterwards. A debugger without the script has already left.
    pub fn finish_preparation() {
        if SCRIPTED.load(Ordering::Relaxed) && traced() {
            // SAFETY: the script's debugger is attached and services the breakpoint.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Op {
        DcCvau(usize),
        DsbIsh,
        IcIvau(usize),
        Isb,
    }

    #[derive(Default)]
    struct Recorder(Vec<Op>);

    impl CacheOps for Recorder {
        fn dc_cvau(&mut self, addr: usize) {
            self.0.push(Op::DcCvau(addr));
        }
        fn dsb_ish(&mut self) {
            self.0.push(Op::DsbIsh);
        }
        fn ic_ivau(&mut self, addr: usize) {
            self.0.push(Op::IcIvau(addr));
        }
        fn isb(&mut self) {
            self.0.push(Op::Isb);
        }
    }

    // spec: A64-CTL-2, A64-MEM-12
    #[test]
    fn ctr_el0_decodes_line_sizes_and_idc_dic_bits() {
        // Typical Cortex-A55/A76: IminLine=4 (64B), DminLine=4 (64B), IDC=0, DIC=0.
        assert_eq!(
            CacheType::decode(0x8444_c004),
            CacheType {
                dminline: 64,
                iminline: 64,
                idc: false,
                dic: false,
            }
        );
        // Asymmetric lines (32B icache, 64B dcache) with IDC=1 (bit 28), DIC=0.
        let idc_only = (1u64 << 28) | (4u64 << 16) | 3u64;
        assert_eq!(
            CacheType::decode(idc_only),
            CacheType {
                dminline: 64,
                iminline: 32,
                idc: true,
                dic: false,
            }
        );
        // DIC=1 (bit 29) with IDC=0, 16B icache, 128B dcache.
        let dic_only = (1u64 << 29) | (5u64 << 16) | 2u64;
        assert_eq!(
            CacheType::decode(dic_only),
            CacheType {
                dminline: 128,
                iminline: 16,
                idc: false,
                dic: true,
            }
        );
        // Both IDC=1 and DIC=1 (ARMv8.5+ coherent caches).
        let coherent = (1u64 << 29) | (1u64 << 28) | (4u64 << 16) | 4u64;
        assert_eq!(
            CacheType::decode(coherent),
            CacheType {
                dminline: 64,
                iminline: 64,
                idc: true,
                dic: true,
            }
        );
    }

    // spec: A64-CTL-2, A64-MEM-12
    #[test]
    fn cache_sync_respects_idc_and_dic_barriers() {
        let write = 0x1000usize as *const u8;
        let exec = 0x2000usize as *const u8;
        // Unaligned span across two 64B dcache / three 32B icache lines, plus an empty range.
        let ranges = [(0x10usize, 0x48usize), (0x14usize, 0usize)];

        // IDC=0, DIC=0: dc cvau -> dsb ish -> ic ivau -> dsb ish -> isb.
        let mut ops = Recorder::default();
        synchronize_with(
            CacheType {
                dminline: 64,
                iminline: 32,
                idc: false,
                dic: false,
            },
            write,
            exec,
            &ranges,
            &mut ops,
        );
        assert_eq!(
            ops.0,
            vec![
                Op::DcCvau(0x1000),
                Op::DcCvau(0x1040),
                Op::DsbIsh,
                Op::IcIvau(0x2000),
                Op::IcIvau(0x2020),
                Op::IcIvau(0x2040),
                Op::DsbIsh,
                Op::Isb,
            ]
        );

        // IDC=1, DIC=0: dsb ish -> ic ivau -> dsb ish -> isb (no dc cvau).
        let mut ops = Recorder::default();
        synchronize_with(
            CacheType {
                dminline: 64,
                iminline: 32,
                idc: true,
                dic: false,
            },
            write,
            exec,
            &ranges,
            &mut ops,
        );
        assert_eq!(
            ops.0,
            vec![
                Op::DsbIsh,
                Op::IcIvau(0x2000),
                Op::IcIvau(0x2020),
                Op::IcIvau(0x2040),
                Op::DsbIsh,
                Op::Isb,
            ]
        );

        // IDC=0, DIC=1: dc cvau -> dsb ish -> isb (no ic ivau, no second dsb ish).
        let mut ops = Recorder::default();
        synchronize_with(
            CacheType {
                dminline: 64,
                iminline: 32,
                idc: false,
                dic: true,
            },
            write,
            exec,
            &ranges,
            &mut ops,
        );
        assert_eq!(
            ops.0,
            vec![Op::DcCvau(0x1000), Op::DcCvau(0x1040), Op::DsbIsh, Op::Isb]
        );

        // IDC=1, DIC=1: dsb ish -> isb only.
        let mut ops = Recorder::default();
        synchronize_with(
            CacheType {
                dminline: 64,
                iminline: 32,
                idc: true,
                dic: true,
            },
            write,
            exec,
            &ranges,
            &mut ops,
        );
        assert_eq!(ops.0, vec![Op::DsbIsh, Op::Isb]);
    }
}
