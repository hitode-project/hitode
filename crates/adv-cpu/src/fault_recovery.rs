//! Explicit scalar fault recovery for frozen Linux/Android native executions.
//! The signal boundary only captures context and redirects to a same-frame stub;
//! `recover_current` completes at most one access in ordinary execution context.
use crate::CpuState;
use crate::execution_abi::{
    HostContextSnapshot, NativeRecoveryPoint, RecoveryAccess, RecoveryValue, StoreCommitProof,
};
use crate::fault_recipe::HostRegisters;
use crate::virtual_memory::{DirectFaultView, FaultClassification, GuestAccess, Reservation};
use std::any::Any;
use std::cell::{Cell, UnsafeCell};
use std::ffi::c_void;
use std::sync::OnceLock;

const MAX_REGIONS: usize = 64;
const MAX_SITES: usize = 4096;
const GPR_COUNT: u32 = if cfg!(target_arch = "aarch64") {
    31
} else {
    16
};
const SIMD_COUNT: u32 = if cfg!(target_arch = "aarch64") {
    32
} else {
    16
};

type Dispatch = unsafe extern "C" fn(i32, usize, usize, usize, *mut c_void) -> usize;
unsafe extern "C" {
    fn adv_posix_fault_install(dispatch: Dispatch) -> i32;
    fn adv_posix_fault_gpr(context: *const c_void, register: u32) -> u64;
    fn adv_posix_fault_flags(context: *const c_void) -> u64;
    fn adv_posix_fault_fp(
        context: *const c_void,
        control: *mut u32,
        status: *mut u32,
        vectors: *mut u8,
    ) -> i32;
}

impl HostRegisters for HostContextSnapshot {
    fn gpr(&self, register: u8) -> Option<u64> {
        (u32::from(register) < self.gpr_count).then(|| self.gpr[usize::from(register)])
    }
}

/// Keeps finalized executable storage and its exact-PC side table borrowed.
pub struct CodeRegion<'a> {
    start: usize,
    end: usize,
    sites: &'a [NativeRecoveryPoint],
    _owner: &'a dyn Any,
}
impl<'a> CodeRegion<'a> {
    /// # Safety
    /// `owner` owns the finalized code range. Each exact machine-PC site must
    /// describe the still-live architectural prefix and an ordinary same-frame
    /// recovery stub. `GuestRegister`/`GuestFlags` sources must remain canonical.
    /// Loads of 1/2/4 bytes zero-extend to the complete 32-bit GPR; signed
    /// extension and partial-register destinations must use a different path.
    /// Store publishers prove private pinned backing and all-or-none behavior
    /// for the actual scalar instruction; this API does not infer that proof.
    pub unsafe fn new(
        owner: &'a dyn Any,
        start: usize,
        len: usize,
        sites: &'a [NativeRecoveryPoint],
    ) -> Result<Self, String> {
        let end = start.checked_add(len).ok_or("native code range overflow")?;
        if len == 0
            || sites.len() > MAX_SITES
            || sites
                .windows(2)
                .any(|pair| pair[0].host_pc >= pair[1].host_pc)
            || sites.iter().any(|site| {
                site.host_pc < start
                    || site.host_pc >= end
                    || site.recovery_pc < start
                    || site.recovery_pc >= end
                    || !valid(site)
            })
        {
            return Err("invalid or unsupported scalar recovery table".into());
        }
        Ok(Self {
            start,
            end,
            sites,
            _owner: owner,
        })
    }
    fn site(&self, pc: usize) -> Option<&NativeRecoveryPoint> {
        if pc < self.start || pc >= self.end {
            return None;
        }
        self.sites
            .binary_search_by_key(&pc, |site| site.host_pc)
            .ok()
            .and_then(|index| self.sites.get(index))
    }
}

fn valid_source(source: RecoveryValue) -> bool {
    match source {
        RecoveryValue::GuestRegister(index) => index < 8,
        RecoveryValue::HostRegister(index) => u32::from(index) < GPR_COUNT,
        _ => true,
    }
}
fn valid(site: &NativeRecoveryPoint) -> bool {
    matches!(site.width, 1 | 2 | 4)
        && site.registers.iter().copied().all(valid_source)
        && [site.flags, site.address, site.value]
            .into_iter()
            .all(valid_source)
        && match (site.access, site.proof, site.destination) {
            (RecoveryAccess::Read, StoreCommitProof::LoadBeforeCommit, Some(index)) => index < 8,
            (RecoveryAccess::Write, StoreCommitProof::AlignedPrivateScalar, None) => true,
            _ => false,
        }
}

/// The result of completing the one interrupted access, after restoring its
/// register prefix. Code writes require the owner's normal invalidation path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryKind {
    Completed,
    AccessViolation,
    CodeWriteProtection,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryOutcome {
    pub kind: RecoveryKind,
    pub guest_pc: u32,
    pub address: u32,
    pub committed_units: u32,
    pub value: u32,
}

/// A session returns normally through the native epilogue and its Rust caller.
pub struct SessionResult<R> {
    pub result: R,
    pub recoveries: u64,
    pub last: Option<RecoveryOutcome>,
    pub snapshot: Option<HostContextSnapshot>,
}

struct Pending {
    site: *const NativeRecoveryPoint,
    snapshot: HostContextSnapshot,
}
struct Session<'a> {
    state: *mut CpuState,
    regions: &'a [CodeRegion<'a>],
    view: DirectFaultView<'a>,
    window: usize,
    window_len: usize,
    guest_base: u32,
    host_page_size: usize,
    pending: UnsafeCell<Pending>,
    has_pending: Cell<bool>,
    recovering: Cell<bool>,
    recoveries: Cell<u64>,
    last: Cell<Option<RecoveryOutcome>>,
}
thread_local! {
    static ACTIVE: Cell<*mut Session<'static>> = const { Cell::new(std::ptr::null_mut()) };
}
struct ActiveScope(*mut Session<'static>);
impl Drop for ActiveScope {
    fn drop(&mut self) {
        ACTIVE.with(|slot| slot.set(self.0));
    }
}

fn install() -> Result<(), String> {
    static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| {
            if unsafe { adv_posix_fault_install(dispatch) } == 0 {
                Err(format!(
                    "install scalar fault handlers: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                Ok(())
            }
        })
        .clone()
}

/// Explicitly opts into recovery while code, metadata, and direct mappings are
/// frozen. Ordinary checked/default execution never installs this session.
///
/// # Safety
/// The closure runs only the borrowed code and never exposes a Rust reference
/// into `state` while native code can mutate it. Recovery stubs return through
/// their own frame. They call `recover_current` outside signal context, either
/// in that frame or immediately after its epilogue in this closure. No guest
/// effect or use of the interrupted result may precede that completion.
/// A store proof covers the actual private backing for this entire closure.
/// The caller materializes `state.fuel` before entering the registered segment;
/// `committed_units` reports its prefix and is never subtracted a second time.
pub unsafe fn with_session<R>(
    state: &mut CpuState,
    regions: &[CodeRegion<'_>],
    window: &Reservation,
    guest_base: u32,
    run: impl FnOnce(*mut CpuState) -> R,
) -> Result<SessionResult<R>, String> {
    if regions.len() > MAX_REGIONS {
        return Err("too many frozen fault regions".into());
    }
    let view = window
        .direct_fault_view()
        .ok_or("fault recovery requires direct mappings")?;
    let host_page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if host_page_size <= 0 {
        return Err("query host page size".into());
    }
    install()?;
    let mut session = Session {
        state,
        regions,
        view,
        window: window.ptr.as_ptr() as usize,
        window_len: window.len,
        guest_base,
        host_page_size: host_page_size as usize,
        pending: UnsafeCell::new(Pending {
            site: std::ptr::null(),
            snapshot: HostContextSnapshot::default(),
        }),
        has_pending: Cell::new(false),
        recovering: Cell::new(false),
        recoveries: Cell::new(0),
        last: Cell::new(None),
    };
    let previous = ACTIVE.with(|slot| slot.replace((&mut session as *mut Session<'_>).cast()));
    let scope = ActiveScope(previous);
    let result = run(session.state);
    drop(scope);
    if session.has_pending.get() {
        return Err("native recovery stub did not complete its pending access".into());
    }
    let recoveries = session.recoveries.get();
    Ok(SessionResult {
        result,
        recoveries,
        last: session.last.get(),
        snapshot: (recoveries > 0).then(|| unsafe { (*session.pending.get()).snapshot }),
    })
}

fn value(source: RecoveryValue, state: &CpuState, snapshot: &HostContextSnapshot) -> Option<u32> {
    match source {
        RecoveryValue::GuestRegister(index) => state.regs.get(usize::from(index)).copied(),
        RecoveryValue::HostRegister(index) => snapshot.gpr(index).map(|value| value as u32),
        RecoveryValue::GuestFlags => Some(state.eflags),
        RecoveryValue::Constant(value) => Some(value),
    }
}

unsafe fn capture(context: *const c_void, pc: usize) -> Option<HostContextSnapshot> {
    let mut snapshot = HostContextSnapshot {
        pc: pc as u64,
        gpr_count: GPR_COUNT,
        simd_count: SIMD_COUNT,
        architecture: if cfg!(target_arch = "aarch64") { 2 } else { 1 },
        ..HostContextSnapshot::default()
    };
    for index in 0..GPR_COUNT {
        snapshot.gpr[index as usize] = unsafe { adv_posix_fault_gpr(context, index) };
    }
    snapshot.sp =
        unsafe { adv_posix_fault_gpr(context, if cfg!(target_arch = "aarch64") { 31 } else { 4 }) };
    snapshot.flags = unsafe { adv_posix_fault_flags(context) };
    if unsafe {
        adv_posix_fault_fp(
            context,
            &mut snapshot.fpcr,
            &mut snapshot.fpsr,
            snapshot.simd.as_mut_ptr().cast(),
        )
    } == 0
    {
        return None;
    }
    Some(snapshot)
}

unsafe extern "C" fn dispatch(
    _signal: i32,
    pc: usize,
    fault_address: usize,
    access: usize,
    context: *mut c_void,
) -> usize {
    ACTIVE
        .try_with(|slot| {
            let pointer = slot.get();
            if pointer.is_null() || context.is_null() {
                return 0;
            }
            // SAFETY: the session is stack-stable on this interrupted thread, and
            // its borrowed code and view are frozen until the normal native return.
            let session = unsafe { &*pointer };
            if session.has_pending.get() || session.recovering.get() {
                return 0;
            }
            let Some(site) = session.regions.iter().find_map(|region| region.site(pc)) else {
                return 0;
            };
            let expected_access = match site.access {
                RecoveryAccess::Read => 0,
                RecoveryAccess::Write => 1,
            };
            if access != usize::MAX && access != expected_access {
                return 0;
            }
            session.has_pending.set(true);
            let Some(snapshot) = (unsafe { capture(context, pc) }) else {
                session.has_pending.set(false);
                return 0;
            };
            let Some(address) = value(site.address, unsafe { &*session.state }, &snapshot) else {
                session.has_pending.set(false);
                return 0;
            };
            let Some(offset) = address.checked_sub(session.guest_base) else {
                session.has_pending.set(false);
                return 0;
            };
            let width = usize::from(site.width);
            let Some(end_offset) = (offset as usize).checked_add(width) else {
                session.has_pending.set(false);
                return 0;
            };
            let Some(start) = session.window.checked_add(offset as usize) else {
                session.has_pending.set(false);
                return 0;
            };
            if end_offset > session.window_len
                || fault_address < start
                || fault_address - start >= width
                || (site.access == RecoveryAccess::Write
                    && (start % width != 0
                        || start / session.host_page_size
                            != (start + width - 1) / session.host_page_size))
            {
                session.has_pending.set(false);
                return 0;
            }
            let guest_access = match site.access {
                RecoveryAccess::Read => GuestAccess::Read,
                RecoveryAccess::Write => GuestAccess::Write,
            };
            if matches!(
                session.view.classify(fault_address, guest_access),
                FaultClassification::OutsideGuest | FaultClassification::Accessible
            ) {
                session.has_pending.set(false);
                return 0;
            }
            // SAFETY: only this signal boundary writes Pending. No owning value or
            // guest memory access is involved, and the reentry flag was set first.
            unsafe {
                *session.pending.get() = Pending { site, snapshot };
            }
            site.recovery_pc
        })
        .unwrap_or(0)
}

/// Complete one captured scalar operation in ordinary execution context.
/// A same-frame stub calls this after the signal handler has returned.
///
/// # Safety
/// Must be called by the published recovery stub or its immediate continuation
/// within `with_session`, with no outstanding Rust reference into CpuState.
pub unsafe fn recover_current() -> Option<RecoveryOutcome> {
    ACTIVE
        .try_with(|slot| {
            let pointer = slot.get();
            if pointer.is_null() {
                return None;
            }
            let session = unsafe { &*pointer };
            if !session.has_pending.get() || session.recovering.replace(true) {
                return None;
            }
            let pending = unsafe { &*session.pending.get() };
            let site = unsafe { &*pending.site };
            let state = unsafe { &mut *session.state };
            // Stage every input before publishing any canonical field. In
            // particular, a GuestRegister EA/value may alias a recovered output.
            let regs = std::array::from_fn(|index| {
                value(site.registers[index], state, &pending.snapshot).unwrap()
            });
            let flags = value(site.flags, state, &pending.snapshot).unwrap();
            let address = value(site.address, state, &pending.snapshot).unwrap();
            let store_value = value(site.value, state, &pending.snapshot).unwrap();
            state.regs = regs;
            state.eflags = flags;
            state.eip = site.guest_pc;
            state.fault_addr = address;
            let offset = (address - session.guest_base) as usize;
            let access = match site.access {
                RecoveryAccess::Read => GuestAccess::Read,
                RecoveryAccess::Write => GuestAccess::Write,
            };
            let classification = session.view.classify(session.window + offset, access);
            let mut outcome = RecoveryOutcome {
                kind: RecoveryKind::AccessViolation,
                guest_pc: site.guest_pc,
                address,
                committed_units: site.committed_units,
                value: store_value,
            };
            if classification == FaultClassification::CodeWriteProtection {
                outcome.kind = RecoveryKind::CodeWriteProtection;
            } else {
                let width = usize::from(site.width);
                let mut bytes = if site.access == RecoveryAccess::Read {
                    [0; 4]
                } else {
                    store_value.to_le_bytes()
                };
                let completed = match site.access {
                    RecoveryAccess::Read => session.view.checked_read(offset, &mut bytes[..width]),
                    RecoveryAccess::Write => session.view.checked_write(offset, &bytes[..width]),
                };
                if completed.is_ok() {
                    outcome.kind = RecoveryKind::Completed;
                    if let Some(destination) = site.destination {
                        outcome.value = u32::from_le_bytes(bytes);
                        state.regs[usize::from(destination)] = outcome.value;
                    }
                    state.eip = site.next_pc;
                    state.fault_addr = 0;
                }
            }
            session.last.set(Some(outcome));
            session.recoveries.set(session.recoveries.get() + 1);
            session.has_pending.set(false);
            session.recovering.set(false);
            Some(outcome)
        })
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests;
