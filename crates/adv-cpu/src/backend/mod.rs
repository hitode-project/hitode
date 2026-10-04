//! Guest x86-32 CPU interface.
//!
//! The runtime never single-steps the guest. It maps memory, sets up a context, and calls
//! [`Backend::run`]. Guest code runs until it executes a trap instruction (`int 0x2e` in a
//! thunk) or faults. On a trap the backend calls [`TrapHandler::on_trap`] from inside the
//! emulation loop. The handler either finishes the call on the spot ([`TrapAction::Continue`]:
//! the fast path for leaf API calls) or asks the backend to park the guest
//! ([`TrapAction::Park`]). Parking unwinds out of `run`, so only the top-level scheduler
//! ever re-enters guest code. There is no host→guest recursion.

use std::time::Duration;

pub use crate::{ArchitecturalState, StateError};
use thiserror::Error;

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
pub mod jit;
pub use file_page::FilePage;
pub mod x87;
pub use adv_sync::shared::{SharedReadView, SharedRegion};

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

/// Why [`Backend::run`] returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// CPU state has been leased to a worker. The controller must not restore
    /// this context until poll_parallel returns the matching token.
    Dispatched { token: u64 },
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
    /// Establish an exclusive guest-memory boundary for composite atomics or
    /// mapping-sensitive host operations. Synchronous backends need no action.
    fn quiesce_memory(&mut self) {}
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
    /// Optional stable shared bytes while this guest borrow is alive. Software devices
    /// can consume frames without a second guest-to-host copy; other backends return None.
    fn shared_bytes(&self, _addr: u32, _len: usize) -> Option<&[u8]> {
        None
    }
    /// Overlap-safe guest memory copy. Backends with shared storage can override this.
    fn copy_memory(&mut self, src: u32, dst: u32, len: usize) -> Result<(), MemFault> {
        let mut bytes = vec![0; len];
        self.read(src, &mut bytes)?;
        self.write(dst, &bytes)
    }
    /// Whether guest instructions may access the entire range with `prot`. Unlike host
    /// reads/writes, this includes page permissions. Unknown backends conservatively reject.
    fn accessible(&self, _addr: u32, _len: u32, _prot: Prot) -> bool {
        false
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
    /// Guest address of one page reserved for the GDT. Never touched by guest code.
    pub gdt_addr: u32,
}

pub trait Backend: Guest {
    type Context;

    fn guest_workers(&self) -> usize {
        1
    }
    fn parallel_capacity_available(&self) -> bool {
        true
    }
    /// Offload only when another ready context can use the inline lane.
    fn set_parallel_demand(&mut self, _ready_peer: bool) {}
    fn poll_parallel(&mut self) -> Option<(u64, Self::Context)> {
        None
    }
    fn wait_parallel(&mut self, _duration: Duration) -> Result<(), String> {
        Err("backend has no parallel execution workers".into())
    }

    /// Install the GDT and load flat CS/DS/ES/SS. Must be called once before running.
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
