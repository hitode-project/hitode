//! Translated execution on AArch64 hosts.
//!
//! Units come from `translate`. A dispatch does not leave translated code: the
//! `dispatch` stub calls `dispatch`, which finds or translates the target
//! unit and, for a static target, patches the link point into a direct
//! branch, so hot paths chain without even that call. Each unit's safepoint
//! checks the countdown, which `run` sets from the fuel and `SLICE`, so a
//! chain returns within its budget. A link is undone when its target is
//! dropped. Dynamic targets go through the indirect table, which the helper
//! fills, and returns through the shadow stack; both are emptied whenever a
//! unit is dropped, so they only ever lead to live units.
//!
//! Faults (A64-MEM-5, A64-MEM-9): every host instruction that may fault has a side-table
//! site naming its guest instruction. The signal handler only checks that the
//! PC is such a site, records it and v8-v23, and resumes
//! at the fault stub. The owner publishes the site's dirty SIMD prefix before
//! interpreting the instruction. Because a
//! translated instruction commits nothing before its last possible fault
//! (A64-FLAG-3), the owner then rebuilds the flags from the site's carry and
//! PF/AF recipe and interprets that one instruction from the exact prior
//! state: the interpreter either completes it (a soft page) or reports the
//! precise guest fault.
//!
//! Soft pages (A64-MEM-6): an instruction whose unchecked sites fault
//! `CHECK_AFTER` times is checked from then on. The unit that faulted goes,
//! and its next translation checks each access of the instruction against
//! the page class table derived from `Memory::native_permissions`. Both tables belong to Memory; native execution keeps
//! pointers, without retaining Arc snapshots that would force whole-table copies.
//! Mapping changes and code discovery refresh changed pages before reentry.
//!
//! Code invalidation: translation registers its guest bytes with
//! `watch_code`, which write-protects them in the window, so any store to
//! them reaches the interpreter or Rust and marks the page dirty. Dirty
//! pages drop their units before the next dispatch.
//!
//! Proved computation uses a second entry emitted from the same trace.
//! Its outer transfers return complete state through fixed exits, and its
//! faults use a private capture and an immutable unit table. The owner
//! keeps the code and mapping stable while its computation session is open.

use iced_x86::Code;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64};

use adv_cpu_core::backend::OfferGate;
use adv_cpu_core::interpreter::{Execution, Step, accessible};
use adv_cpu_core::memory::{Memory, READ, WRITE};
use adv_cpu_core::trap::{Bridge, call_host, call_trap};
use adv_cpu_core::{CpuState, IndirectBranch, IndirectBranchKind};

use crate::cache::CodeCache;
use crate::encode::{b, bl};
use crate::runtime::{self, Exit, ResidentCold, Runtime};
use crate::translate::{self, Features, Operand, Sense, Site, Stubs};

mod compile;
mod head;
mod offload;
mod run;
mod store;
#[cfg(test)]
mod testing;

/// Offer gates for runs whose handler publishes none: no offer is possible,
/// or Rust decides every arrival.
static CLEAR_GATE: OfferGate = OfferGate {
    stale: AtomicU32::new(0),
    split_over: AtomicU32::new(0),
    split_deadline: AtomicU64::new(u64::MAX),
};
static STALE_GATE: OfferGate = OfferGate {
    stale: AtomicU32::new(1),
    split_over: AtomicU32::new(0),
    split_deadline: AtomicU64::new(u64::MAX),
};

const CACHE_LEN: usize = 64 << 20;

/// Most guest instructions one `run` completes, so the owner still sees
/// deadlines and interrupts while linked units loop.
const SLICE: u64 = runtime::COMPUTATION_SLICE as u64;
/// Budget exits at one EIP that promote it: a few slices of owner time.
pub(crate) const PROMOTION: u8 = 4;

type Enter = unsafe extern "C" fn(*mut Runtime, usize);

fn alias_base(memory: &Memory) -> u64 {
    (memory.ptr.as_ptr() as usize).wrapping_sub(memory.base as usize) as u64
}

/// An indirect table entry no EIP matches.
const EMPTY: [u64; 2] = [u64::MAX, 0];

/// Doubling copies fill the 1 MiB table with a few memcpys; an element-wise
/// fill dominates executor creation in unoptimized test builds.
fn empty_table() -> Box<[[u64; 2]]> {
    let mut table = Vec::with_capacity(runtime::TABLE_LEN);
    table.push(EMPTY);
    while table.len() < runtime::TABLE_LEN {
        table.extend_from_within(..table.len().min(runtime::TABLE_LEN - table.len()));
    }
    table.into_boxed_slice()
}

/// Faults at unchecked sites after which an instruction is checked (A64-MEM-6).
pub(crate) const CHECK_AFTER: u32 = 8;

/// Page class table entries: every guest page.
const CLASSES: usize = 1 << 20;

/// Which guest accesses check their page first (A64-MEM-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Checks {
    /// Those of instructions that faulted `CHECK_AFTER` times.
    Adaptive,
    /// All of them, for tests.
    All,
    /// All of them, every one through the owner, for tests.
    Slow,
}

/// What the signal handler may read while translated code runs.
struct Faults {
    /// Executable range of translated code.
    start: usize,
    end: Cell<usize>,
    /// Host PC of each site, ascending: units are appended in address
    /// order, and their sites in emission order.
    sites: Vec<(usize, Site)>,
    stub: usize,
    capture: FaultCapture,
    /// The shadow stack's guard pages.
    guard: (usize, usize),
    /// Where an overflow resumes, and the runtime field it reads the
    /// faulting PC from.
    reset: usize,
    resume: *mut u64,
}

#[derive(Default)]
struct FaultCapture {
    /// The site that faulted, written in signal context.
    pc: Cell<usize>,
    /// v8-v23 captured before the fault stub restores host SIMD.
    simd: Cell<[[u8; 16]; 16]>,
}

fn site_at(sites: &[(usize, Site)], pc: usize) -> Option<Site> {
    let index = sites.binary_search_by_key(&pc, |&(at, _)| at).ok()?;
    Some(sites[index].1)
}

impl Faults {
    fn site(&self, pc: usize) -> Option<Site> {
        site_at(&self.sites, pc)
    }
}

#[derive(Clone, Copy)]
enum Active {
    None,
    Owner(*const Faults),
    Worker(*const offload::Faults<'static>),
}

thread_local! {
    /// The `Faults` of the code this thread is running, or null.
    static ACTIVE: Cell<Active> = const { Cell::new(Active::None) };
}

impl FaultCapture {
    fn record(
        &self,
        site: Site,
        pc: usize,
        stub: usize,
        snapshot: &adv_cpu_core::execution_abi::HostContextSnapshot,
    ) -> usize {
        self.simd.set(std::array::from_fn(|n| snapshot.simd[8 + n]));
        self.pc.set(pc);
        if site.resume != 0 {
            // The unit publishes its cached x87 state, then faults.
            pc.wrapping_add_signed(site.resume as isize)
        } else {
            stub
        }
    }

    fn recover(&self, cpu: &mut CpuState, site: Site, temps: [u64; 2]) {
        recover(cpu, site, temps);
        for (n, bytes) in self.simd.get().into_iter().enumerate() {
            if site.simd_dirty & (1 << n) == 0 {
                continue;
            }
            if n < 8 {
                let value = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                cpu.mmx[n] = value;
                cpu.x87.regs[n] = adv_cpu_core::X87Register::from_parts(value, 0xffff);
            } else {
                cpu.xmm[n - 8] = bytes;
            }
        }
    }
}

/// Runs in signal context: no allocation, no locks.
unsafe fn on_fault(
    pc: usize,
    address: usize,
    snapshot: &adv_cpu_core::execution_abi::HostContextSnapshot,
) -> usize {
    ACTIVE
        .try_with(|slot| {
            let faults = match slot.get() {
                Active::None => return 0,
                Active::Worker(faults) => {
                    // SAFETY: the worker keeps its private capture and unit
                    // side table alive while its entry is active on this thread.
                    let faults = unsafe { &*faults };
                    return faults.record(pc, snapshot);
                }
                Active::Owner(faults) => faults,
            };
            // SAFETY: `run` keeps the pointee alive and unmodified, apart
            // from the `pc` and `simd` cells, until it clears the slot.
            let faults = unsafe { &*faults };
            if !(faults.start..faults.end.get()).contains(&pc) {
                return 0;
            }
            if (faults.guard.0..faults.guard.1).contains(&address) {
                // A shadow push overflowed: empty the stack and retry it.
                // SAFETY: the runtime outlives the run, and translated code
                // is stopped here.
                unsafe { faults.resume.write(pc as u64) };
                return faults.reset;
            }
            let Some(site) = faults.site(pc) else {
                return 0;
            };
            faults.capture.record(site, pc, faults.stub, snapshot)
        })
        .unwrap_or(0)
}

/// The shadow return stack: entries of {host continuation, guest EIP}
/// growing down from sentinels that never match, over guard pages.
struct ShadowStack {
    base: *mut libc::c_void,
    len: usize,
    guard: usize,
}

impl ShadowStack {
    const ENTRIES: usize = 1 << 16;

    fn new() -> Result<Self, String> {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let guard = page.max(16 << 10);
        let len = guard + (Self::ENTRIES + runtime::SHADOW_SENTINELS) * 16;
        let len = len.next_multiple_of(page);
        // SAFETY: a fresh anonymous mapping.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err("cannot map the shadow stack".into());
        }
        let stack = Self { base, len, guard };
        // SAFETY: inside the mapping just made.
        let protected = unsafe {
            libc::mprotect(
                base.cast::<u8>().add(guard).cast(),
                len - guard,
                libc::PROT_READ | libc::PROT_WRITE,
            )
        };
        if protected != 0 {
            return Err("cannot protect the shadow stack".into());
        }
        let top = stack.top() as *mut u64;
        for k in 0..runtime::SHADOW_SENTINELS {
            // SAFETY: the sentinels are the last entries of the mapping.
            unsafe {
                top.add(2 * k).write(0);
                top.add(2 * k + 1).write(u64::MAX);
            }
        }
        Ok(stack)
    }

    /// The empty stack pointer, just below the sentinels.
    fn top(&self) -> u64 {
        (self.base as usize + self.len - runtime::SHADOW_SENTINELS * 16) as u64
    }

    fn guard(&self) -> (usize, usize) {
        (self.base as usize, self.base as usize + self.guard)
    }
}

impl Drop for ShadowStack {
    fn drop(&mut self) {
        // SAFETY: the mapping `new` made.
        unsafe { libc::munmap(self.base, self.len) };
    }
}

/// Host features from the OS.
fn features() -> Features {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        const HWCAP_FLAGM: u64 = 1 << 35;
        // SAFETY: getauxval has no preconditions.
        let hwcap = unsafe { libc::getauxval(libc::AT_HWCAP) } as u64;
        Features {
            flagm: hwcap & HWCAP_FLAGM != 0,
        }
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&value);
        // SAFETY: a valid name, and an output buffer of `len` bytes.
        let found = unsafe {
            libc::sysctlbyname(
                c"hw.optional.arm.FEAT_FlagM".as_ptr(),
                (&raw mut value).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        } == 0;
        Features {
            flagm: found && value != 0,
        }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    unit: usize,
    retired: u32,
}

struct Linked {
    point: usize,
    original: u32,
    next: u32,
    back: bool,
    direct: Option<usize>,
    sample: Option<(usize, u8)>,
}

impl Linked {
    fn branch(&self, target: usize) -> u32 {
        if self.original & 0xFC00_0000 == 0x9400_0000 {
            bl((target as i64 - self.point as i64) as i32)
        } else {
            head::branch(self.point, target)
        }
    }
}

struct InternalEdge {
    point: usize,
    target: usize,
    transition: usize,
    link: usize,
    source: u32,
    source_pc: u32,
    back: bool,
    sample: Option<(usize, u8)>,
}

/// One trip through translated code.
/// How runs ended and what the cache went through, for reports.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Diagnostics {
    pub first_visits: u64,
    pub first_declined: u64,
    pub empty: u64,
    pub over_fuel: u64,
    pub interprets: u64,
    pub faults: u64,
    pub dispatches: u64,
    pub dropped_units: u64,
    pub clears: u64,
    /// Entries promoted to loop traces.
    pub promotions: u64,
    /// Empty units translated, by `Fetcher::failure`.
    pub empty_units: [u64; 4],
    /// Checked accesses `access` performed and refused.
    pub accesses: u64,
    pub refused: u64,
    /// Faults at checked sites, refusals included.
    pub checked_faults: u64,
    /// Instructions `interpret` ran, and the runs it left.
    pub helpers: u64,
    pub helper_exits: u64,
    /// Proofs whose head loop splits, and how its blocks ran.
    pub split_proofs: u64,
    pub split: offload::SplitCounts,
    /// Detached optimized forms entered by the ordinary owner run loop.
    #[cfg_attr(not(test), expect(dead_code, reason = "reported by Debug"))]
    pub optimized_runs: u64,
    /// Admitted resident entries when the JIT map is enabled.
    pub resident_entries: u64,
    /// Resident form exits that went through Rust (A64-OPT-9).
    pub resident_cold_exits: u64,
    /// Fills of the whole indirect table, which also reset the shadow stack.
    pub global_invalidations: u64,
}

pub(crate) struct Run {
    pub exit: Exit,
    /// Guest instructions completed.
    pub retired: u32,
    /// Bytes translated on the way, zero on a cache hit.
    pub translated: usize,
}

/// What `dispatch` works on during one `run`.
struct Owner {
    native: *mut Native,
    memory: *mut Memory,
    /// The run's trap `Bridge`, or null when traps go to the owner.
    bridge: *mut std::ffi::c_void,
    translated: usize,
    error: Option<String>,
    discovered: Option<u32>,
    internal_discovered: Vec<(u32, u32, CpuState)>,
}

unsafe extern "C" fn discover_internal(runtime: *mut Runtime, eip: u32, point: usize) {
    // SAFETY: the threshold stub publishes the head's state and keeps the
    // live Tier-1 registers on its stack while the owner records the sample.
    let (owner, mut at) = unsafe {
        (
            &mut *((*runtime).owner as *mut Owner),
            (*(*runtime).cpu).clone(),
        )
    };
    let native = unsafe { &mut *owner.native };
    let source = native.code.internal_edges[&eip]
        .iter()
        .find(|edge| edge.point == point)
        .unwrap()
        .source;
    at.eip = eip;
    owner.internal_discovered.push((eip, source, at));
    native.stop_counting(eip);
}

/// Called by the `dispatch` stub with the guest EIP and the link point to
/// patch, or 0. Returns the unit to continue at, or 0 to leave.
unsafe extern "C" fn dispatch(runtime: *mut Runtime, eip: u32, point: usize) -> usize {
    // SAFETY: `run` points `owner` at a live `Owner` whose pointees it does
    // not touch while translated code runs.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    // An indirect transfer missed the table: record it as the interpreter
    // would, so a jump to nowhere names its source.
    let branch = std::mem::take(unsafe { &mut (*runtime).branch });
    if branch != 0 {
        // SAFETY: the runtime's CPU is live while units run.
        let cpu = unsafe { &mut *(*runtime).cpu };
        record_branch(cpu, branch, eip);
    }
    if owner.error.is_some() {
        return 0;
    }
    if native.is_stop(eip) {
        return 0;
    }
    let back = native.code.back_points.contains(&point);
    if back && native.counters[&eip].get() == 0 {
        native.stop_counting(eip);
        owner.discovered = Some(eip);
        return 0;
    }
    // A first visit runs in the owner; the point links on the next one.
    let entry = native.code.units.get(&eip).copied();
    if entry.is_none()
        && native.code.direct_points.contains_key(&point)
        && let Some(head) = native.heads.get(&eip)
        && head.trampoline != 0
        && !head.unprofitable
    {
        native.patch(point, eip, head.tier1);
        native.code.cache.sync();
        return point;
    }
    if entry.is_none()
        && (!crate::translate::Fetcher::fetchable(memory, eip) || native.first_visit(eip))
    {
        return 0;
    }
    let before = native.code.cache.used();
    let unit = match native.link(memory, point, eip, entry) {
        Ok(unit) => unit,
        Err(error) => {
            owner.error = Some(error);
            return 0;
        }
    };
    native.resolve_tables(memory, eip, native.code.units[&eip].unit);
    if point == 0 {
        native.code.table[runtime::table_slot(eip)] = [u64::from(eip), unit as u64];
    }
    owner.translated += native.code.cache.used().saturating_sub(before);
    native.code.cache.sync();
    native.code.faults.end.set(native.code.cache.next());
    unit
}

fn record_branch(cpu: &mut CpuState, branch: u64, target: u32) {
    if branch != 0 {
        cpu.last_indirect_branch = IndirectBranch {
            source: branch as u32,
            target,
            kind: match branch >> 32 {
                1 => IndirectBranchKind::Call,
                2 => IndirectBranchKind::Jump,
                _ => IndirectBranchKind::Return,
            },
        };
    }
}

/// Called by the `access` stub for a checked access of `kind` the window
/// cannot take: performs it as the interpreter would, returning a scalar
/// or transferring a vector through `Runtime::access_vector`. Refusal records
/// `site` as faulting, supplies its publication continuation in
/// `Runtime::access_resume` and sets bit 32. Stores to
/// code are refused too, so the interpreter invalidates the code.
unsafe extern "C" fn access(
    runtime: *mut Runtime,
    address: u32,
    site: usize,
    value: u32,
    kind: u32,
) -> u64 {
    // SAFETY: as in `dispatch`.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    native.diagnostics.accesses += 1;
    let width = (kind & 31) as usize;
    // SAFETY: the runtime is live; the cold path shares this transfer field.
    let vector = unsafe { &mut (*runtime).access_vector };
    let done = if kind & runtime::ACCESS_STORE != 0 {
        let scalar = value.to_le_bytes();
        let bytes = if width > 4 {
            &vector[..width]
        } else {
            &scalar[..width]
        };
        (accessible(memory, address, width as u8, WRITE)
            && !memory.stores_code(address, width)
            && memory.write(address, bytes).is_ok())
        .then_some(0)
    } else if width > 4 {
        (accessible(memory, address, width as u8, READ)
            && memory.read_into(address, &mut vector[..width]).is_ok())
        .then_some(0)
    } else {
        let mut bytes = [0; 4];
        (accessible(memory, address, width as u8, READ)
            && memory.read_into(address, &mut bytes[..width]).is_ok())
        .then(|| {
            let shift = 32 - 8 * width as u32;
            let value = u32::from_le_bytes(bytes) << shift;
            if kind & runtime::ACCESS_SIGNED != 0 {
                ((value as i32) >> shift) as u32
            } else {
                value >> shift
            }
        })
    };
    match done {
        Some(value) => u64::from(value),
        None => {
            native.diagnostics.refused += 1;
            native.code.faults.capture.pc.set(site);
            let resume = native.code.faults.site(site).map_or(0, |site| site.resume);
            // SAFETY: the runtime is live and translated code is in this helper.
            unsafe {
                (*runtime).access_resume = if resume != 0 {
                    site.wrapping_add_signed(resume as isize) as u64
                } else {
                    0
                };
            }
            1 << 32
        }
    }
}

/// Called by the `interpret` stub: runs helper `index` with the guest in
/// `CpuState`, a REP for at most the countdown left and the instruction
/// prepaid. Continues if it completed and changed no code or mapping;
/// otherwise leaves to dispatch where it went, or with the step for the
/// owner to complete, refunding the instruction (`Runtime::interpret`).
unsafe extern "C" fn interpret(runtime: *mut Runtime, index: u32, countdown: u64) -> u64 {
    const LEAVE: u32 = Exit::Dispatch as u32 + 1;
    // SAFETY: as in `dispatch`; the runtime's CPU is live while units run.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    let cpu = unsafe { &mut *(*runtime).cpu };
    let helper = &native.code.helpers[index as usize];
    cpu.eip = helper.pc;
    native.diagnostics.helpers += 1;
    native.code.helper_counts[index as usize] += 1;
    let (epoch, dirty) = (memory.epoch, memory.dirty_pages.len());
    let units = countdown.saturating_add(1).min(u64::from(u32::MAX)) as u32;
    let count = cpu.regs[1];
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        helper.prepared.execute(cpu, memory, units)
    }));
    if let (Some(forms), Ok(execution)) = (&mut native.retired_forms, &run) {
        let (retired, elements) = helper
            .prepared
            .retirement(*execution, cpu, helper.pc, count);
        forms[2] += retired;
        forms[4] += elements;
    }
    let (status, charge) = match run {
        Ok(Execution {
            step: Step::Executed,
            units,
        }) => {
            let same = memory.epoch == epoch && memory.dirty_pages.len() == dirty;
            let status = if same && cpu.eip == helper.next && cpu.eflags & 0x100 == 0 {
                0
            } else {
                LEAVE
            };
            (status, i64::from(units) - 1)
        }
        // A host call returns here unless it moved the guest elsewhere,
        // changed memory, parked or failed (`call_trap`).
        Ok(Execution {
            step: Step::Trap { vector, .. },
            units,
        }) if !owner.bridge.is_null() => {
            // SAFETY: the executor's bridge outlives the run and nothing
            // else uses it while translated code runs.
            let bridge = unsafe { &mut *owner.bridge.cast::<Bridge<'_>>() };
            let resumes = host(
                native,
                memory,
                bridge,
                cpu,
                (epoch, dirty),
                helper.next,
                |b, c| call_trap(b, c, u32::from(vector)),
            );
            let status = if resumes { 0 } else { LEAVE };
            (status, i64::from(units) - 1)
        }
        Ok(execution) => {
            native.pending = Some(execution);
            (Exit::Stepped as u32 + 1, -1)
        }
        Err(_) => {
            owner.error = Some(format!("the interpreter panicked at {:#x}", helper.pc));
            (LEAVE, -1)
        }
    };
    if native.retired_forms.is_some() {
        native.helper_units += (charge + 1) as u64;
    }
    if status != 0 {
        native.diagnostics.helper_exits += 1;
    }
    (charge as u64) << 32 | u64::from(status)
}

/// Called by the `call` stub: EIP in bits 0-31, thunk in 32-47, INT length
/// in 48-51, and bit 63 for an unrecognized INT (A64-CTL-12).
unsafe extern "C" fn call(runtime: *mut Runtime, site: u64, _countdown: u64) -> u64 {
    const LEAVE: u32 = Exit::Dispatch as u32 + 1;
    // SAFETY: as in `interpret`.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    let cpu = unsafe { &mut *(*runtime).cpu };
    let pc = site as u32;
    if owner.bridge.is_null() {
        // Without a host the interpreter takes the INT, which is refunded.
        cpu.eip = pc;
        return (-1i64 as u64) << 32 | u64::from(Exit::Interpret as u32 + 1);
    }
    if native.pending_work() {
        cpu.eip = pc;
        return (-1i64 as u64) << 32 | u64::from(LEAVE);
    }
    // SAFETY: as in `interpret`.
    let bridge = unsafe { &mut *owner.bridge.cast::<Bridge<'_>>() };
    // The INT has committed its next PC.
    let next = pc.wrapping_add((site >> 48 & 15) as u32);
    cpu.eip = next;
    let before = (memory.epoch, memory.dirty_pages.len());
    let thunk = (site >> 32 & 0xffff) as u32;
    if host(native, memory, bridge, cpu, before, next, |b, c| {
        if site >> 63 != 0 {
            call_trap(b, c, 0x2e)
        } else {
            call_host(b, c, thunk)
        }
    }) {
        0
    } else {
        native.diagnostics.helper_exits += 1;
        u64::from(LEAVE)
    }
}

/// Makes a host call from translated code with `call`, the guest in `cpu`.
/// True continues at `next`: the call returned there and changed no code
/// or mapping, and no deadline, interrupt or stop is due. Otherwise the
/// unit leaves for wherever the call went.
fn host(
    native: &mut Native,
    memory: &mut Memory,
    bridge: &mut Bridge<'_>,
    cpu: &mut CpuState,
    (epoch, dirty): (u64, usize),
    next: u32,
    call: impl FnOnce(&mut Bridge<'_>, &mut CpuState) -> u32,
) -> bool {
    // Generated accesses are paused. Let host mapping changes update the
    // raw permissions in place instead of copying the whole table.
    let status = call(bridge, cpu);
    if status != 0
        && let Some(word) = &native.interrupt
    {
        word.fetch_or(
            adv_sync::pending::WORK,
            std::sync::atomic::Ordering::Release,
        );
    }
    status == 0
        && memory.epoch == epoch
        && memory.dirty_pages.len() == dirty
        && cpu.eip == next
        && !native.pending_work()
        && !native.past()
}

/// Read the owner's counter without synchronizing guest architectural state.
unsafe extern "C" fn timestamp(runtime: *mut Runtime) -> u64 {
    // SAFETY: generated code runs with this owner's live bridge.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    // SAFETY: Executor installs its live bridge before every native entry.
    let clock = unsafe { (*owner.native).clock };
    unsafe { &mut *clock.cast::<Bridge<'_>>() }.read_timestamp()
}

/// The virtual counter, which reads in a few nanoseconds where
/// `Instant::now` takes tens.
fn ticks() -> u64 {
    let value: u64;
    // SAFETY: EL0 may read the virtual counter on every supported OS.
    unsafe { std::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack)) };
    value
}

/// `at` as a virtual counter value.
pub(crate) fn deadline(at: std::time::Instant) -> u64 {
    let frequency: u64;
    // SAFETY: as in `ticks`.
    unsafe { std::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency, options(nomem, nostack)) };
    let left = at.saturating_duration_since(std::time::Instant::now());
    let left = left.as_nanos() * u128::from(frequency) / 1_000_000_000;
    ticks().saturating_add(u64::try_from(left).unwrap_or(u64::MAX))
}

pub(crate) struct Native {
    code: store::CodeStore,
    pub(crate) images: std::rc::Rc<std::cell::RefCell<Vec<(u32, u32)>>>,
    bridge: *mut std::ffi::c_void,
    pub(crate) interrupt: Option<Arc<std::sync::atomic::AtomicU32>>,
    pub(crate) pending_mask: u32,
    /// Counter addresses survive unit replacement and link restoration.
    counters: FxHashMap<u32, Box<Cell<u32>>>,
    heads: FxHashMap<u32, head::Head>,
    #[cfg(test)]
    ordinary: FxHashSet<u32>,
    /// Entries that have run, so a return to one translates it.
    seen: FxHashSet<u32>,
    /// Whether entries run once before they are translated.
    pub(crate) first_visits: bool,
    pub(crate) checks: Checks,
    /// Faults at unchecked sites by guest instruction.
    strikes: FxHashMap<u32, u32>,
    /// Guest instructions whose accesses check their page first.
    checked: FxHashSet<u32>,
    /// Faults at unchecked sites so far.
    pub(crate) unchecked_faults: u64,
    /// The run's wall deadline as a virtual counter value.
    pub(crate) deadline: Option<u64>,
    /// Diagnostic counts of how runs end; see `Diagnostics`.
    pub(crate) diagnostics: Diagnostics,
    pub(crate) retired_forms: Option<[u64; 5]>,
    helper_units: u64,
    /// The memory epoch last classified, and the all-slow test table.
    classified_epoch: Option<u64>,
    classes: Option<Arc<Vec<u8>>>,
    #[cfg(test)]
    classifications: u64,
    /// The step an `Exit::Stepped` leaves the owner.
    pub(crate) pending: Option<Execution>,
    /// Stop addresses seen so far; units end before all of them.
    stops: Vec<u32>,
    /// Budget exits an entry needs before it is promoted; zero translates
    /// every entry with loop traces.
    pub(crate) promotion: u8,
    /// Budget exits by EIP, and the entries promoted to loop traces.
    samples: FxHashMap<u32, u8>,
    promoted: FxHashSet<u32>,
    count_resident_entries: bool,
    /// The frame's pace when the owner last reached a loop whose blocks
    /// split (A64-TR-9).
    pub(crate) pace: std::cell::Cell<Option<adv_cpu_core::backend::Pace>>,
    offload: offload::Offload,
    resident_params: Vec<u128>,
    /// Where `opt_exit` saves an inline form's spill cells.
    inline_saved: Vec<u128>,
    /// Builds optimized forms, which `install_optimized` adopts.
    compiler: compile::Compiler,
    pub(crate) features: Features,
    runtime: Box<Runtime>,
    /// Live timestamp bridge, including runs without a trap handler.
    pub(crate) clock: *mut std::ffi::c_void,
    /// The x87 control word of the CPU a run starts on.
    control: u16,
    pe: bool,
}

impl Native {
    /// `window` is the host address of guest address 0 in the guest's
    /// `GuestWindow`, which must outlive this.
    pub fn new(window: usize) -> Result<Self, String> {
        let mut cache = CodeCache::new(CACHE_LEN)?;
        adv_cpu_core::fault_recovery::install_translated(on_fault)?;
        unsafe extern "C" fn resident_access(
            runtime: *mut Runtime,
            address: u32,
            site: usize,
            value: u32,
            kind: u32,
        ) -> u64 {
            if kind & runtime::ACCESS_STORE != 0 {
                // SAFETY: the owner and memory are live as in `access`.
                let owner = unsafe { &*((*runtime).owner as *const Owner) };
                let memory = unsafe { &*owner.memory };
                let last = address.wrapping_add((kind & 31) - 1);
                if [address, last].into_iter().any(|at| {
                    memory.file_page_copy_on_write(at).is_some() && memory.shared_range(at, 1)
                }) {
                    return 1 << 32;
                }
            }
            // SAFETY: same owner and arguments as the Tier-1 stub.
            unsafe { access(runtime, address, site, value, kind) }
        }

        let mut stubs = runtime::stubs();
        let access_entry = stubs.code.len() * 4;
        stubs.code.extend(translate::opt::back::access_stub(
            resident_access as *const () as u64,
        ));
        let base = cache.append(&stubs.code).ok_or("stubs do not fit")?;
        crate::jit_map::record_stubs(
            base,
            base + 4 * stubs.code.len(),
            &[
                (stubs.enter, "enter"),
                (stubs.opt_enter, "opt_enter"),
                (stubs.exit, "exit"),
                (stubs.fault, "fault"),
                (stubs.dispatch, "dispatch"),
                (stubs.lookup, "lookup"),
                (stubs.lookup_slow, "lookup_slow"),
                (stubs.ret_miss, "ret_miss"),
                (stubs.shadow_reset, "shadow_reset"),
                (stubs.access, "access"),
                (access_entry, "resident_access"),
                (stubs.interpret, "interpret"),
                (stubs.call, "call"),
            ],
        );
        // SAFETY: `enter` and `opt_enter` follow the C calling convention with
        // the signature of `Enter`; see `runtime::emit_enter` and `emit_opt_enter`.
        let enter = unsafe { std::mem::transmute::<usize, Enter>(base + stubs.enter) };
        let opt_enter = unsafe { std::mem::transmute::<usize, Enter>(base + stubs.opt_enter) };
        let mut table = empty_table();
        let shadow = ShadowStack::new()?;
        let mut runtime = Box::new(Runtime::new(std::ptr::null_mut(), window as u64));
        runtime.table = table.as_mut_ptr() as u64;
        runtime.resident_access = (base + access_entry) as u64;
        runtime.resident_discover = discover_internal as *const () as u64;
        runtime.shadow_top = shadow.top();
        runtime.shadow = shadow.top();
        Ok(Self {
            code: store::CodeStore {
                rows: FxHashMap::default(),
                table_sources: FxHashSet::default(),
                stub_len: cache.used(),
                faults: Faults {
                    start: base,
                    end: Cell::new(cache.next()),
                    sites: Vec::new(),
                    stub: base + stubs.fault,
                    capture: FaultCapture::default(),
                    guard: shadow.guard(),
                    reset: base + stubs.shadow_reset,
                    resume: &raw mut runtime.resume,
                },
                cache,
                enter,
                opt_enter,
                stubs: Stubs {
                    exit: base + stubs.exit,
                    dispatch: base + stubs.dispatch,
                    lookup: base + stubs.lookup,
                    lookup_slow: base + stubs.lookup_slow,
                    ret_miss: base + stubs.ret_miss,
                    access: base + stubs.access,
                    interpret: base + stubs.interpret,
                    call: base + stubs.call,
                    fault: base + stubs.fault,
                    opt_exit: base + stubs.opt_exit,
                },
                units: FxHashMap::default(),
                pages: FxHashMap::default(),
                links: FxHashMap::default(),
                linked: Vec::new(),
                points: Vec::new(),
                back_points: FxHashSet::default(),
                direct_points: FxHashMap::default(),
                internal_edges: FxHashMap::default(),
                tier1_words: FxHashMap::default(),
                uses_permissions: false,
                placed: Vec::new(),
                work: translate::Workspace::default(),
                helpers: Vec::new(),
                helper_counts: Vec::new(),
                helper_eips: FxHashMap::default(),
                generation: 0,
                table,
                shadow,
            },
            images: Default::default(),
            bridge: std::ptr::null_mut(),
            interrupt: None,
            pending_mask: u32::MAX,
            counters: FxHashMap::default(),
            heads: FxHashMap::default(),
            #[cfg(test)]
            ordinary: FxHashSet::default(),
            seen: FxHashSet::default(),
            first_visits: true,
            checks: Checks::Adaptive,
            strikes: FxHashMap::default(),
            checked: FxHashSet::default(),
            unchecked_faults: 0,
            deadline: None,
            diagnostics: Diagnostics::default(),
            classified_epoch: None,
            #[cfg(test)]
            classifications: 0,
            classes: None,
            retired_forms: None,
            helper_units: 0,
            pending: None,
            stops: Vec::new(),
            promotion: PROMOTION,
            samples: FxHashMap::default(),
            promoted: FxHashSet::default(),
            count_resident_entries: crate::jit_map::enabled(),
            pace: std::cell::Cell::new(None),
            offload: offload::Offload::default(),
            resident_params: Vec::new(),
            inline_saved: Vec::new(),
            compiler: compile::Compiler::new(),
            features: features(),
            runtime,
            clock: std::ptr::null_mut(),
            control: 0,
            pe: false,
        })
    }

    pub(crate) fn count_forms(&mut self, enabled: bool) {
        self.retired_forms = enabled.then_some([0; 5]);
        if enabled {
            crate::jit_map::enable_memory();
        }
    }

    fn resident_cold_exit(&mut self, eip: u32, site: u32, reason: ResidentCold, checked: bool) {
        self.diagnostics.resident_cold_exits += 1;
        let head = self.heads.get_mut(&eip).unwrap();
        let count = head.cold_counts.entry((site, reason)).or_default();
        *count += 1;
        if count.is_power_of_two() {
            crate::jit_map::optimized(
                eip,
                format_args!("resident\tcold\t{site}\t{reason:?}\t{count}"),
            );
        }
        if *count >= u64::from(head::COLD_EXITS) && !head.unprofitable {
            match reason {
                ResidentCold::FrameDisjoint => self.refuse_resident_fact(eip, u32::MAX),
                ResidentCold::HostFault if !checked => {
                    if head.checked.insert(site) {
                        self.rebuild_resident_versions(eip);
                    }
                }
                _ => self.cut_resident_site(eip, site, reason),
            }
        }
    }

    pub(crate) fn set_body_services(&mut self, services: adv_cpu_core::body::Services) {
        self.runtime.body.services = services;
    }

    pub(crate) fn set_spin_yield(&mut self, enabled: bool) {
        self.runtime.spin_yield = u32::from(enabled);
    }

    pub(super) fn is_stop(&self, eip: u32) -> bool {
        self.stops.contains(&eip)
    }

    fn pending_work(&self) -> bool {
        self.interrupt.as_ref().is_some_and(|word| {
            word.load(std::sync::atomic::Ordering::Acquire) & self.pending_mask != 0
        })
    }

    /// Whether the run's deadline has passed.
    pub(crate) fn past(&self) -> bool {
        self.deadline.is_some_and(|at| ticks() >= at)
    }

    /// Keeps the loop at `head` in ordinary units, for tests of their
    /// mechanisms.
    #[cfg(test)]
    pub(crate) fn keep_ordinary(&mut self, head: u32) {
        self.ordinary.insert(head);
        if let Some(count) = self.counters.get(&head) {
            count.set(u32::MAX);
        }
    }

    #[cfg(test)]
    pub(crate) fn switch_counts(&self) -> (usize, usize) {
        (
            self.code.rows.values().map(Vec::len).sum(),
            self.code
                .rows
                .values()
                .flatten()
                .filter(|&&(_, point, stub)| {
                    let value = u64::from(self.code.cache.word(point))
                        | (u64::from(self.code.cache.word(point + 4)) << 32);
                    value != stub as u64
                })
                .count(),
        )
    }

    #[cfg(test)]
    pub(crate) fn discard_switch_target(&mut self, eip: u32) {
        assert!(self.drop_unit(eip));
        self.forget_dynamic();
    }

    #[cfg(test)]
    pub(crate) fn channel_batches(&self) -> u64 {
        self.runtime.channel_batches
    }

    #[cfg(test)]
    pub(crate) fn native_batches(&self) -> u64 {
        self.runtime.native_batches
    }

    #[cfg(test)]
    pub(crate) fn vector_regions(&self) -> u64 {
        self.runtime.vector_regions
    }

    #[cfg(test)]
    pub(crate) fn unit_address(&self, eip: u32) -> Option<usize> {
        self.code.units.get(&eip).map(|entry| entry.unit)
    }

    /// Guest instructions whose accesses check their page.
    pub(crate) fn checked_count(&self) -> usize {
        self.checked.len()
    }

    /// Counts a fault at an unchecked site of `eip`, at host address `pc`.
    /// From `CHECK_AFTER` on, `eip` is checked and a new unit supersedes
    /// the one holding `pc`.
    fn strike(&mut self, memory: &mut Memory, eip: u32, pc: usize) {
        let strikes = self.strikes.entry(eip).or_default();
        *strikes += 1;
        if *strikes < CHECK_AFTER {
            return;
        }
        self.checked.insert(eip);
        let index = self.code.placed.partition_point(|&(start, _)| start <= pc);
        let Some(&(start, entry)) = index.checked_sub(1).map(|index| &self.code.placed[index])
        else {
            return;
        };
        // A unit dropped before stays in `placed`.
        if self
            .code
            .units
            .get(&entry)
            .is_some_and(|unit| unit.unit == start)
        {
            self.replace_unit(memory, entry);
        }
    }

    /// Points the runtime at the page classes of the current permissions.
    fn classify(&mut self, memory: &mut Memory) {
        if self.classified_epoch == Some(memory.epoch) {
            return;
        }
        #[cfg(test)]
        {
            self.classifications += 1;
        }
        self.runtime.alias = alias_base(memory);
        self.runtime.alias_classes =
            (memory.perms.as_ptr() as usize).wrapping_sub((memory.base >> 12) as usize) as u64;
        self.runtime.code_lines = (memory.code_lines() as usize)
            .wrapping_sub((memory.base >> 12) as usize * size_of::<u64>())
            as u64;
        memory.fast.body_table((memory.base >> 12) as usize);
        // Drop this temporary Arc before a helper can modify the tables.
        drop(memory.native_permissions());
        self.runtime.classes = if self.checks == Checks::Slow {
            self.classes
                .get_or_insert_with(|| Arc::new(vec![0; CLASSES]))
                .as_ptr()
        } else {
            memory.fast.native_table((memory.base >> 12) as usize)
        } as u64;
        self.runtime.body = adv_cpu_core::body::BodyContext {
            window: self.runtime.window as *mut u8,
            alias: self.runtime.alias as *mut u8,
            classes: if self.checks == Checks::Slow {
                self.runtime.classes as *const u8
            } else {
                memory.fast.body_table((memory.base >> 12) as usize)
            },
            services: self.runtime.body.services,
        };
        self.classified_epoch = Some(memory.epoch);
    }

    fn counter(&mut self, eip: u32) -> (u64, u32) {
        let count = self.counters.entry(eip).or_insert_with(|| {
            let value = head::HOT;
            #[cfg(test)]
            let value = if self.ordinary.contains(&eip) {
                u32::MAX
            } else {
                value
            };
            Box::new(value.into())
        });
        (std::ptr::from_ref(&**count) as u64, count.get())
    }

    fn bind_counter(&mut self, at: usize, address: u64) {
        self.code.cache.patch(at, address as u32);
        self.code.cache.patch(at + 4, (address >> 32) as u32);
    }

    fn stop_counting(&mut self, eip: u32) {
        self.counters[&eip].set(u32::MAX);
        self.repoint_resident_edges(eip, None);
    }

    fn discover(&mut self, memory: &mut Memory, eip: u32, source: u32, at: CpuState) {
        let mut trace = self.loop_trace(memory, eip, &self.stops);
        trace.x86 = trace
            .insts
            .iter()
            .map(|inst| {
                let mut bytes = [0; 15];
                let len = inst.next_pc.wrapping_sub(inst.pc) as usize;
                let bytes = bytes.get_mut(..len)?;
                memory.read_into(inst.pc, bytes).ok()?;
                let decoded = iced_x86::Decoder::with_ip(32, bytes, u64::from(inst.pc), 0).decode();
                (!decoded.is_invalid() && decoded.len() == len).then_some(decoded)
            })
            .collect();
        for &(start, len) in &trace.code {
            memory.watch_code(start, len as usize);
        }
        self.watch_pages(eip, &trace.code);
        let code = self.code.units[&source].unit;
        self.heads
            .insert(eip, head::Head::new(code, source, trace.detached()));
        self.request_resident(eip, compile::Task::Discover, at);
    }

    fn request_resident(&mut self, eip: u32, task: compile::Task, mut at: CpuState) {
        at.refresh_mmx();
        let head = self.heads.get_mut(&eip).unwrap();
        head.pending = Some(task);
        if let Some(last) = head.versions.last() {
            last.slot.limit.set(0);
        }
        let mut checked = self.checked.clone();
        checked.extend(&head.checked);
        self.compiler.request(compile::Request {
            eip,
            generation: self.code.generation,
            code: head.code,
            task,
            trace: head.trace.detached(),
            proof: self.offload.proof(eip),
            at,
            checked,
            all_checked: self.checks != Checks::Adaptive,
            refused: head.refused.clone(),
            cuts: head.cuts.clone(),
            count_entries: self.count_resident_entries,
        });
    }

    fn resident_arrival(&mut self, cpu: &CpuState) {
        let point = self.runtime.resident_point;
        if point == 0 {
            return;
        }
        let eip = cpu.eip;
        let head = self.heads.get_mut(&eip).unwrap();
        if head.rebuilds != 0 || matches!(head.pending, Some(compile::Task::Replace(_))) {
            return;
        }
        if point == 1 {
            if head.pending.is_none() && head.versions.len() < head::VERSIONS {
                self.request_resident(eip, compile::Task::Append, cpu.clone());
            }
            return;
        }
        let (patch, sample) = if let Some(edge) = self
            .code
            .internal_edges
            .get_mut(&eip)
            .and_then(|edges| edges.iter_mut().find(|edge| edge.point == point))
        {
            (edge.link, &mut edge.sample)
        } else {
            let mut next = self.code.links[&eip];
            while self.code.linked[next as usize].point != point {
                next = self.code.linked[next as usize].next;
            }
            let edge = &mut self.code.linked[next as usize];
            let patch = edge.direct.map_or(point, |transition| transition + 4);
            (patch, &mut edge.sample)
        };
        let version = head.versions.iter().position(|version| {
            cpu.rep.is_none()
                && cpu.mmx_mode == 0
                && translate::opt::admit_into(
                    &version.sources,
                    &version.assumed,
                    cpu,
                    &mut self.resident_params,
                    |_, _| 0,
                )
        });
        let mut patched = false;
        if let Some(version) = version {
            let count = sample
                .filter(|&(last, _)| last == version)
                .map_or(1, |(_, count)| count + 1);
            *sample = Some((version, count));
            if count == 2 {
                self.code
                    .cache
                    .patch(patch, head::branch(patch, head.versions[version].entry));
                patched = true;
            }
        } else {
            *sample = None;
        }
        head.probes -= 1;
        if head.probes == 0 {
            self.code.cache.patch(
                head.trampoline,
                head::branch(head.trampoline, head.versions[0].entry),
            );
            patched = true;
        }
        if patched {
            self.code.cache.sync();
        }
    }

    fn environment_miss(&mut self) {
        let event = self.runtime.resident_env;
        if event == 0 {
            return;
        }
        let eip = (event >> 32) as u32;
        let index = ((event >> 8) & 0xff) as usize - 1;
        let head = self.heads.get_mut(&eip).unwrap();
        let version = &head.versions[index];
        let fact = event as u8 as usize;
        version.slot.env[fact].set(0);
        let site = version.plain_sites[fact];
        self.refuse_resident_fact(eip, site);
    }

    fn refuse_resident_fact(&mut self, eip: u32, site: u32) {
        let head = self.heads.get_mut(&eip).unwrap();
        if head.refused.insert(site) {
            for (i, version) in head.versions.iter().enumerate() {
                if version.plain_sites.contains(&site) {
                    head.rebuilds |= 1 << i;
                }
            }
            self.rebuild_resident(eip);
        }
    }

    fn cut_resident_site(&mut self, eip: u32, site: u32, reason: ResidentCold) {
        let head = self.heads.get_mut(&eip).unwrap();
        if head.cuts.insert(site) {
            if head
                .trace
                .reaches_latch_without(&|pc| head.cuts.contains(&pc))
            {
                self.rebuild_resident_versions(eip);
            } else {
                crate::jit_map::declined(eip, reason);
                self.demote_resident(eip);
            }
        }
    }

    fn rebuild_resident_versions(&mut self, eip: u32) {
        let head = self.heads.get_mut(&eip).unwrap();
        head.rebuilds = (1 << head.versions.len()) - 1;
        self.rebuild_resident(eip);
    }

    fn rebuild_resident(&mut self, eip: u32) {
        let head = self.heads.get_mut(&eip).unwrap();
        if head.unprofitable || head.rebuilds == 0 {
            return;
        }
        let (trampoline, tier1) = (head.trampoline, head.tier1);
        // A sampled link may bypass the trampoline. Send those links
        // through it too until all stale versions have been replaced.
        self.code
            .cache
            .patch(trampoline, head::branch(trampoline, tier1));
        self.repoint_resident_edges(eip, Some(trampoline));
        let head = self.heads.get_mut(&eip).unwrap();
        if head.pending.is_none() {
            let index = head.rebuilds.trailing_zeros() as usize;
            head.rebuilds &= !(1 << index);
            let at = head.versions[index].at.clone();
            self.request_resident(eip, compile::Task::Replace(index), at);
        }
    }

    fn install_optimized(&mut self, wait: bool) {
        while let Some(done) = self.compiler.next(wait) {
            let eip = done.eip;
            if done.generation != self.code.generation {
                continue;
            }
            if done.task == compile::Task::Job {
                if !self.finish_job(eip, done.code, done.job) {
                    continue;
                }
                if let Some(head) = self.heads.get_mut(&eip) {
                    head.pending = None;
                    if head.versions.len() < head::VERSIONS
                        && let Some(last) = head.versions.last()
                    {
                        last.slot
                            .limit
                            .set(last.slot.misses.get().wrapping_add(head::MISSES));
                    }
                    self.rebuild_resident(eip);
                }
                continue;
            }
            let Some(head) = self
                .heads
                .get_mut(&eip)
                .filter(|head| head.code == done.code)
            else {
                continue;
            };
            head.pending = None;
            if let Some(job) = done.job {
                let trace = head.trace.detached();
                self.place_job(eip, &done.at, trace, job);
            }
            let head = &self.heads[&eip];
            if !head.unprofitable
                && (done.task != compile::Task::Append || head.rebuilds == 0)
                && !done
                    .resident
                    .is_some_and(|form| self.install_resident(eip, done.task, done.at, form))
                && matches!(done.task, compile::Task::Replace(_))
            {
                self.demote_resident(eip);
            }
            let head = &self.heads[&eip];
            if done.task == compile::Task::Discover && head.trampoline == 0 {
                self.repoint_resident_edges(eip, None);
            }
            self.rebuild_resident(eip);
        }
    }

    fn install_resident(
        &mut self,
        eip: u32,
        task: compile::Task,
        at: CpuState,
        form: compile::ResidentForm,
    ) -> bool {
        let compile::ResidentForm { built, mut lowered } = form;
        let source = self.heads[&eip].source;
        let mut probation = None;
        if lowered.iteration_retired != 0
            && let Some(words) = self.code.tier1_words.get(&source)
            && let Some(tier1) = lowered
                .iteration_pcs
                .iter()
                .try_fold(0u32, |sum, pc| words.get(pc).map(|&words| sum + words))
        {
            if tier1 <= lowered.iteration_words {
                self.demote_resident(eip);
                return true;
            }
            probation = Some(head::Probation {
                patches: Vec::new(),
                iteration_retired: u64::from(lowered.iteration_retired),
                saved_words: u64::from(tier1 - lowered.iteration_words),
                entry_exit_words: u64::from(lowered.entry_words + lowered.exit_words),
            });
        }
        let head = self.heads.get_mut(&eip).unwrap();
        let (trampoline, tier1) = if head.trampoline == 0 {
            let trampoline = self.code.cache.next();
            let tier1 = trampoline + 4;
            let mut words = vec![b(4)];
            if let Some(unit) = self.code.units.get(&eip) {
                words.push(head::branch(tier1, unit.unit));
            } else {
                words.extend(head::fallback(eip, tier1, self.code.stubs.exit));
            }
            if self.code.cache.append(&words).is_none() {
                return false;
            }
            (trampoline, tier1)
        } else {
            (head.trampoline, head.tier1)
        };
        let index = match task {
            compile::Task::Replace(i) => i,
            _ => head.versions.len(),
        };
        let slot = Box::<head::Slot>::default();
        let entry = head::emit(
            (eip, index, probation.is_some()),
            &head.sample,
            &slot,
            self.offload.gate(eip),
            &mut lowered,
            self.code.cache.next(),
            head::Targets {
                tier1,
                exit: self.code.stubs.exit,
                dispatch: self.code.stubs.dispatch,
                opt_exit: self.code.stubs.opt_exit,
            },
        );
        if self.code.cache.append(&entry.words).is_none() {
            return false;
        }
        for &(point, transition) in &lowered.transitions {
            self.code
                .direct_points
                .insert(entry.form + point, entry.form + transition);
        }
        let from = self.code.faults.sites.len().saturating_sub(1);
        self.code.faults.sites.extend(
            lowered
                .sites
                .iter()
                .map(|&(offset, site)| (entry.form + offset, site)),
        );
        if !self.code.faults.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.code.faults.sites.sort_unstable_by_key(|&(at, _)| at);
        }
        crate::jit_map::optimized(eip, format_args!("installed\t-\t-"));
        crate::jit_map::record(
            entry.form,
            &lowered.words,
            eip,
            head.trace.prepaid(0),
            &lowered.marks,
            "a64-resident",
        );
        if self.inline_saved.len() < lowered.spill_cells {
            self.inline_saved.resize(lowered.spill_cells, 0);
        }
        if let Some(probation) = &mut probation {
            probation.patches = lowered
                .probation
                .iter()
                .map(|word| entry.form + word * 4)
                .collect();
        }
        let version = head::Version {
            entry: entry.entry,
            probe: entry.probe,
            next: entry.next,
            at,
            assumed: built.assumed,
            sources: built.func.entry,
            plain_sites: built.plain_sites,
            records: lowered.records,
            flags: lowered.flags,
            slot,
            probation,
        };
        if index == head.versions.len() {
            head.versions.push(version);
        } else {
            head.versions[index] = version;
        }
        head.trampoline = trampoline;
        head.tier1 = tier1;
        head.probes = head::PROBES;
        head.sample.set(head::SAMPLE);
        head.versions
            .iter()
            .for_each(|version| version.slot.limit.set(0));
        if head.versions.len() < head::VERSIONS {
            let last = &head.versions.last().unwrap().slot;
            last.limit.set(last.misses.get().wrapping_add(head::MISSES));
        }
        if head.rebuilds == 0 {
            self.code
                .cache
                .patch(trampoline, head::branch(trampoline, head.versions[0].probe));
        }
        for pair in head.versions.windows(2) {
            self.code
                .cache
                .patch(pair[0].next, head::branch(pair[0].next, pair[1].entry));
        }
        self.code.uses_permissions = true;
        self.code.faults.end.set(self.code.cache.next());
        self.repoint_resident_edges(eip, Some(trampoline));
        true
    }

    fn repoint_resident_edges(&mut self, eip: u32, resident: Option<usize>) {
        self.repoint_internal_edges(eip, resident);
        let mut link = self.code.links.get(&eip).copied().unwrap_or(u32::MAX);
        while let Some(edge) = self.code.linked.get_mut(link as usize) {
            link = edge.next;
            if edge.back || edge.direct.is_some() {
                edge.sample = None;
                let unit = self
                    .code
                    .units
                    .get(&eip)
                    .map_or_else(|| self.heads[&eip].tier1, |entry| entry.unit);
                if resident.is_none() {
                    self.code.cache.patch(edge.point, edge.branch(unit));
                } else {
                    Self::route_link(&mut self.code.cache, edge, self.heads.get(&eip), unit);
                }
            }
        }
        self.code.cache.sync();
    }

    fn repoint_internal_edges(&mut self, eip: u32, resident: Option<usize>) {
        let head = self.heads.get(&eip);
        for edge in self.code.internal_edges.get_mut(&eip).into_iter().flatten() {
            edge.sample = None;
            let target = if let Some(resident) = resident.filter(|_| {
                edge.back
                    || !head
                        .unwrap()
                        .trace
                        .insts
                        .iter()
                        .any(|inst| inst.pc == edge.source_pc)
            }) {
                self.code
                    .cache
                    .patch(edge.link, head::branch(edge.link, resident));
                edge.transition
            } else {
                edge.target
            };
            self.code
                .cache
                .patch(edge.point, head::branch(edge.point, target));
        }
    }

    fn finish_probation(&mut self) {
        let event = self.runtime.resident_probation;
        if event == 0 {
            return;
        }
        let eip = (event >> 32) as u32;
        let index = event as u32 as usize - 1;
        let version = &mut self.heads.get_mut(&eip).unwrap().versions[index];
        let probation = version.probation.take().unwrap();
        if probation.profitable(&version.slot) {
            for point in probation.patches {
                self.code.cache.patch(point, crate::encode::NOP);
            }
            self.code.cache.sync();
        } else {
            crate::jit_map::demoted(
                eip,
                index,
                version.slot.entries.get(),
                version.slot.countdown[1].get() / probation.iteration_retired,
            );
            self.demote_resident(eip);
        }
    }

    fn demote_resident(&mut self, eip: u32) {
        let head = self.heads.get_mut(&eip).unwrap();
        head.unprofitable = true;
        head.versions.clear();
        self.repoint_resident_edges(eip, None);
    }

    /// Budget samples select Tier-1 loop traces.
    fn sample(&mut self, memory: &mut Memory, eip: u32) {
        if self.promotion == 0 || self.is_stop(eip) || self.promoted.contains(&eip) {
            return;
        }
        let count = self.samples.entry(eip).or_default();
        *count += 1;
        if *count >= self.promotion {
            self.samples.remove(&eip);
            self.promoted.insert(eip);
            self.diagnostics.promotions += 1;
            crate::jit_map::optimized(eip, format_args!("promoted\t-\t-"));
            self.replace_unit(memory, eip);
        }
    }

    #[cfg(test)]
    fn prepare_job(&mut self, memory: &mut Memory, stop: u32, at: &CpuState) {
        self.observe_stop(stop);
        self.control = at.x87.control;
        self.pe = at.x87.status & 0x20 != 0;
        if !self.code.units.contains_key(&at.eip) {
            self.translate(memory, at.eip).unwrap();
        }
        self.discover(memory, at.eip, at.eip, at.clone());
        self.install_optimized(true);
        self.demote_resident(at.eip);
    }

    /// The trace a hot entry at `eip` takes.
    fn loop_trace(&self, memory: &mut Memory, eip: u32, stops: &[u32]) -> translate::Trace {
        let mut trace = translate::Trace::default();
        let handler = if self.bridge.is_null() {
            None
        } else {
            // SAFETY: preparation stays within this owner's bridge call.
            unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                .handler
                .as_ref()
        };
        trace.build(
            memory,
            eip,
            stops,
            &|pc, word| handler.and_then(|handler| handler.host_thunk(pc, word)),
            &|pc| {
                let handler = handler?;
                let (target, original) = handler.redirect(pc)?;
                Some(translate::Redirect {
                    target,
                    original: original.to_vec(),
                    checks: handler.redirect_checks(pc).to_vec(),
                    body: handler.direct_body(pc),
                })
            },
            true,
        );
        trace
    }
}

/// Rebuilds the x86 flags at a fault site, where the exit stub assumed
/// subtraction sense and canonical PF/AF sources.
fn recover(cpu: &mut CpuState, site: Site, temps: [u64; 2]) {
    cpu.eip = site.eip;
    let regs = cpu.regs;
    let value = |operand| match operand {
        Operand::Guest(index) => regs[usize::from(index)],
        Operand::Temp(index) => temps[usize::from(index)] as u32,
        Operand::Imm(value) => value,
    };
    if let Some(result) = site.nz {
        let result = value(result);
        cpu.eflags = cpu.eflags & !0xc0 | u32::from(result == 0) << 6 | (result >> 24 & 0x80);
    }
    if let Some(overflow) = site.overflow.evaluate(value) {
        cpu.eflags = cpu.eflags & !0x800 | u32::from(overflow) << 11;
    }
    match site.carry.evaluate(value) {
        Some(cf) => cpu.eflags = cpu.eflags & !1 | u32::from(cf),
        None if site.sense == Sense::Addition => cpu.eflags ^= 1,
        None => {}
    }
    if let Some((parity, adjust)) = site.recipe.evaluate(value) {
        let pf = u32::from((parity as u8).count_ones().is_multiple_of(2));
        cpu.eflags = cpu.eflags & !0x04 | pf << 2;
        if let Some(adjust) = adjust {
            cpu.eflags = cpu.eflags & !0x10 | adjust & 0x10;
        }
    }
}

#[cfg(test)]
mod tests {
    use adv_cpu_core::memory::Memory;

    use super::*;
    use crate::encode::Size::{W, X};
    use crate::encode::{Reg, ldr, ldr_reg, ldur, movk, movz, sub_imm};
    use crate::translate::{Carry, Recipe};

    const X19: Reg = Reg::new(19);
    const X23: Reg = Reg::new(23);
    const X27: Reg = Reg::new(27);
    const STOP: u32 = 0xffff_0000;

    /// Guest memory over the whole window and its translator.
    fn native() -> (Memory, Native) {
        let memory = Memory::window(0, 1 << 32).unwrap();
        let mut native = Native::new(memory.window_base().unwrap()).unwrap();
        native.observe_stop(STOP);
        (memory, native)
    }

    /// A unit at `eip` that loads EAX with `load` (a site of `eip`) and then
    /// dispatches to `eip + 3`.
    fn install(native: &mut Native, eip: u32, load: u32) {
        let at = native.code.cache.next();
        let next = eip + 3;
        // Nothing is prepaid: the exit charges the countdown.
        let site = Site {
            eip,
            refund: 0,
            sense: Sense::Subtraction,
            carry: Carry::Host,
            nz: None,
            overflow: translate::Overflow::Host,
            recipe: Recipe::Canonical,
            checked: false,
            simd_dirty: 0,
            resume: 0,
            opt_record: None,
        };
        let words = [
            load,
            sub_imm(X, Reg::new(9), Reg::new(9), 1, false),
            movz(W, Reg::new(0), Exit::Dispatch as u16, 0),
            movz(W, Reg::new(1), next as u16, 0),
            movk(W, Reg::new(1), (next >> 16) as u16, 16),
            b((native.code.stubs.exit as i64 - (at + 20) as i64) as i32),
        ];
        native.place(eip, &words, &[(0, site)], 1, &[]).unwrap();
    }

    fn cpu(eip: u32, esp: u32, ecx: u32) -> CpuState {
        let mut cpu = CpuState {
            eip,
            eflags: 0x202,
            ..CpuState::default()
        };
        cpu.regs[0] = 0x5a5a_5a5a;
        cpu.regs[1] = ecx;
        cpu.regs[4] = esp;
        cpu
    }

    const OUTSIDE: u32 = 0x1_0000;
    const HEAD: u32 = 0x1_4000;
    const DATA: u32 = 0x2_0000;
    const OUTPUT: u32 = 0x3_0000;
    // mov [edi],eax; add edi,4; dec ecx; jne HEAD; int3
    const STORE_LOOP: &[u8] = &[0x89, 0x07, 0x83, 0xc7, 4, 0x49, 0x75, 0xf8, 0xcc];

    fn resident_cpu(eip: u32, count: u32) -> CpuState {
        let mut state = cpu(eip, 0, count);
        state.regs[7] = OUTPUT;
        state
    }

    fn resident_loop(body: &[u8]) -> (Memory, Native, u32) {
        use adv_cpu_core::memory::EXEC;

        let (mut memory, mut native) = native();
        memory.map(OUTSIDE, 0x8000, READ | WRITE).unwrap();
        memory.map(OUTPUT, 0x4000, READ | WRITE).unwrap();
        let mut jump = vec![0x31, 0xc0, 0x0f, 0x84]; // xor eax,eax; jz HEAD
        jump.extend_from_slice(&(HEAD - OUTSIDE - 8).to_le_bytes());
        memory.write(OUTSIDE, &jump).unwrap();
        let mut indirect = vec![0xb8];
        indirect.extend_from_slice(&HEAD.to_le_bytes());
        indirect.extend_from_slice(&[0xff, 0xe0]);
        memory.write(OUTSIDE + 16, &indirect).unwrap();
        memory.write(HEAD, body).unwrap();
        memory.protect(OUTSIDE, 0x8000, READ | EXEC).unwrap();
        let stop = HEAD + body.len() as u32 - 1;
        native.observe_stop(stop);
        native.first_visits = false;
        (memory, native, stop)
    }

    fn enter(native: &mut Native, memory: &mut Memory, stop: u32, cpu: &mut CpuState) -> Run {
        native
            .run(cpu, memory, stop, 4096, std::ptr::null_mut())
            .unwrap()
            .unwrap()
    }

    fn outside_link(native: &Native, head: u32) -> &Linked {
        let mut next = native.code.links[&head];
        loop {
            let edge = &native.code.linked[next as usize];
            if !edge.back {
                return edge;
            }
            next = edge.next;
        }
    }

    // spec: A64-OPT-8, A64-OPT-9
    #[test]
    fn resident_links_survive_tier1_promotion() {
        let (mut memory, mut native, stop) = resident_loop(STORE_LOOP);
        let invalidations = native.diagnostics.global_invalidations;
        let mut state = resident_cpu(OUTSIDE + 16, head::HOT);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 2 + 4 * head::HOT));
        assert_eq!(native.counters[&HEAD].get(), 1);
        assert!(!native.heads.contains_key(&HEAD));
        let unit = native.code.units[&HEAD].unit;
        let table = runtime::table_slot(HEAD);
        assert_eq!(native.code.table[table], [u64::from(HEAD), unit as u64]);

        let mut state = resident_cpu(OUTSIDE, 5);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 22));
        let edge = &native.code.internal_edges[&HEAD][0];
        let (back, transition, fallback, resident) =
            (edge.point, edge.transition, edge.target, edge.link);
        let link = outside_link(&native, HEAD);
        let (outside, unresolved) = (link.point, link.original);
        let arrival = link.direct.unwrap();
        assert_eq!(native.heads[&HEAD].versions.len(), 1);
        let trampoline = native.heads[&HEAD].trampoline;
        assert_eq!(native.code.cache.word(back), head::branch(back, transition));
        assert_eq!(
            native.code.cache.word(resident),
            head::branch(resident, trampoline)
        );
        assert_eq!(
            native.code.cache.word(outside),
            head::branch(outside, arrival)
        );
        assert_eq!(native.code.table[table][1], unit as u64);

        native.promotion = 1;
        native.sample(&mut memory, HEAD);
        let new = native.code.units[&HEAD].unit;
        assert_ne!(new, unit);
        assert_eq!(native.diagnostics.promotions, 1);
        assert_eq!(native.code.cache.word(back), head::branch(back, transition));
        assert_eq!(
            native.code.cache.word(resident),
            head::branch(resident, trampoline)
        );
        assert_eq!(
            native.code.cache.word(outside),
            head::branch(outside, arrival)
        );
        assert_eq!(native.code.table[table][1], new as u64);
        assert_eq!(
            native.code.cache.word(native.heads[&HEAD].tier1),
            head::branch(native.heads[&HEAD].tier1, new)
        );
        assert_eq!(native.diagnostics.global_invalidations, invalidations);

        memory.protect(HEAD, 0x4000, READ | WRITE).unwrap();
        memory.write(HEAD, &[0x90]).unwrap();
        native.invalidate(&mut memory);
        assert!(!native.heads.contains_key(&HEAD));
        assert_eq!(native.code.cache.word(back), head::branch(back, fallback));
        assert_eq!(native.code.cache.word(outside), unresolved);
        assert_eq!(native.code.table[table], EMPTY);
    }

    // spec: A64-OPT-9
    #[test]
    fn resident_discovery_builds_a_pointer_loop_without_a_proof() {
        let (mut memory, mut native, stop) = resident_loop(&[0x8b, 0x36, 0x49, 0x75, 0xfb, 0xcc]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        memory.write(DATA, &DATA.to_le_bytes()).unwrap();
        let mut state = resident_cpu(OUTSIDE, head::HOT + 1);
        state.regs[6] = DATA;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 2 + 3 * (head::HOT + 1)));
        assert!(!native.offload.contains(HEAD));
        let head = &native.heads[&HEAD];
        let edge = &native.code.internal_edges[&HEAD][0];
        assert_eq!(head.versions.len(), 1);
        assert_eq!(
            native.code.cache.word(edge.point),
            self::head::branch(edge.point, edge.transition)
        );
        assert_eq!(
            native.code.cache.word(edge.link),
            self::head::branch(edge.link, head.trampoline)
        );
        let outside = outside_link(&native, HEAD);
        let transition = outside.direct.unwrap();
        assert_eq!(
            native.code.cache.word(outside.point),
            self::head::branch(outside.point, transition)
        );
    }

    // spec: A64-OPT-8, A64-OPT-9
    #[test]
    fn internal_back_edges_discover_in_place_and_publish_only_on_transition() {
        let mut body = vec![0x90];
        for value in 0..8 {
            body.extend_from_slice(&[0xb8, value, 0, 0, 0]); // mov eax, imm32
        }
        body.extend_from_slice(STORE_LOOP);
        let latch = body.len() - 2;
        body[latch] = (-(latch as i32)) as u8; // jne HEAD + 1
        let (mut memory, mut native, stop) = resident_loop(&body);
        native.promotion = 0;
        let head = HEAD + 1;
        let mut state = resident_cpu(HEAD, head::HOT);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 1 + 12 * head::HOT));
        assert_eq!(native.counters[&head].get(), 1);
        assert!(!native.heads.contains_key(&head));

        let mut state = resident_cpu(HEAD, 2);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 25));
        assert!(!native.code.units.contains_key(&head));
        let edges = &native.code.internal_edges[&head];
        assert!(edges.iter().any(|edge| !edge.back));
        for edge in edges {
            assert_eq!(
                native.code.cache.word(edge.point),
                head::branch(edge.point, edge.transition)
            );
        }
        assert_eq!(native.heads[&head].versions[0].at.regs[1], 1);

        let mut state = resident_cpu(HEAD, 3);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 37));
        assert_eq!(native.heads[&head].versions[0].slot.entries.get(), 1);
        assert!(!native.code.units.contains_key(&head));

        let mut state = resident_cpu(HEAD, 2);
        state.mxcsr ^= 0x2000;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (head, 1));
        assert!(!native.code.units.contains_key(&head));
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 24));
        assert!(native.code.units.contains_key(&head));

        memory.protect(HEAD, 0x4000, READ | WRITE).unwrap();
        memory.write(head, &[0xcc]).unwrap();
        native.invalidate(&mut memory);
        assert!(!native.heads.contains_key(&head));
        for edge in &native.code.internal_edges[&head] {
            assert_eq!(
                native.code.cache.word(edge.point),
                head::branch(edge.point, edge.target)
            );
        }
    }

    // spec: A64-CTL-15, A64-OPT-8
    #[test]
    fn spin_back_edges_stay_in_tier1_and_yield() {
        // mov eax,[esi]; test eax,eax; je HEAD; int3
        let (mut memory, mut native, stop) =
            resident_loop(&[0x8b, 0x06, 0x85, 0xc0, 0x74, 0xfa, 0xcc]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        native.promotion = 0;
        let mut state = resident_cpu(HEAD, 0);
        state.regs[6] = DATA;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert!(run.retired > 3 * head::HOT);
        assert!(!native.counters.contains_key(&HEAD));
        assert!(!native.heads.contains_key(&HEAD));
        assert!(!native.force_resident(&mut memory, HEAD));

        state.eip = HEAD;
        native.set_spin_yield(true);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!(
            (run.exit, run.retired, state.eip),
            (Exit::Reschedule, 3, HEAD)
        );
    }

    // spec: A64-OPT-9, PROC-CLOCK-7
    #[test]
    fn resident_clock_polls_mark_observation_and_resample() {
        const PAGE: u32 = 0x7ffe_0000;
        let (mut memory, mut native, stop) = resident_loop(&[
            0xa1, 0x20, 3, 0xfe, 0x7f, // mov eax,[clock]
            0x8b, 0x15, 0x20, 3, 0xfe, 0x7f, // mov edx,[clock]
            0x85, 0xc0, // test eax,eax
            0x75, 9, // jne stop
            0x88, 0x0d, 0x20, 3, 2, 0, // mov [DATA + 0x320],cl
            0x49, 0x75, 0xe8, // dec ecx; jne HEAD
            0xcc,
        ]);
        // The second view updates the shared clock between polls.
        let storage = memory.map_shared_audio(PAGE, 0x4000).unwrap();
        memory
            .map_shared_file(DATA, 0x4000, storage, READ | WRITE, false)
            .unwrap();
        assert!(native.force_resident(&mut memory, HEAD));
        assert!(native.heads[&HEAD].versions[0].plain_sites.is_empty());
        for pc in [HEAD, HEAD + 5] {
            assert!(
                native
                    .code
                    .faults
                    .sites
                    .iter()
                    .any(|&(_, site)| { site.eip == pc && site.opt_record.is_some() })
            );
        }

        // Start after the loads, so only a resident arrival marks the clock.
        let mut state = resident_cpu(HEAD + 11, 0x100);
        state.regs[0] = 0;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((run.exit, run.retired), (Exit::Dispatch, 16));
        let head = &native.heads[&HEAD];
        assert_eq!(head.sample.get(), head::SAMPLE - 1);
        assert_eq!(head.versions[0].slot.misses.get(), 0);
        assert_eq!(
            (state.eip, state.regs[0], state.regs[2]),
            (stop, 0xff, 0xff)
        );
        assert_ne!(state.clock_read, 0);
    }

    // spec: A64-OPT-8
    #[test]
    fn resident_context_misses_chain_three_versions_and_stable_edges_link_directly() {
        let (mut memory, mut native, stop) = resident_loop(STORE_LOOP);
        enter(
            &mut native,
            &mut memory,
            stop,
            &mut resident_cpu(OUTSIDE, head::HOT + 1),
        );
        for mode in 1..=2 {
            let mut state = resident_cpu(OUTSIDE, 10);
            state.mxcsr = 0x1f80 | mode << 13;
            for _ in 0..4 {
                enter(&mut native, &mut memory, stop, &mut state);
                if native.heads[&HEAD].versions.len() == mode as usize + 1 {
                    break;
                }
                state.eip = OUTSIDE;
            }
            assert_eq!(native.heads[&HEAD].versions.len(), mode as usize + 1);
            let versions = &native.heads[&HEAD].versions;
            let previous = &versions[mode as usize - 1];
            assert_eq!(
                native.code.cache.word(previous.next),
                head::branch(previous.next, versions[mode as usize].entry)
            );
            assert!(previous.slot.misses.get() >= head::MISSES);
        }
        let resident = &native.heads[&HEAD];
        let before: Vec<_> = resident
            .versions
            .iter()
            .map(|v| v.slot.misses.get())
            .collect();
        resident.sample.set(8);
        let mut state = resident_cpu(OUTSIDE, 2);
        state.mxcsr = 0x7f80;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 10));
        let resident = &native.heads[&HEAD];
        assert_eq!(resident.versions.len(), head::VERSIONS);
        assert!(
            resident
                .versions
                .iter()
                .zip(before)
                .all(|(v, before)| v.slot.misses.get() == before + 2)
        );
        let last = resident.versions.last().unwrap();
        assert_eq!(
            native.code.cache.word(last.next),
            head::branch(last.next, resident.tier1)
        );

        for _ in 0..2 * head::SAMPLE {
            let mut state = resident_cpu(OUTSIDE, 10);
            state.mxcsr = 0x5f80;
            enter(&mut native, &mut memory, stop, &mut state);
        }
        let resident = &native.heads[&HEAD];
        let transition = outside_link(&native, HEAD).direct.unwrap();
        let patch = transition + 4;
        assert_eq!(
            native.code.cache.word(patch),
            head::branch(patch, resident.versions[2].entry)
        );
    }

    // spec: A64-MEM-7, A64-OPT-8
    #[test]
    fn replacing_a_checked_tier1_unit_keeps_dynamic_links() {
        let (mut memory, mut native, stop) = resident_loop(&[0x8b, 0x06, 0x49, 0x75, 0xfb, 0xcc]);
        memory.map(DATA, 0x4000, 0).unwrap();
        let invalidations = native.diagnostics.global_invalidations;
        let mut state = resident_cpu(OUTSIDE + 16, 2);
        state.regs[6] = DATA;
        assert_eq!(
            enter(&mut native, &mut memory, stop, &mut state).exit,
            Exit::Fault
        );
        let old = native.code.units[&HEAD].unit;
        for _ in 1..CHECK_AFTER {
            assert_eq!(
                enter(&mut native, &mut memory, stop, &mut state).exit,
                Exit::Fault
            );
        }
        let new = native.code.units[&HEAD].unit;
        assert_ne!(new, old);
        assert!(native.checked.contains(&HEAD));
        assert_eq!(
            native.code.table[runtime::table_slot(HEAD)],
            [u64::from(HEAD), new as u64]
        );
        assert_eq!(native.diagnostics.global_invalidations, invalidations);
    }

    // spec: A64-OPT-8
    #[test]
    fn the_compile_pool_installs_two_heads_and_ignores_a_stale_result() {
        use adv_cpu_core::memory::EXEC;
        let (mut memory, mut native, _) = resident_loop(STORE_LOOP);
        let other = HEAD + 0x4000;
        let stale = other + 0x4000;
        for entry in [other, stale] {
            memory.map(entry, 0x4000, READ | WRITE).unwrap();
            memory.write(entry, STORE_LOOP).unwrap();
            memory.protect(entry, 0x4000, READ | EXEC).unwrap();
            native.observe_stop(entry + STORE_LOOP.len() as u32 - 1);
        }
        native.compiler = compile::Compiler::background();
        for entry in [HEAD, other, stale] {
            native.translate(&mut memory, entry).unwrap();
            native.discover(&mut memory, entry, entry, resident_cpu(entry, 4));
        }
        memory.protect(stale, 0x4000, READ | WRITE).unwrap();
        memory.write(stale, &[0x90]).unwrap();
        native.invalidate(&mut memory);
        native.install_optimized(true);
        for eip in [HEAD, other] {
            assert_eq!(native.heads[&eip].versions.len(), 1);
        }
        assert!(!native.heads.contains_key(&stale));
        assert!(!native.offload.contains(stale));
    }

    // spec: A64-OPT-8, A64-OPT-9
    #[test]
    fn a_plain_page_miss_rebuilds_without_the_fact() {
        // mov eax,[esi]; add ebx,eax; dec ecx; jne HEAD; int3
        let (mut memory, mut native, stop) =
            resident_loop(&[0x8b, 0x06, 0x01, 0xc3, 0x49, 0x75, 0xf9, 0xcc]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        native.checked.insert(HEAD);
        assert!(native.force_resident(&mut memory, HEAD));
        let first = &native.heads[&HEAD].versions[0];
        assert!(first.plain_sites.contains(&HEAD));
        let old = first.entry;

        memory.unmap(DATA, 0x4000).unwrap();
        memory.map_shared_audio(DATA, 0x1000).unwrap();
        let mut state = resident_cpu(OUTSIDE, 2);
        state.regs[6] = DATA;
        enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!(native.heads[&HEAD].versions[0].slot.env[0].get(), 2);
        for _ in 1..head::MISSES.div_ceil(2) {
            let mut state = resident_cpu(OUTSIDE, 2);
            state.regs[6] = DATA;
            enter(&mut native, &mut memory, stop, &mut state);
        }
        let resident = &native.heads[&HEAD];
        assert!(resident.refused.contains(&HEAD));
        assert_ne!(resident.versions[0].entry, old);
        assert!(!resident.versions[0].plain_sites.contains(&HEAD));
    }

    // spec: A64-OPT-9
    #[test]
    fn repeated_frame_aliases_rebuild_without_cached_frame_cells() {
        // mov eax,[esp+4]; mov [edi],ecx; dec ecx; jne HEAD; int3
        let (mut memory, mut native, stop) =
            resident_loop(&[0x8b, 0x44, 0x24, 4, 0x89, 0x0f, 0x49, 0x75, 0xf7, 0xcc]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        native.count_resident_entries = true;
        let mut state = resident_cpu(HEAD, 4);
        state.regs[4] = DATA;
        state.regs[7] = DATA + 4;
        assert!(native.force_resident_at(&mut memory, state.clone()));
        let first = &native.heads[&HEAD].versions[0];
        assert!(first.plain_sites.contains(&u32::MAX));
        let old = first.entry;
        state.eip = OUTSIDE;
        native.compiler = compile::Compiler::background();

        for _ in 0..head::COLD_EXITS {
            enter(&mut native, &mut memory, stop, &mut state.clone());
        }
        native.install_optimized(true);
        let resident = &native.heads[&HEAD];
        let site = (HEAD + 4, ResidentCold::FrameDisjoint);
        assert_eq!(resident.cold_counts[&site], u64::from(head::COLD_EXITS));
        assert_ne!(resident.versions[0].entry, old);
        assert!(!resident.versions[0].plain_sites.contains(&u32::MAX));

        let before = native.diagnostics.resident_entries;
        let cold = native.diagnostics.resident_cold_exits;
        for _ in 0..100 {
            let mut state = state.clone();
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
            assert_eq!(state.regs[0], 2);
        }
        assert_eq!(native.diagnostics.resident_entries - before, 100);
        assert_eq!(native.diagnostics.resident_cold_exits, cold);
    }

    // spec: A64-MEM-7, A64-OPT-9
    #[test]
    fn two_resident_cold_exits_rebuild_the_site_as_checked() {
        let (mut memory, mut native, stop) = resident_loop(&[
            0x8b, 0x06, 0x83, 0xc6, 4, 0x89, 0x07, 0x83, 0xc7, 4, 0x49, 0x75, 0xf3, 0xcc,
        ]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        memory.map_shared_audio(DATA + 0x4000, 0x1000).unwrap();
        assert!(native.force_resident(&mut memory, HEAD));
        let old = native.heads[&HEAD].versions[0].entry;
        for expected in 1..=u64::from(head::COLD_EXITS) {
            let mut state = resident_cpu(OUTSIDE, 3);
            state.regs[6] = DATA + 0x4000 - 4;
            enter(&mut native, &mut memory, stop, &mut state);
            assert_eq!(native.diagnostics.resident_cold_exits, expected);
            assert_eq!(state.eip, HEAD);
        }
        let resident = &native.heads[&HEAD];
        assert!(resident.checked.contains(&HEAD));
        assert_ne!(resident.versions[0].entry, old);

        for _ in 0..100 {
            let mut state = resident_cpu(OUTSIDE, 3);
            state.regs[6] = DATA + 0x4000 - 4;
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
        }
        assert_eq!(
            native.diagnostics.resident_cold_exits,
            u64::from(head::COLD_EXITS)
        );
    }

    fn repeated_optional_cold_site(
        (instruction, site_offset): (&[u8], u32),
        reason: ResidentCold,
        checked: bool,
        setup: impl FnOnce(&mut Memory, &mut CpuState),
    ) {
        // test ebx,ebx; jz latch; <instruction>; dec ecx; jne HEAD; int3
        let mut body = vec![0x85, 0xdb, 0x74, instruction.len() as u8];
        body.extend_from_slice(instruction);
        body.extend_from_slice(&[0x49, 0x75, (-(body.len() as i8 + 3)) as u8, 0xcc]);
        let (mut memory, mut native, stop) = resident_loop(&body);
        let site = HEAD + 4 + site_offset;
        if checked {
            native.checks = Checks::All;
        }
        let mut state = resident_cpu(HEAD, 4);
        state.regs[3] = 1;
        setup(&mut memory, &mut state);
        native.count_resident_entries = true;
        assert!(native.force_resident_at(&mut memory, state.clone()));
        let old = native.heads[&HEAD].versions[0].entry;
        state.eip = OUTSIDE;
        for _ in 0..head::COLD_EXITS {
            enter(&mut native, &mut memory, stop, &mut state.clone());
        }
        let head = &native.heads[&HEAD];
        let cold = u64::from(head::COLD_EXITS);
        assert_eq!(head.cold_counts.get(&(site, reason)), Some(&cold));
        assert!(head.cuts.contains(&site));
        assert_ne!(head.versions[0].entry, old);

        let first = enter(&mut native, &mut memory, stop, &mut state.clone());
        assert!(first.retired > 0);
        assert_eq!(native.diagnostics.resident_cold_exits, cold);

        let entries = native.diagnostics.resident_entries;
        for _ in 0..100 {
            let mut state = state.clone();
            state.regs[3] = 0;
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
        }
        assert!(native.diagnostics.resident_entries >= entries + 100);
        assert_eq!(native.diagnostics.resident_cold_exits, cold);
    }

    // spec: A64-OPT-4, A64-OPT-9
    #[test]
    fn repeated_validation_cuts_before_the_instruction() {
        // mulss xmm0,xmm1 overflows and would set a clear MXCSR bit.
        repeated_optional_cold_site(
            (&[0xf3, 0x0f, 0x59, 0xc1], 0),
            ResidentCold::Validation,
            false,
            |_, state| {
                state.mxcsr = 0x1fa0;
                state.xmm[0][..4].copy_from_slice(&f32::MAX.to_le_bytes());
                state.xmm[1][..4].copy_from_slice(&2f32.to_le_bytes());
            },
        );
    }

    // spec: A64-MEM-7, A64-OPT-9
    #[test]
    fn repeated_blocked_access_cuts_before_the_instruction() {
        // mov eax,[esi]
        repeated_optional_cold_site(
            (&[0x8b, 0x06], 0),
            ResidentCold::BlockedAccess,
            true,
            |memory, state| {
                memory.map(DATA, 0x4000, 0).unwrap();
                state.regs[6] = DATA;
            },
        );
    }

    // spec: A64-MEM-7, A64-OPT-9
    #[test]
    fn repeated_checked_host_fault_cuts_before_the_instruction() {
        // mov eax,[esi]; lea esi,[esi+4] (the load must run in the loop).
        repeated_optional_cold_site(
            (&[0x8b, 0x06, 0x8d, 0x76, 4], 0),
            ResidentCold::HostFault,
            true,
            |memory, state| {
                memory.map(DATA, 0x4000, READ | WRITE).unwrap();
                state.regs[6] = DATA;
                // SAFETY: the test owns this mapped window page. Keep the
                // permission table readable to exercise a checked host fault.
                assert_eq!(
                    unsafe {
                        libc::mprotect(
                            (memory.window_base().unwrap() + DATA as usize) as *mut libc::c_void,
                            0x4000,
                            libc::PROT_NONE,
                        )
                    },
                    0
                );
            },
        );
    }

    // spec: A64-OPT-5, A64-OPT-9
    #[test]
    fn repeated_page_cross_cuts_before_the_instruction() {
        // mov eax,[esi]; mov [esi],eax
        repeated_optional_cold_site(
            (&[0x8b, 0x06, 0x89, 0x06], 2),
            ResidentCold::PageCross,
            false,
            |memory, state| {
                memory.map(DATA, 0x4000, READ | WRITE).unwrap();
                state.regs[6] = DATA + 0xfff;
            },
        );
    }

    // spec: A64-OPT-9
    #[test]
    fn repeated_division_cuts_before_the_instruction() {
        // div esi with a zero divisor.
        repeated_optional_cold_site((&[0xf7, 0xf6], 0), ResidentCold::Division, false, |_, _| {});
    }

    // spec: A64-MEM-7, A64-OPT-9
    #[test]
    fn repeated_blocked_access_on_every_iteration_declines_the_head() {
        // mov eax,[esi]; lea esi,[esi+4]; dec ecx; jne HEAD; int3
        let (mut memory, mut native, stop) =
            resident_loop(&[0x8b, 0x06, 0x8d, 0x76, 4, 0x49, 0x75, 0xf8, 0xcc]);
        memory.map(DATA, 0x4000, 0).unwrap();
        native.checks = Checks::All;
        assert!(native.force_resident(&mut memory, HEAD));
        let mut state = resident_cpu(OUTSIDE, 4);
        state.regs[6] = DATA;
        for _ in 0..head::COLD_EXITS {
            enter(&mut native, &mut memory, stop, &mut state.clone());
        }
        assert!(native.heads[&HEAD].unprofitable);
        for _ in 0..100 {
            let mut state = state.clone();
            assert_eq!(
                enter(&mut native, &mut memory, stop, &mut state).exit,
                Exit::Fault
            );
            assert_eq!(state.eip, HEAD);
        }
        assert_eq!(
            native.diagnostics.resident_cold_exits,
            u64::from(head::COLD_EXITS)
        );
    }

    fn probation_loop(trips: u32) -> (Memory, Native, u32) {
        let mut body = Vec::new();
        for value in 0..6 {
            body.extend_from_slice(&[0xb8, value, 0, 0, 0]);
        }
        body.extend_from_slice(STORE_LOOP);
        let latch = body.len() - 2;
        body[latch] = (-(latch as i32 + 1)) as u8;
        let (mut memory, mut native, stop) = resident_loop(&body);
        native.translate(&mut memory, HEAD).unwrap();
        native.discover(&mut memory, HEAD, HEAD, resident_cpu(HEAD, trips));
        native.install_optimized(true);
        (memory, native, stop)
    }

    // spec: A64-OPT-8
    #[test]
    fn outside_arrival_runs_the_first_iteration_in_resident_until_demotion() {
        let (mut memory, mut native, stop) = probation_loop(2);
        let invalidations = native.diagnostics.global_invalidations;
        let mut state = resident_cpu(OUTSIDE, 2);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!(state.eip, stop);
        let slot = &native.heads[&HEAD].versions[0].slot;
        assert_eq!(slot.entries.get(), 1);
        // Only the two instructions leading into the loop retire in Tier 1.
        assert_eq!(u64::from(run.retired) - slot.countdown[1].get(), 2);

        for _ in 1..300 {
            let mut state = resident_cpu(OUTSIDE, 2);
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
        }
        assert!(native.heads[&HEAD].unprofitable);
        assert_eq!(native.diagnostics.global_invalidations, invalidations);
        let outside = outside_link(&native, HEAD);
        assert_eq!(
            native.code.cache.word(outside.point),
            outside.branch(native.code.units[&HEAD].unit)
        );
        let edge = &native.code.internal_edges[&HEAD][0];
        assert_eq!(
            native.code.cache.word(edge.point),
            head::branch(edge.point, edge.target)
        );
    }

    // spec: A64-OPT-8
    #[test]
    fn resident_probation_keeps_long_invocations() {
        let (mut memory, mut native, stop) = probation_loop(100);
        let invalidations = native.diagnostics.global_invalidations;
        let patches = native.heads[&HEAD].versions[0]
            .probation
            .as_ref()
            .unwrap()
            .patches
            .clone();
        for _ in 0..300 {
            let mut state = resident_cpu(OUTSIDE, 100);
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
        }
        assert!(!native.heads[&HEAD].unprofitable);
        assert_eq!(native.diagnostics.global_invalidations, invalidations);
        assert!(
            patches
                .into_iter()
                .all(|word| native.code.cache.word(word) == crate::encode::NOP)
        );
    }

    // spec: A64-REG-6, A64-MEM-1
    #[test]
    fn the_window_is_aligned_and_biases_the_stack_registers() {
        let (memory, native) = native();
        let base = memory.window_base().unwrap();
        assert_eq!(base % (1 << 32), 0);
        assert!(base >= adv_cpu_core::virtual_memory::WINDOW_GUARD);
        assert_eq!(native.runtime.window, base as u64);
    }

    // spec: A64-MEM-8, A64-CTL-8
    #[test]
    fn raw_permissions_remain_owned_by_memory() {
        use adv_cpu_core::memory::{EXEC, READ, WRITE};
        use adv_cpu_core::trap::ExecutionTrap;

        struct Mapping(bool);
        impl ExecutionTrap for Mapping {
            fn call(
                &mut self,
                _: &mut CpuState,
                memory: &mut Memory,
                vector: u32,
            ) -> Result<bool, String> {
                assert_eq!(vector, 0x2e);
                assert_eq!(Arc::strong_count(&memory.perms), 1);
                if self.0 {
                    let before = Arc::as_ptr(&memory.perms);
                    memory.protect(0x8000, 0x1000, READ)?;
                    assert_eq!(Arc::as_ptr(&memory.perms), before);
                }
                Ok(false)
            }
        }

        for change_mapping in [false, true] {
            let (mut memory, mut native) = native();
            memory.map(0x1000, 0x1000, READ | WRITE | EXEC).unwrap();
            memory.map(0x8000, 0x1000, READ | WRITE).unwrap();
            memory.write(0x8000, &42u32.to_le_bytes()).unwrap();
            // mov eax, 7; int 0x2e; mov eax, [ecx]
            let code = [0xb8, 7, 0, 0, 0, 0xcd, 0x2e, 0x8b, 0x01];
            memory.write(0x1000, &code).unwrap();
            let stop = 0x1000 + code.len() as u32;
            native.first_visits = false;
            native.checks = Checks::All;
            let mut cpu = cpu(0x1000, 0, 0x8000);
            let mut mapping = Mapping(change_mapping);
            let mut bridge = Bridge {
                timestamp: Default::default(),
                memory: &mut memory,
                error: None,
                trap: None,
                calls: 0,
                handler: Some(&mut mapping),
                parked: false,
                deadline: None,
                clock_due: true,
                unclocked_bytes: 0,
                bulk_calls: 0,
                bulk_bytes: 0,
                cliffs: Default::default(),
                scratch: Default::default(),
            };
            let bridge_ptr = (&raw mut bridge).cast();
            let run = native
                .run(&mut cpu, &mut memory, stop, 100, bridge_ptr)
                .unwrap()
                .unwrap();
            assert!(bridge.error.is_none(), "{:?}", bridge.error);
            assert_eq!(bridge.calls, 1);
            assert_eq!(Arc::strong_count(&memory.perms), 1);
            if change_mapping {
                assert_eq!((cpu.eip, cpu.regs[0]), (0x1007, 7));
                assert_eq!(run.exit, Exit::Dispatch);
                native
                    .run(&mut cpu, &mut memory, stop, 100, bridge_ptr)
                    .unwrap();
            }
            assert_eq!((cpu.eip, cpu.regs[0]), (stop, 42));
            let before = Arc::as_ptr(&memory.perms);
            memory.unmap(0x8000, 0x1000).unwrap();
            assert_eq!(Arc::as_ptr(&memory.perms), before);
        }
    }

    #[test]
    fn a_readable_guest_page_loads_directly() {
        let (mut memory, mut native) = native();
        memory.map(0x2_0000, 0x4000, 1).unwrap();
        memory
            .write(0x2_0004, &0x1234_5678u32.to_le_bytes())
            .unwrap();
        install(
            &mut native,
            0x1000,
            ldr_reg(W, X19, X27, Reg::new(20), false), // mov eax, [ecx]
        );
        let mut cpu = cpu(0x1000, 0x8000, 0x2_0004);
        let run = native
            .run(&mut cpu, &mut memory, STOP, 1, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!((run.exit, run.retired), (Exit::Dispatch, 1));
        assert_eq!(cpu.regs[0], 0x1234_5678);
        assert_eq!(cpu.eip, 0x1003);
    }

    // spec: A64-CTL-9
    #[test]
    fn a_new_stop_cuts_only_the_units_from_its_page() {
        use adv_cpu_core::memory::{EXEC, READ};

        let (mut memory, mut native) = native();
        memory.map(0x1000, 0x2000, READ | WRITE).unwrap();
        // 0x1000: mov eax, 1; jmp 0x2000
        memory
            .write(0x1000, &[0xb8, 1, 0, 0, 0, 0xe9, 0xf6, 0x0f, 0, 0])
            .unwrap();
        // 0x1800: mov ebx, 5; int3
        memory.write(0x1800, &[0xbb, 5, 0, 0, 0, 0xcc]).unwrap();
        // 0x2000: add eax, 2; inc eax; int3
        memory
            .write(0x2000, &[0x83, 0xc0, 0x02, 0x40, 0xcc])
            .unwrap();
        memory.protect(0x1000, 0x2000, READ | EXEC).unwrap();
        native.first_visits = false;
        let to = |native: &mut Native, memory: &mut Memory, eip, stop| {
            let mut cpu = cpu(eip, 0x8000, 0);
            while cpu.eip != stop {
                native
                    .run(&mut cpu, memory, stop, 100, std::ptr::null_mut())
                    .unwrap();
            }
            cpu
        };
        to(&mut native, &mut memory, 0x1800, 0x1805);
        assert_eq!(to(&mut native, &mut memory, 0x1000, 0x2004).regs[0], 4);
        let (clears, kept) = (native.diagnostics.clears, native.unit_address(0x1800));
        assert!(kept.is_some());

        // The unit through 0x2003 must end there now; the other page keeps its unit.
        let cpu = to(&mut native, &mut memory, 0x1000, 0x2003);
        assert_eq!(cpu.regs[0], 3);
        assert_eq!(native.unit_address(0x1800), kept);
        assert_eq!(native.diagnostics.clears, clears);
    }

    // spec: A64-MEM-1, A64-MEM-5, A64-MEM-9
    #[test]
    fn faults_in_guards_and_inaccessible_pages_resume_at_the_site() {
        let (mut memory, mut native) = native();
        // A page the guest cannot read stays inaccessible in the window.
        memory.map(0xa_0000, 0x1000, 0).unwrap();
        // push/pop past 0 and past 4 GiB, an unmapped and an unreadable page.
        install(&mut native, 0x1000, ldur(W, X19, X23, -4));
        install(&mut native, 0x2000, ldr(W, X19, X23, 4));
        install(
            &mut native,
            0x3000,
            ldr_reg(W, X19, X27, Reg::new(20), false),
        );
        for (eip, esp, ecx) in [
            (0x1000, 0, 0),
            (0x2000, 0xffff_fffc, 0),
            (0x3000, 0x8000, 0x9_0000),
            (0x3000, 0x8000, 0xa_0000),
        ] {
            let mut cpu = cpu(eip, esp, ecx);
            let before = cpu.clone();
            let run = native
                .run(&mut cpu, &mut memory, STOP, 1, std::ptr::null_mut())
                .unwrap()
                .unwrap();
            assert_eq!((run.exit, run.retired), (Exit::Fault, 0), "{eip:#x}");
            assert_eq!(
                cpu, before,
                "{eip:#x}: state is exactly that before the instruction"
            );
        }
    }

    // spec: A64-FLAG-3, A64-FLAG-5, A64-MEM-10
    #[test]
    fn a_fault_rebuilds_flags_from_the_sites_sense_and_recipe() {
        let mut cpu = cpu(0, 0, 0);
        cpu.regs[0] = 0x10;
        // After `add eax, 1` gave 0x10: C set means CF clear in addition
        // sense, which the stub read as CF set; 0x10 has odd parity, and
        // 0x0f ^ 1 ^ 0x10 has bit 4 set.
        cpu.eflags = 0x202 | 1 | 0x04;
        recover(
            &mut cpu,
            Site {
                eip: 0x1234,
                refund: 3,
                sense: Sense::Addition,
                carry: Carry::Host,
                nz: None,
                overflow: translate::Overflow::Host,
                recipe: Recipe::AddResult(Operand::Guest(0), Operand::Imm(1)),
                checked: false,
                simd_dirty: 0,
                resume: 0,
                opt_record: None,
            },
            [0; 2],
        );
        assert_eq!(cpu.eip, 0x1234);
        assert_eq!(cpu.eflags, 0x202 | 0x10);
    }
    // spec: A64-CTL-6
    #[test]
    fn j1_indirect_slots_fold_high_bits_keep_low_bits_and_clear_on_discard() {
        let (_, mut native) = native();
        let targets = [0x1000, 0x1001, 0x41000];
        install(&mut native, targets[0], ldr_reg(W, X19, X27, X19, false));
        for (index, target) in targets.into_iter().enumerate() {
            for other in &targets[..index] {
                assert_ne!(runtime::table_slot(target), runtime::table_slot(*other));
            }
            native.code.table[runtime::table_slot(target)] = [u64::from(target), 42];
        }
        native.discard_switch_target(targets[0]);
        assert!(native.code.table.iter().all(|entry| *entry == EMPTY));
    }
    // spec: A64-MEM-8, EXEC-CALL-3
    #[test]
    fn one_page_changes_keep_both_native_tables_in_place() {
        let (mut memory, mut native) = native();
        memory.map(0x8000, 0x2000, READ | WRITE).unwrap();
        native.classify(&mut memory);
        let raw = memory.perms.as_ptr();
        let classes = native.runtime.classes;
        let body_classes = native.runtime.body.classes;
        let owner = memory.native_permissions();
        let derived = owner.as_ptr();
        drop(owner);
        memory.protect(0x8000, 0x1000, READ).unwrap();
        native.classify(&mut memory);
        assert_eq!(native.runtime.classes, classes);
        assert_eq!(memory.perms.as_ptr(), raw);
        assert_eq!(memory.native_permissions().as_ptr(), derived);
        assert_eq!(native.runtime.body.classes, body_classes);
        assert_eq!(memory.perms[8], READ);
        assert_eq!(unsafe { *(classes as *const u8).add(8) } & 7, READ);
        assert_eq!(unsafe { *body_classes.add(9) } & 7, READ | WRITE);
        memory.map(0xa000, 0x1000, READ | WRITE).unwrap();
        native.classify(&mut memory);
        assert_eq!(memory.perms.as_ptr(), raw);
        assert_eq!(native.runtime.classes, classes);
        memory.unmap(0xa000, 0x1000).unwrap();
        native.classify(&mut memory);
        assert_eq!(memory.perms.as_ptr(), raw);
        assert_eq!(native.runtime.classes, classes);
        memory.watch_code(0x9000, 1);
        native.classify(&mut memory);
        assert_eq!(native.runtime.classes, classes);
        assert_eq!(
            unsafe { *(classes as *const u8).add(9) } & 7,
            READ | WRITE | adv_cpu_core::memory::EXEC
        );
    }

    // spec: EXEC-CALL-2
    #[test]
    fn unchanged_epochs_do_not_classify_aliases() {
        let (mut memory, mut native) = native();
        memory.map(0x8000, 0x1000, READ | WRITE).unwrap();
        native.classify(&mut memory);
        let classifications = native.classifications;
        for _ in 0..20 {
            native.classify(&mut memory);
        }
        assert_eq!(native.classifications, classifications);
        memory.protect(0x8000, 0x1000, READ).unwrap();
        native.classify(&mut memory);
        assert_eq!(native.classifications, classifications + 1);
    }
}
