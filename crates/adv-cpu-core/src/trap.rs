//! Raw CPU trap boundary shared by native and interpreted execution.
use crate::{CpuState, Memory};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// A caller-owned trap receiver. API decoding and all host continuation state
/// belong to the machine and ABI layers, outside the execution layer.
pub trait ExecutionTrap {
    /// The body has committed its results and its return epilogue.
    fn body_yield(&mut self, _state: &mut CpuState, _memory: &mut Memory) -> Result<bool, String> {
        Ok(false)
    }

    /// Shared service storage retained by the execution owner for this run.
    fn body_services(&self) -> crate::body::Services {
        crate::body::Services::default()
    }

    /// Diagnostic modes affecting translated bindings; changes discard units.
    fn diagnostics(&self) -> u8 {
        0
    }

    /// See [`crate::backend::TrapHandler::native_region`].
    fn native_region(&self, _pc: u32, _code: &[u8]) -> Vec<crate::native_region::NativeRegion> {
        Vec::new()
    }

    /// See [`crate::backend::TrapHandler::host_thunk`].
    fn host_thunk(&self, _pc: u32, _word: u32) -> Option<u32> {
        None
    }

    /// `call` for the INT 0x2e of a pair `host_thunk` recognized as
    /// `thunk`, its token still pushed.
    fn host_call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        _thunk: u32,
    ) -> Result<bool, String> {
        self.call(state, memory, 0x2e)
    }

    /// See [`crate::backend::TrapHandler::redirect`].
    fn redirect(&self, _pc: u32) -> Option<(u32, &[u8])> {
        None
    }

    /// Required host services also replace their entry in interpreted runs.
    fn required_redirect(&self, _pc: u32) -> Option<(u32, &[u8])> {
        None
    }

    /// See [`crate::backend::TrapHandler::direct_body`].
    fn direct_body(&self, _pc: u32) -> Option<crate::body::DirectBody> {
        None
    }

    /// The execution owner's guest clock for RDTSC. This must not block,
    /// allocate, perform IO, lock, or panic. None uses the standalone counter.
    fn guest_time_us(&mut self) -> Option<u64> {
        None
    }

    /// Receive an interrupt with the CPU paused. Software INT has committed
    /// its next PC; CPU exceptions retain their faulting PC. True yields to
    /// the caller, which owns all decisions about waiting and re-entry.
    fn call(
        &mut self,
        state: &mut CpuState,
        memory: &mut Memory,
        vector: u32,
    ) -> Result<bool, String>;
}

pub struct Bridge<'a> {
    pub timestamp: crate::timestamp::Counter,
    pub memory: *mut Memory,
    pub error: Option<String>,
    pub trap: Option<(u32, u8)>,
    pub calls: u64,
    pub handler: Option<&'a mut dyn ExecutionTrap>,
    pub parked: bool,
    pub deadline: Option<std::time::Instant>,
    /// Whether bulk string work must read the wall clock before continuing:
    /// set by host calls of unknown duration and after CLOCK_BYTES of bulk
    /// work, so short REP operations do not each query the clock.
    pub clock_due: bool,
    pub unclocked_bytes: u64,
    pub bulk_calls: u64,
    pub bulk_bytes: u64,
    pub cliffs: Box<crate::Cliffs>,
    /// Operand buffer of the file-memory helpers; generated code reads and
    /// writes it around one synchronous call.
    pub scratch: Scratch,
}

#[derive(Default)]
#[repr(C, align(16))]
pub struct Scratch(pub [u8; 16]);

// Bulk work between wall-clock checks: tens of microseconds at memcpy rates.
const CLOCK_BYTES: u64 = 256 << 10;

impl Bridge<'_> {
    /// The wall deadline has passed. Reads the clock only when a check is due.
    pub fn expired(&mut self) -> bool {
        let Some(at) = self.deadline else {
            return false;
        };
        if !self.clock_due {
            return false;
        }
        if std::time::Instant::now() >= at {
            return true;
        }
        self.clock_due = false;
        self.unclocked_bytes = 0;
        false
    }

    pub fn charge_bulk(&mut self, bytes: u64) {
        self.unclocked_bytes += bytes;
        if self.unclocked_bytes >= CLOCK_BYTES {
            self.clock_due = true;
        }
    }

    pub fn read_timestamp(&mut self) -> u64 {
        let now = self.handler.as_mut().and_then(|h| h.guest_time_us());
        self.timestamp.read(now)
    }
}

/// SAFETY: generated code passes its live, exclusively borrowed Bridge. This
/// reads only the owner's clock/counter, never guest memory or architectural flags.
pub unsafe extern "C" fn read_timestamp(bridge: *mut u8) -> u64 {
    unsafe { &mut *bridge.cast::<Bridge<'_>>() }.read_timestamp()
}

pub fn call_trap(bridge: &mut Bridge<'_>, state: &mut CpuState, vector: u32) -> u32 {
    trap(bridge, state, vector, |handler, state, memory| {
        handler.call(state, memory, vector)
    })
}

/// `call_trap` for a host call `ExecutionTrap::host_thunk` recognized.
pub fn call_host(bridge: &mut Bridge<'_>, state: &mut CpuState, thunk: u32) -> u32 {
    trap(bridge, state, 0x2e, |handler, state, memory| {
        handler.host_call(state, memory, thunk)
    })
}

fn trap(
    bridge: &mut Bridge<'_>,
    state: &mut CpuState,
    vector: u32,
    call: impl FnOnce(&mut dyn ExecutionTrap, &mut CpuState, &mut Memory) -> Result<bool, String>,
) -> u32 {
    state.commit_mmx();
    let Some(handler) = bridge.handler.as_mut() else {
        bridge.trap = Some((state.eip, vector as u8));
        return 1;
    };
    let before = unsafe { &*bridge.memory }.epoch;
    let pc = state.eip;
    bridge.calls += 1;
    let result = catch_unwind(AssertUnwindSafe(|| {
        call(&mut **handler, state, unsafe { &mut *bridge.memory })
    }))
    .unwrap_or_else(|_| Err("CPU trap handler panicked".into()));
    bridge.clock_due = true;
    match result {
        Ok(parked) => {
            bridge.parked = parked;
            if parked
                || state.eip != pc
                || state.eflags & (1 << 8) != 0
                || state.x87.status & 0x80 != 0
                || unsafe { &*bridge.memory }.epoch != before
            {
                2
            } else {
                0
            }
        }
        Err(error) => {
            bridge.error = Some(error);
            1
        }
    }
}

/// C ABI called by native INT 0x2e. No panic crosses generated code.
pub unsafe extern "C" fn trap_call(
    bridge: *mut std::ffi::c_void,
    state: *mut CpuState,
    pc: u32,
) -> u32 {
    // SAFETY: Executor creates both objects for this run and pauses native code here.
    let (bridge, state) = unsafe { (&mut *bridge.cast::<Bridge<'_>>(), &mut *state) };
    state.settle_flags();
    if bridge.handler.is_none() {
        bridge.trap = Some((pc, 0x2e));
        return 1;
    }
    call_trap(bridge, state, 0x2e)
}
