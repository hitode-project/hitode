//! Guest x86-32 CPU interface.
//!
//! The runtime never single-steps the guest. It maps memory, sets up a context, and calls
//! [`Backend::run`]. Guest code runs until it executes a trap instruction (`int 0x2e` in a
//! thunk) or faults. On a trap the backend calls [`TrapHandler::on_trap`] from inside the
//! emulation loop. The handler either finishes the call on the spot ([`TrapAction::Continue`]:
//! the usual path for API calls) or asks the backend to park the guest
//! ([`TrapAction::Park`]). Parking unwinds out of `run`, so only the top-level scheduler
//! ever re-enters guest code. There is no host→guest recursion.

use std::time::Duration;

pub use crate::{ArchitecturalState, StateError};
use thiserror::Error;

/// Cumulative host memory work, owned by the guest execution worker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostAccess {
    pub reads: u64,
    pub read_bytes: u64,
    pub writes: u64,
    pub written_bytes: u64,
    pub shared_copies: u64,
    pub fallbacks: u64,
    pub tlb_misses: u64,
}

/// Identity of a retained contiguous read-only file range.
pub struct Pinned {
    pub(crate) addr: u32,
    pub(crate) len: usize,
    pub(crate) generation: std::cell::Cell<u64>,
    pub(crate) view: ReadOnlyFileView,
}

/// Tiny `bitflags`-style newtype, so we don't pull a dependency for one type.
macro_rules! bitflags_lite {
    ($(#[$m:meta])* $name:ident : $t:ty { $($flag:ident = $v:expr),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub $t);
        impl $name {
            $(pub const $flag: $name = $name($v);)*
            pub const fn contains(self, other: $name) -> bool { self.0 & other.0 == other.0 }
        }
        impl core::ops::BitOr for $name {
            type Output = $name;
            fn bitor(self, rhs: $name) -> $name { $name(self.0 | rhs.0) }
        }
    };
}

pub mod asm;
pub mod file_page;
pub use crate::shared_memory::ReadOnlyFileView;
pub use file_page::FilePage;
pub mod x87;
pub use adv_sync::shared::{SharedReadView, SharedRegion};

/// Execution-owner-only atomic storage for guest COM reference words.
/// The Rc returned by Guest prevents transfer to device callbacks.
pub trait CounterStorage {
    fn load(&self, offset: usize) -> u32;
    fn store(&self, offset: usize, value: u32);
}

/// Guest general-purpose registers plus EIP/EFLAGS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reg {
    Eax,
    Ecx,
    Edx,
    Ebx,
    Esp,
    Ebp,
    Esi,
    Edi,
    Eip,
    Eflags,
}

bitflags_lite! {
    /// Page protection for [`Backend::map`].
    Prot: u8 { READ = 1, WRITE = 2, EXEC = 4 }
}

impl Prot {
    pub const RW: Prot = Prot(Prot::READ.0 | Prot::WRITE.0);
    pub const RX: Prot = Prot(Prot::READ.0 | Prot::EXEC.0);
    pub const RWX: Prot = Prot(Prot::READ.0 | Prot::WRITE.0 | Prot::EXEC.0);
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("guest memory fault at {addr:#010x} (+{len})")]
pub struct MemFault {
    pub addr: u32,
    pub len: u32,
}

/// Physical page locking is optional and must never be inferred from residency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageLockError {
    Unsupported,
    /// A Win32 error from the backing page owner or its host pin operation.
    Win32(u32),
}

/// Why [`Backend::run`] returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// A trap handler returned [`TrapAction::Park`]. EIP is at the park address and the rest
    /// of the guest state is exactly what the handler left.
    Parked,
    /// A single run exhausted its host wall-clock budget. PC is the stopped guest address.
    BudgetExceeded { pc: u32, budget: Duration },
    /// A host notification stopped at a resumable CPU boundary. No guest work
    /// or scheduler budget is charged for the notification itself.
    Interrupted { pc: u32 },
    /// The guest faulted (unmapped access, invalid instruction, ...). EIP is the faulting pc.
    Fault(Fault),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    pub kind: FaultKind,
    pub pc: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FaultKind {
    Read(u32),
    Write(u32),
    Fetch(u32),
    InvalidInstruction,
    /// Backend-specific error text.
    Other(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrapAction {
    /// Resume the guest at the current EIP (the handler may have changed registers/memory).
    Continue,
    /// Stop the guest and return [`Exit::Parked`] from [`Backend::run`].
    Park,
}

/// Canonical guest floating-point control state, independent of the host FPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FpuEnvironment {
    pub x87_control: u16,
    pub mxcsr: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum FpuEnvironmentError {
    #[error("backend does not support the guest floating-point environment")]
    Unsupported,
    #[error("MXCSR contains reserved bits: {0:#x}")]
    InvalidMxcsr(u32),
    #[error("backend floating-point environment access failed: {0}")]
    Backend(String),
}

impl FpuEnvironment {
    /// The virtual x86 profile exposes every architectural low MXCSR bit.
    pub const MXCSR_MASK: u32 = 0xffff;

    pub fn validate(self) -> Result<(), FpuEnvironmentError> {
        if self.mxcsr & !Self::MXCSR_MASK != 0 {
            Err(FpuEnvironmentError::InvalidMxcsr(self.mxcsr))
        } else {
            Ok(())
        }
    }
}

/// Registers and memory as seen from inside a trap handler.
pub trait Guest {
    /// Whether the loader should retain native-cache identity and AOT metadata.
    fn wants_code_images(&self) -> bool {
        false
    }
    /// Register mapped image metadata and optional static-discovery bytes.
    /// Backends without image compilation can ignore this loader hint.
    fn register_code_image(&mut self, _image: crate::AotImage) {}
    /// Establish an exclusive guest-memory boundary for composite atomics or
    /// mapping-sensitive host operations. Synchronous backends need no action.
    fn quiesce_memory(&mut self) {}
    /// Pin the actual backing storage, without a per-page reference count.
    /// Backends without an owned, physically pinnable window reject the request.
    fn lock_pages(&mut self, _addr: u32, _len: u32) -> Result<(), PageLockError> {
        Err(PageLockError::Unsupported)
    }
    fn unlock_pages(&mut self, _addr: u32, _len: u32) -> Result<(), PageLockError> {
        Err(PageLockError::Unsupported)
    }
    /// Used to reject unsupported decommit without leaving a successful pin live.
    fn has_locked_pages(&self, _addr: u32, _len: u32) -> bool {
        false
    }
    /// Process teardown releases pins without retiring report-readable memory.
    fn retire_locked_pages(&mut self) -> Result<(), PageLockError> {
        Ok(())
    }
    /// Capture architecture at a paused boundary, materializing floating aliases.
    /// This contains no scheduler, REP continuation, clock or host IRQ metadata.
    fn capture_architecture(&mut self) -> Result<ArchitecturalState, StateError> {
        Err(StateError::Unsupported)
    }
    /// Validate all supported fields before committing any architectural change.
    fn restore_architecture(&mut self, _state: &ArchitecturalState) -> Result<(), StateError> {
        Err(StateError::Unsupported)
    }
    /// Materialize deferred floating aliases and read canonical control state.
    fn fpu_environment(&mut self) -> Result<FpuEnvironment, FpuEnvironmentError> {
        Err(FpuEnvironmentError::Unsupported)
    }
    /// Invalid MXCSR leaves both controls unchanged. Changing x87 masks preserves
    /// exception flags, tags and registers, while recomputing pending ES/B status.
    /// An unchanged x87 control word preserves the complete original x87 state.
    fn set_fpu_environment(
        &mut self,
        _environment: FpuEnvironment,
    ) -> Result<(), FpuEnvironmentError> {
        Err(FpuEnvironmentError::Unsupported)
    }
    /// Most recent committed indirect transfer for fetch-fault diagnostics.
    /// Backends without this observation return None.
    fn last_indirect_branch(&self) -> Option<crate::IndirectBranch> {
        None
    }
    /// Virtual CPU feature bits from CPUID leaf 1 EDX. API layers must use
    /// these capabilities rather than detecting the host CPU or assuming a backend.
    fn cpuid_features(&self) -> u32 {
        0
    }
    /// Push an IEEE float onto the guest x87 stack, as a cdecl floating return does.
    fn push_f32(&mut self, value: f32) {
        self.push_f64(f64::from(value));
    }
    /// Push an IEEE double without reducing it to single precision.
    fn push_f64(&mut self, _value: f64) {
        unimplemented!("backend does not support x87 doubles")
    }
    /// Pop ST(0), converting extended precision to a host double. An empty stack is None.
    fn pop_f64(&mut self) -> Option<f64> {
        None
    }
    fn reg(&self, r: Reg) -> u32;
    fn set_reg(&mut self, r: Reg, v: u32);
    fn read(&self, addr: u32, buf: &mut [u8]) -> Result<(), MemFault>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), MemFault>;
    /// Map a dedicated zeroed PCM allocation and return an independently owned
    /// read-only alias. Unsupported backends reject this explicitly. Guest
    /// protection/unmapping never invalidates the returned read view.
    fn map_shared_audio(&mut self, addr: u32, len: u32) -> Result<SharedRegion, MemFault> {
        Err(MemFault { addr, len })
    }
    /// Map zeroed execution-owner words; failure MUST leave mappings unchanged.
    fn map_shared_counters(
        &mut self,
        addr: u32,
        len: u32,
    ) -> Result<std::rc::Rc<dyn CounterStorage>, MemFault> {
        Err(MemFault { addr, len })
    }
    /// Map `len` bytes of host file `fd` from `offset` at `addr`, shared with
    /// every other mapping of the file. Data only: `prot` is READ, with WRITE
    /// for a writable descriptor.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    fn map_shared_file(
        &mut self,
        addr: u32,
        len: u32,
        _fd: std::os::fd::BorrowedFd<'_>,
        _offset: u64,
        _prot: Prot,
    ) -> Result<(), MemFault> {
        Err(MemFault { addr, len })
    }
    /// Alias worker-confined file data pages. COPY views share reads until the
    /// first write to each page, then retain a private page. No executable views.
    fn map_file_pages(
        &mut self,
        addr: u32,
        pages: &[std::rc::Rc<FilePage>],
        _prot: Prot,
        _copy_on_write: bool,
    ) -> Result<(), MemFault> {
        Err(MemFault {
            addr,
            len: u32::try_from(pages.len().saturating_mul(file_page::FILE_PAGE_SIZE))
                .unwrap_or(u32::MAX),
        })
    }
    /// Whether a file page still awaits its first private COPY write.
    fn file_page_copy_on_write(&self, _addr: u32) -> Option<bool> {
        None
    }
    /// A counter that advances whenever an unwritten private copy page is
    /// written or unmapped. None means the backend cannot report it.
    fn private_copy_epoch(&self) -> Option<u64> {
        None
    }
    /// Map an owned prepared snapshot with a per-page first-write barrier.
    /// Unlike file aliases these private pages may execute, and their source
    /// cannot be changed externally. Protection changes never reset the barrier.
    fn map_private_copy_pages(
        &mut self,
        addr: u32,
        bytes: &[u8],
        _prot: Prot,
    ) -> Result<(), MemFault> {
        Err(MemFault {
            addr,
            len: u32::try_from(bytes.len()).unwrap_or(u32::MAX),
        })
    }
    /// Convert prepared snapshot pages to ordinary private ownership without
    /// writing bytes; protection changes do not perform this conversion.
    fn privatize_copy_pages(&mut self, addr: u32, len: u32) -> Result<(), MemFault> {
        Err(MemFault { addr, len })
    }
    /// Optional stable shared bytes while this guest borrow is alive. Software devices
    /// can consume frames without a second guest-to-host copy; other backends return None.
    fn shared_bytes(&self, _addr: u32, _len: usize) -> Option<&[u8]> {
        None
    }
    /// Mutable ordinary guest bytes while this exclusive borrow is alive.
    /// Shared/file pages, missing WRITE permission and code-watched pages
    /// return None; callers retain the normal `write` fallback. Producing this
    /// borrow does not relocate storage or change mappings, so disjoint raw
    /// input pointers from `shared_bytes` remain valid during it.
    fn writable_bytes(&mut self, _addr: u32, _len: usize) -> Option<&mut [u8]> {
        None
    }
    /// Identify immutable file input, retaining its storage independently of mappings.
    fn pinned(&self, _addr: u32, _len: usize) -> Option<Pinned> {
        None
    }
    /// Revalidate identity after mapping changes; unchanged generations need no walk.
    fn still_pinned(&self, _pinned: &Pinned) -> bool {
        false
    }
    /// Cumulative memory work on the guest execution worker.
    fn host_access(&self) -> HostAccess {
        HostAccess::default()
    }
    /// Retained contiguous read-only file input, independent of the guest VM.
    fn read_only_file_view(&self, _addr: u32, _len: usize) -> Option<ReadOnlyFileView> {
        None
    }
    /// Overlap-safe guest memory copy. Backends with shared storage can override this.
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        let mut bytes = vec![0; len];
        self.read(src, &mut bytes)?;
        self.write(dst, &bytes)
    }
    /// Set `len` bytes at `addr` to `byte`, as guest stores would: the
    /// whole range must be writable, and nothing is written otherwise.
    fn fill_memory(&mut self, addr: u32, byte: u8, len: usize) -> Result<(), MemFault> {
        let fault = MemFault {
            addr,
            len: u32::try_from(len).unwrap_or(u32::MAX),
        };
        if !self.accessible(addr, fault.len, Prot::WRITE) || fault.len as usize != len {
            return Err(fault);
        }
        let chunk = [byte; 4096];
        let mut at = 0;
        while at < len {
            let count = (len - at).min(chunk.len());
            self.write(addr.wrapping_add(at as u32), &chunk[..count])?;
            at += count;
        }
        Ok(())
    }
    /// Whether guest instructions may access the entire range with `prot`. Unlike host
    /// reads/writes, this includes page permissions. Unknown backends conservatively reject.
    fn accessible(&self, _addr: u32, _len: u32, _prot: Prot) -> bool {
        false
    }
    /// First actually mapped page in `[addr, limit)`, including pages with no
    /// access permission and lazy file bindings. Bounds must be page-aligned.
    /// Query the backend's existing mapping metadata without reading guest bytes.
    /// Unsupported backends reject rather than claim an unknown range is free.
    fn next_mapped_page(&self, addr: u32, _limit: u64) -> Result<Option<u32>, MemFault> {
        Err(MemFault { addr, len: 0 })
    }
    /// Map zeroed memory. Page-aligned `addr` and `size`. Allowed from inside trap handlers.
    fn map(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault>;
    fn protect(&mut self, addr: u32, size: u32, prot: Prot) -> Result<(), MemFault>;
    fn unmap(&mut self, addr: u32, size: u32) -> Result<(), MemFault>;
}

/// Convenience accessors over [`Guest`].
pub trait GuestExt: Guest {
    fn read_u8(&self, addr: u32) -> Result<u8, MemFault> {
        let mut b = [0; 1];
        self.read(addr, &mut b)?;
        Ok(b[0])
    }
    fn read_u16(&self, addr: u32) -> Result<u16, MemFault> {
        let mut b = [0; 2];
        self.read(addr, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }
    fn read_u32(&self, addr: u32) -> Result<u32, MemFault> {
        let mut b = [0; 4];
        self.read(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    fn write_u32(&mut self, addr: u32, v: u32) -> Result<(), MemFault> {
        self.write(addr, &v.to_le_bytes())
    }
    /// NUL-terminated byte string, at most `max` bytes (the terminator is not included).
    fn read_cstr(&self, addr: u32, max: usize) -> Result<Vec<u8>, MemFault> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 64];
        while out.len() < max {
            // Don't cross a page boundary with one read: the next page may be unmapped.
            let a = addr.wrapping_add(out.len() as u32);
            let n = chunk
                .len()
                .min(0x1000 - (a as usize & 0xfff))
                .min(max - out.len());
            self.read(a, &mut chunk[..n])?;
            if let Some(end) = chunk[..n].iter().position(|&b| b == 0) {
                out.extend_from_slice(&chunk[..end]);
                return Ok(out);
            }
            out.extend_from_slice(&chunk[..n]);
        }
        Ok(out)
    }
    fn push_u32(&mut self, v: u32) -> Result<(), MemFault> {
        let sp = self.reg(Reg::Esp).wrapping_sub(4);
        self.write_u32(sp, v)?;
        self.set_reg(Reg::Esp, sp);
        Ok(())
    }
    /// The `i`th stack argument of a call trapped at function entry (return address at [esp]).
    fn arg(&self, i: u32) -> Result<u32, MemFault> {
        self.read_u32(self.reg(Reg::Esp).wrapping_add(4 + 4 * i))
    }
}

impl<G: Guest + ?Sized> GuestExt for G {}

pub trait TrapHandler {
    /// Preparation only: admit the complete offered loop body. This is never
    /// called by generated code; the returned capability obeys NativeRegion's
    /// safety contract and is invalidated with the translation's code bytes.
    fn native_region(&self, _pc: u32, _code: &[u8]) -> Vec<crate::native_region::NativeRegion> {
        Vec::new()
    }

    /// The thunk whose host call the exact PUSH token / INT pair at `pc`
    /// makes, so translated code may call it directly.
    fn host_thunk(&self, _pc: u32, _word: u32) -> Option<u32> {
        None
    }

    /// `on_trap` for the INT 0x2e of a pair `host_thunk` recognized as
    /// `thunk`, its token still pushed.
    fn on_host_call(&mut self, cpu: &mut dyn Guest, _thunk: u32) -> TrapAction {
        self.on_trap(cpu, 0x2e)
    }

    /// Where execution continues instead of entering the function at `pc`,
    /// while the bytes there are still the returned ones (A64-CTL-14). The
    /// target must behave as the function does.
    fn redirect(&self, _pc: u32) -> Option<(u32, &[u8])> {
        None
    }

    /// The direct body that runs in place of the function at `pc`, which
    /// `redirect` also redirects (A64-CALL-2).
    fn direct_body(&self, _pc: u32) -> Option<crate::body::DirectBody> {
        None
    }

    /// Required host services also replace their entry in interpreted runs.
    fn required_redirect(&self, _pc: u32) -> Option<(u32, &[u8])> {
        None
    }

    /// The execution owner's guest clock, when one is installed. CPU reference
    /// counters use the same domain as guest OS time; this is not device time.
    /// A standalone backend may use its own reference clock when absent.
    fn guest_time_us(&mut self) -> Option<u64> {
        None
    }

    /// `vector` is the interrupt number (0x2e for thunks; CPU exceptions use their own vectors).
    fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction;
}

/// Flat 32-bit segments with a per-context FS base (TEB).
#[derive(Clone, Copy, Debug)]
pub struct SegmentSetup {
    /// Guest address of one page reserved for descriptor tables. The GDT uses
    /// the first 128 bytes; the upper half holds a zeroed, non-present IDT.
    /// Actual guest exceptions use the owner's exception pipeline, not ring 0
    /// handlers installed in this table.
    pub gdt_addr: u32,
}

impl SegmentSetup {
    pub const IDT_OFFSET: u32 = 0x800;
    pub const IDT_LIMIT: u16 = 256 * 8 - 1;
}

pub trait Backend: Guest {
    /// Snapshot only at an explicitly enabled host frame boundary.
    fn frame_counters(&self) -> Option<crate::frame_counters::FrameCounters> {
        None
    }
    /// A fresh address space with the same execution policy. No memory or JIT
    /// blocks may be aliased by this constructor. Optional for test backends.
    fn new_process(&mut self) -> Result<Self, String>
    where
        Self: Sized,
    {
        Err("backend does not support independent guest processes".into())
    }
    type Context;

    /// Install the guest-owned GDT/IDT and load flat CS/DS/ES/SS before running.
    fn setup_segments(&mut self, seg: SegmentSetup) -> Result<(), MemFault>;
    /// Point FS at `base` (the current context's TEB).
    fn set_fs_base(&mut self, base: u32) -> Result<(), MemFault>;

    /// The address guest code jumps to when a handler parks it. It is a mapped, never-executed
    /// location; `run` returns [`Exit::Parked`] before executing it.
    fn park_addr(&self) -> u32;

    fn new_context(&self) -> Self::Context;
    fn save(&self, ctx: &mut Self::Context);
    fn restore(&mut self, ctx: &Self::Context);

    /// Set the host wall-clock limit for each run (including in-hook host calls). `None`
    /// disables it. This clock is independent of guest time; expiration stops execution,
    /// without modifying the stopped guest registers or choosing another context. This is
    /// a watchdog, not a hard real-time deadline: host scheduling may delay delivery and
    /// a running host handler must return before the guest can stop. A normal park or
    /// guest fault wins if it coincides with expiration. Zero stops before executing code.
    fn set_execution_budget(&mut self, budget: Option<Duration>);

    /// The current host wall-clock allowance; read without altering hooks or CPU state.
    fn execution_budget(&self) -> Option<Duration>;

    /// Permit resumable host-notification exits for a sliced run. Backends that
    /// do not support host interrupts keep their existing synchronous behavior.
    fn set_host_interrupts(&mut self, _enabled: bool) {}

    fn host_interrupt_pending(&self) -> bool {
        false
    }

    /// Run from the current EIP until a handler parks, the budget expires, or the guest faults.
    fn run(&mut self, handler: &mut dyn TrapHandler) -> Exit;

    /// Drop any translated code overlapping `[addr, addr+len)` (after the host patches code).
    fn invalidate_code(&mut self, addr: u32, len: u32);

    /// Guest instructions completed so far, telling loop iterations apart
    /// (PROC-CLOCK-5). Zero when the backend does not count them.
    fn retired_instructions(&self) -> u64 {
        0
    }

    /// Count executed translation blocks (measurement only; it slows execution).
    fn set_block_counting(&mut self, _on: bool) {}

    /// Blocks executed and their guest code bytes since counting was first enabled.
    fn executed_blocks(&self) -> Option<(u64, u64)> {
        None
    }

    /// Also count executions per block start (measurement only; implies block counting).
    fn set_block_profile(&mut self, _on: bool) {}

    /// (block start, executions, block size in bytes), unordered.
    fn block_profile(&self) -> Vec<(u32, u64, u32)> {
        Vec::new()
    }
}
