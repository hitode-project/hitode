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

use iced_x86::Code;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;
use std::sync::Arc;

use adv_cpu_core::interpreter::{Execution, Step, accessible};
use adv_cpu_core::memory::{Memory, READ, WRITE};
use adv_cpu_core::trap::{Bridge, call_host, call_trap};
use adv_cpu_core::{CpuState, IndirectBranch, IndirectBranchKind};

use crate::cache::CodeCache;
use crate::encode::{b, bl};
use crate::runtime::{self, Exit, Runtime};
use crate::translate::{self, Features, Operand, Sense, Site, Stubs};

const CACHE_LEN: usize = 64 << 20;

/// Most guest instructions one `run` completes, so the owner still sees
/// deadlines and interrupts while linked units loop.
const SLICE: u64 = 1 << 16;

type Enter = unsafe extern "C" fn(*mut Runtime, usize);

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
    /// The site that faulted, written in signal context.
    pc: Cell<usize>,
    /// v8-v23 captured before the fault stub restores host SIMD.
    simd: Cell<[[u8; 16]; 16]>,
    /// The shadow stack's guard pages.
    guard: (usize, usize),
    /// Where an overflow resumes, and the runtime field it reads the
    /// faulting PC from.
    reset: usize,
    resume: *mut u64,
}

impl Faults {
    fn site(&self, pc: usize) -> Option<Site> {
        let index = self.sites.binary_search_by_key(&pc, |&(at, _)| at).ok()?;
        Some(self.sites[index].1)
    }
}

thread_local! {
    /// The `Faults` of the code this thread is running, or null.
    static ACTIVE: Cell<*const Faults> = const { Cell::new(std::ptr::null()) };
}

/// Runs in signal context: no allocation, no locks.
unsafe fn on_fault(
    pc: usize,
    address: usize,
    snapshot: &adv_cpu_core::execution_abi::HostContextSnapshot,
) -> usize {
    ACTIVE
        .try_with(|slot| {
            let faults = slot.get();
            if faults.is_null() {
                return 0;
            }
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
            faults
                .simd
                .set(std::array::from_fn(|n| snapshot.simd[8 + n]));
            faults.pc.set(pc);
            if site.resume != 0 {
                // The unit publishes its cached x87 state, then faults.
                return pc.wrapping_add_signed(site.resume as isize);
            }
            faults.stub
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
        cpu.last_indirect_branch = IndirectBranch {
            source: branch as u32,
            target: eip,
            kind: match branch >> 32 {
                1 => IndirectBranchKind::Call,
                2 => IndirectBranchKind::Jump,
                _ => IndirectBranchKind::Return,
            },
        };
    }
    if native.stops.contains(&eip) || owner.error.is_some() {
        return 0;
    }
    // A first visit runs in the owner; the point links on the next one.
    if native.first_visit(eip) {
        return 0;
    }
    let before = native.cache.used();
    let unit = match native.link(memory, point, eip) {
        Ok(unit) => unit,
        Err(error) => {
            owner.error = Some(error);
            return 0;
        }
    };
    native.resolve_tables(memory, eip, unit);
    if point == 0 {
        native.table[runtime::table_slot(eip)] = [u64::from(eip), unit as u64];
    }
    owner.translated += native.cache.used().saturating_sub(before);
    native.cache.sync();
    native.faults.end.set(native.cache.next());
    unit
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
            native.faults.pc.set(site);
            let resume = native.faults.site(site).map_or(0, |site| site.resume);
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
    let helper = &native.helpers[index as usize];
    cpu.eip = helper.pc;
    native.diagnostics.helpers += 1;
    native.helper_counts[index as usize] += 1;
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
    pub(crate) images: std::rc::Rc<std::cell::RefCell<Vec<(u32, u32)>>>,
    /// Switch-table slots by guest target: source entry, host slot and
    /// original shared stub. Dispatch looks targets up on every miss.
    rows: FxHashMap<u32, Vec<(u32, usize, usize)>>,
    /// Entries of units that own switch tables.
    table_sources: FxHashSet<u32>,
    bridge: *mut std::ffi::c_void,
    pub(crate) interrupt: Option<Arc<std::sync::atomic::AtomicU32>>,
    pub(crate) pending_mask: u32,
    cache: CodeCache,
    enter: Enter,
    stubs: Stubs,
    /// Cache bytes the stubs occupy; clearing keeps them.
    stub_len: usize,
    units: FxHashMap<u32, Entry>,
    /// Unit entries by the guest pages their code came from.
    pages: FxHashMap<u32, Vec<u32>>,
    /// Patched link points, with the words they held, by the unit they
    /// branch to: the head of a list in `linked`.
    links: FxHashMap<u32, u32>,
    /// Link point, the word it held, and the next point to the same unit.
    linked: Vec<(usize, u32, u32)>,
    /// Link points of the units the current translation placed.
    points: Vec<(usize, u32, bool)>,
    /// Entries that have run, so a return to one translates it.
    seen: FxHashSet<u32>,
    /// Whether entries run once before they are translated.
    pub(crate) first_visits: bool,
    pub(crate) checks: Checks,
    /// Faults at unchecked sites by guest instruction.
    strikes: FxHashMap<u32, u32>,
    /// Guest instructions whose accesses check their page first.
    checked: FxHashSet<u32>,
    uses_permissions: bool,
    /// Faults at unchecked sites so far.
    pub(crate) unchecked_faults: u64,
    /// The run's wall deadline as a virtual counter value.
    pub(crate) deadline: Option<u64>,
    /// Diagnostic counts of how runs end; see `Diagnostics`.
    pub(crate) diagnostics: Diagnostics,
    pub(crate) retired_forms: Option<[u64; 5]>,
    helper_units: u64,
    /// Each placed unit's host address and entry, ascending.
    placed: Vec<(usize, u32)>,
    /// The memory epoch last classified, and the all-slow test table.
    classified_epoch: Option<u64>,
    classes: Option<Arc<Vec<u8>>>,
    #[cfg(test)]
    classifications: u64,
    /// Buffers of the last translation.
    work: translate::Workspace,
    /// What the units' `interpret` calls run, until the cache clears,
    /// and how often each ran.
    helpers: Vec<translate::Helper>,
    helper_counts: Vec<u64>,
    /// Helper runs by EIP from before the last clear.
    helper_eips: FxHashMap<u32, u64>,
    /// The step an `Exit::Stepped` leaves the owner.
    pub(crate) pending: Option<Execution>,
    /// Counts cache clears, which invalidate link points.
    generation: u64,
    /// The indirect table the runtime points at.
    table: Box<[[u64; 2]]>,
    shadow: ShadowStack,
    /// Stop addresses seen so far; units end before all of them.
    stops: Vec<u32>,
    pub(crate) features: Features,
    faults: Faults,
    runtime: Box<Runtime>,
    /// Live timestamp bridge, including runs without a trap handler.
    pub(crate) clock: *mut std::ffi::c_void,
    /// The x87 control word of the CPU a run starts on.
    control: u16,
}

impl Native {
    /// `window` is the host address of guest address 0 in the guest's
    /// `GuestWindow`, which must outlive this.
    pub fn new(window: usize) -> Result<Self, String> {
        let mut cache = CodeCache::new(CACHE_LEN)?;
        adv_cpu_core::fault_recovery::install_translated(on_fault)?;
        let stubs = runtime::stubs();
        let base = cache.append(&stubs.code).ok_or("stubs do not fit")?;
        crate::jit_map::record_stubs(
            base,
            base + 4 * stubs.code.len(),
            &[
                (stubs.enter, "enter"),
                (stubs.exit, "exit"),
                (stubs.fault, "fault"),
                (stubs.dispatch, "dispatch"),
                (stubs.lookup, "lookup"),
                (stubs.lookup_slow, "lookup_slow"),
                (stubs.ret_miss, "ret_miss"),
                (stubs.shadow_reset, "shadow_reset"),
                (stubs.access, "access"),
                (stubs.interpret, "interpret"),
                (stubs.call, "call"),
            ],
        );
        // SAFETY: `enter` follows the C calling convention with the
        // signature of `Enter`; see `runtime::emit_enter`.
        let enter = unsafe { std::mem::transmute::<usize, Enter>(base + stubs.enter) };
        let mut table = empty_table();
        let shadow = ShadowStack::new()?;
        let mut runtime = Box::new(Runtime::new(std::ptr::null_mut(), window as u64));
        runtime.table = table.as_mut_ptr() as u64;
        runtime.shadow_top = shadow.top();
        runtime.shadow = shadow.top();
        Ok(Self {
            images: Default::default(),
            rows: FxHashMap::default(),
            table_sources: FxHashSet::default(),
            bridge: std::ptr::null_mut(),
            interrupt: None,
            pending_mask: u32::MAX,
            stub_len: cache.used(),
            faults: Faults {
                start: base,
                end: Cell::new(cache.next()),
                sites: Vec::new(),
                stub: base + stubs.fault,
                pc: Cell::new(0),
                simd: Cell::new([[0; 16]; 16]),
                guard: shadow.guard(),
                reset: base + stubs.shadow_reset,
                resume: &raw mut runtime.resume,
            },
            cache,
            enter,
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
            },
            units: FxHashMap::default(),
            pages: FxHashMap::default(),
            links: FxHashMap::default(),
            linked: Vec::new(),
            points: Vec::new(),
            seen: FxHashSet::default(),
            first_visits: true,
            checks: Checks::Adaptive,
            strikes: FxHashMap::default(),
            checked: FxHashSet::default(),
            uses_permissions: false,
            unchecked_faults: 0,
            deadline: None,
            diagnostics: Diagnostics::default(),
            placed: Vec::new(),
            classified_epoch: None,
            #[cfg(test)]
            classifications: 0,
            classes: None,
            work: translate::Workspace::default(),
            helpers: Vec::new(),
            helper_counts: Vec::new(),
            retired_forms: None,
            helper_units: 0,
            helper_eips: FxHashMap::default(),
            pending: None,
            generation: 0,
            table,
            shadow,
            stops: Vec::new(),
            features: features(),
            runtime,
            clock: std::ptr::null_mut(),
            control: 0,
        })
    }

    pub(crate) fn count_forms(&mut self, enabled: bool) {
        self.retired_forms = enabled.then_some([0; 5]);
        if enabled {
            crate::jit_map::enable_memory();
        }
    }

    /// Runs the unit at `cpu.eip` if it completes at most `fuel` guest
    /// instructions; `None` leaves them to the interpreter. A fault returns
    /// `Exit::Fault` with EIP at the faulting instruction and the guest state
    /// exactly that before it.
    pub fn run(
        &mut self,
        cpu: &mut CpuState,
        memory: &mut Memory,
        stop: u32,
        fuel: u64,
        bridge: *mut std::ffi::c_void,
    ) -> Result<Option<Run>, String> {
        self.bridge = bridge;
        self.control = cpu.x87.control;
        self.invalidate(memory);
        self.observe_stop(stop);
        let (entry, translated) = match self.units.get(&cpu.eip).copied() {
            Some(entry) => (entry, 0),
            None if self.first_visit(cpu.eip) => {
                let visit = translate::first::run(cpu, memory, stop, fuel.min(SLICE));
                if let Some(forms) = &mut self.retired_forms {
                    forms[3] += u64::from(visit.retired);
                }
                self.diagnostics.first_visits += 1;
                self.diagnostics.first_declined += u64::from(visit.declined);
                let exit = if visit.declined {
                    Exit::Interpret
                } else {
                    Exit::Dispatch
                };
                return Ok(Some(Run {
                    exit,
                    retired: visit.retired,
                    translated: 0,
                }));
            }
            None => {
                let before = self.cache.used();
                let entry = self.translate(memory, cpu.eip)?;
                (entry, self.cache.used().saturating_sub(before))
            }
        };
        if u64::from(entry.retired) > fuel {
            self.diagnostics.over_fuel += 1;
            return Ok(None);
        }
        if entry.retired == 0 {
            self.diagnostics.empty += 1;
            // The first instruction is not translated: skip the round trip.
            return Ok(Some(Run {
                exit: Exit::Interpret,
                retired: 0,
                translated,
            }));
        }
        debug_assert_eq!(cpu.lazy_flags[3], 0, "lazy flags settle before entry");
        self.helper_units = 0;
        let budget = fuel.min(SLICE);
        if self.uses_permissions || self.checks != Checks::Adaptive || !self.checked.is_empty() {
            self.classify(memory);
        }
        if let Some(word) = &self.interrupt {
            word.fetch_and(
                !(adv_sync::pending::STOP | adv_sync::pending::WORK),
                std::sync::atomic::Ordering::AcqRel,
            );
        }
        self.cache.sync();
        self.faults.end.set(self.cache.next());
        let runtime: *mut Runtime = &mut *self.runtime;
        let faults: *const Faults = &self.faults;
        let enter = self.enter;
        let mut owner = Owner {
            native: self,
            memory,
            bridge,
            translated,
            error: None,
        };
        // SAFETY: until `enter` returns, `self` is reached only through
        // `owner`, by `dispatch`. `unit` is translated code ending at the
        // stubs, and the runtime points at a live CPU for the call.
        unsafe {
            (*runtime).countdown = budget;
            (*runtime).cpu = cpu;
            (*runtime).helper = dispatch as *const () as u64;
            (*runtime).access = access as *const () as u64;
            (*runtime).fault_simd = (*faults).simd.as_ptr() as u64;
            (*runtime).interpret = interpret as *const () as u64;
            (*runtime).call = call as *const () as u64;
            (*runtime).timestamp = timestamp as *const () as u64;
            (*runtime).owner = &raw mut owner as u64;
            let previous = ACTIVE.replace(faults);
            enter(runtime, entry.unit);
            ACTIVE.set(previous);
        }
        self.runtime.cpu = std::ptr::null_mut();
        self.runtime.owner = 0;
        if let Some(error) = owner.error {
            return Err(error);
        }
        let translated = owner.translated;
        let exit = Exit::from_raw(self.runtime.exit)
            .ok_or_else(|| format!("unknown a64 exit {}", self.runtime.exit))?;
        match exit {
            Exit::Interpret => self.diagnostics.interprets += 1,
            Exit::Fault => self.diagnostics.faults += 1,
            Exit::Dispatch => self.diagnostics.dispatches += 1,
            Exit::Stepped => {}
            Exit::Yield => self.runtime.shadow = self.shadow.top(),
        }
        let mut retired = budget - self.runtime.countdown;
        if exit == Exit::Fault {
            let pc = self.faults.pc.get();
            let site = self.faults.site(pc).expect("faulting site");
            recover(cpu, site, self.runtime.temps);
            for (n, bytes) in self.faults.simd.get().into_iter().enumerate() {
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
            retired -= u64::from(site.refund);
            if !site.checked {
                self.unchecked_faults += 1;
                self.strike(site.eip, pc);
            } else {
                self.diagnostics.checked_faults += 1;
            }
        }
        if let Some(forms) = &mut self.retired_forms {
            // Native REP elements stay in translated fuel: telling them
            // apart would cost emitted code in every REP loop.
            forms[0] += retired - self.helper_units;
        }
        let retired = retired as u32;
        Ok(Some(Run {
            exit,
            retired,
            translated,
        }))
    }

    pub(crate) fn set_body_services(&mut self, services: adv_cpu_core::body::Services) {
        self.runtime.body.services = services;
    }

    /// The unit at `eip`, translated if needed; a nonzero `point` is
    /// patched into a branch to it. The caller keeps stops unlinked: the
    /// owner must see them.
    fn link(&mut self, memory: &mut Memory, point: usize, eip: u32) -> Result<usize, String> {
        let generation = self.generation;
        let entry = match self.units.get(&eip) {
            Some(&entry) => entry,
            None => self.translate(memory, eip)?,
        };
        // Translating may have cleared the cache and the link point with it.
        if point != 0 && self.generation == generation {
            self.patch(point, eip, entry.unit);
        }
        Ok(entry.unit)
    }

    /// Makes the link point at `point` branch to `unit`, the unit at `eip`.
    fn patch(&mut self, point: usize, eip: u32, unit: usize) {
        let word = self.cache.word(point);
        let offset = (unit as i64 - point as i64) as i32;
        // A real call's `bl` stays a call.
        let link = if word & 0xFC00_0000 == 0x9400_0000 {
            bl(offset)
        } else {
            b(offset)
        };
        self.cache.patch(point, link);
        let next = self.links.insert(eip, self.linked.len() as u32);
        self.linked.push((point, word, next.unwrap_or(u32::MAX)));
    }

    fn resolve_tables(&mut self, memory: &Memory, eip: u32, unit: usize) {
        if !self.rows.contains_key(&eip)
            || self.stops.contains(&eip)
            || !translate::tables::image(&self.images.borrow(), eip, 1)
        {
            return;
        }
        // Import trampolines and transport thunks remain patch-sensitive.
        let Ok(bytes) = memory.read(eip, 15) else {
            return;
        };
        let mut decoder =
            iced_x86::Decoder::with_ip(32, bytes, u64::from(eip), iced_x86::DecoderOptions::NONE);
        let instruction = decoder.decode();
        if matches!(
            instruction.flow_control(),
            iced_x86::FlowControl::UnconditionalBranch
                | iced_x86::FlowControl::IndirectBranch
                | iced_x86::FlowControl::Interrupt
        ) || matches!(instruction.code(), Code::Pushd_imm32 | Code::Pushd_imm8)
        {
            return;
        }
        for &(_, point, _) in &self.rows[&eip] {
            self.cache.patch(point, unit as u32);
            self.cache.patch(point + 4, ((unit as u64) >> 32) as u32);
        }
    }

    /// Drops units whose guest code changed, and restores the link points
    /// that branch to them.
    fn invalidate(&mut self, memory: &mut Memory) {
        if memory.dirty_pages.is_empty() {
            return;
        }
        let mut dropped = false;
        for page in std::mem::take(&mut memory.dirty_pages).into_keys() {
            for eip in self.pages.remove(&page).unwrap_or_default() {
                let unit = self.drop_unit(eip);
                self.diagnostics.dropped_units += u64::from(unit);
                dropped |= unit;
            }
        }
        if dropped {
            self.forget_dynamic();
        }
    }

    /// Drops the unit at `eip`, if any, and restores the link points that
    /// branch to it; the caller forgets dynamic targets.
    fn drop_unit(&mut self, eip: u32) -> bool {
        if self.table_sources.remove(&eip) {
            self.rows.retain(|_, slots| {
                slots.retain(|&(source, _, _)| source != eip);
                !slots.is_empty()
            });
        }
        for &(_, point, stub) in self.rows.get(&eip).into_iter().flatten() {
            self.cache.patch(point, stub as u32);
            self.cache.patch(point + 4, ((stub as u64) >> 32) as u32);
        }
        let mut next = self.links.remove(&eip).unwrap_or(u32::MAX);
        while let Some(&(point, word, after)) = self.linked.get(next as usize) {
            self.cache.patch(point, word);
            next = after;
        }
        self.units.remove(&eip).is_some()
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

    #[cfg(test)]
    pub(crate) fn switch_counts(&self) -> (usize, usize) {
        (
            self.rows.values().map(Vec::len).sum(),
            self.rows
                .values()
                .flatten()
                .filter(|&&(_, point, stub)| {
                    let value = u64::from(self.cache.word(point))
                        | (u64::from(self.cache.word(point + 4)) << 32);
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
    pub(crate) fn unit_address(&self, eip: u32) -> Option<usize> {
        self.units.get(&eip).map(|entry| entry.unit)
    }

    /// Guest instructions whose accesses check their page.
    pub(crate) fn checked_count(&self) -> usize {
        self.checked.len()
    }

    /// Counts a fault at an unchecked site of `eip`, at host address `pc`.
    /// From `CHECK_AFTER` on, `eip` is checked and the unit holding `pc`
    /// goes, so the next dispatch translates it that way.
    fn strike(&mut self, eip: u32, pc: usize) {
        let strikes = self.strikes.entry(eip).or_default();
        *strikes += 1;
        if *strikes < CHECK_AFTER {
            return;
        }
        self.checked.insert(eip);
        let index = self.placed.partition_point(|&(start, _)| start <= pc);
        let Some(&(start, entry)) = index.checked_sub(1).map(|index| &self.placed[index]) else {
            return;
        };
        // A unit dropped before stays in `placed`.
        if self
            .units
            .get(&entry)
            .is_some_and(|unit| unit.unit == start)
        {
            self.drop_unit(entry);
            self.forget_dynamic();
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
        self.runtime.alias =
            (memory.ptr.as_ptr() as usize).wrapping_sub(memory.base as usize) as u64;
        self.runtime.alias_classes =
            (memory.perms.as_ptr() as usize).wrapping_sub((memory.base >> 12) as usize) as u64;
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

    /// Empties the indirect table and the shadow stack, which may lead to
    /// dropped units.
    fn forget_dynamic(&mut self) {
        self.table.fill(EMPTY);
        self.runtime.shadow = self.shadow.top();
    }

    /// Units must not run past any stop; a new one clears them all.
    fn observe_stop(&mut self, stop: u32) {
        if self.stops.contains(&stop) {
            return;
        }
        self.stops.push(stop);
        if let Some(word) = &self.interrupt {
            word.fetch_or(
                adv_sync::pending::STOP,
                std::sync::atomic::Ordering::Release,
            );
        }
        self.clear();
    }

    /// Clears the cache as a whole (A64-CTL-1).
    pub(crate) fn clear(&mut self) {
        self.diagnostics.clears += 1;
        self.cache.truncate(self.stub_len);
        self.units.clear();
        self.rows.clear();
        self.table_sources.clear();
        self.uses_permissions = false;
        self.pages.clear();
        self.links.clear();
        self.linked.clear();
        self.placed.clear();
        self.helper_eips = self.helper_histogram();
        self.helpers.clear();
        self.helper_counts.clear();
        self.generation += 1;
        self.faults.sites.clear();
        self.forget_dynamic();
    }

    /// Helper runs by EIP.
    pub(crate) fn helper_histogram(&self) -> FxHashMap<u32, u64> {
        let mut eips = self.helper_eips.clone();
        for (helper, &count) in self.helpers.iter().zip(&self.helper_counts) {
            *eips.entry(helper.pc).or_default() += count;
        }
        eips
    }

    /// Whether code at `eip` is known to run, so translating it pays.
    fn ran(&self, eip: u32) -> bool {
        !self.first_visits || self.seen.contains(&eip)
    }

    /// Whether `eip`, which has no unit, runs for the first time; it will
    /// not be again.
    fn first_visit(&mut self, eip: u32) -> bool {
        self.first_visits && !self.units.contains_key(&eip) && self.seen.insert(eip)
    }

    /// Translates the unit at `eip`, and ahead of time the units its main
    /// path leaves to that have run before, so they share one cache sync
    /// and link directly. Every link point of these units to a unit that
    /// exists is patched.
    fn translate(&mut self, memory: &mut Memory, eip: u32) -> Result<Entry, String> {
        self.points.clear();
        let entry = match self.emit(memory, eip) {
            Some(entry) => entry,
            None => {
                self.clear();
                self.points.clear();
                self.emit(memory, eip)
                    .ok_or("a64 unit does not fit an empty cache")?
            }
        };
        // Only the first unit's main path is translated ahead: deeper
        // successors are less certain to run.
        let first = self.points.len();
        let mut index = 0;
        while let Some(&(point, target, main)) = self.points.get(index) {
            index += 1;
            // The owner must see stops.
            if self.stops.contains(&target) {
                continue;
            }
            let unit = match self.units.get(&target) {
                Some(entry) => entry.unit,
                // A full cache waits for the next miss to clear it.
                None if main && index <= first && self.ran(target) => {
                    match self.emit(memory, target) {
                        Some(entry) => entry.unit,
                        None => break,
                    }
                }
                None => continue,
            };
            self.patch(point, target, unit);
        }
        Ok(entry)
    }

    fn emit(&mut self, memory: &mut Memory, eip: u32) -> Option<Entry> {
        let at = self.cache.next();
        let mut work = std::mem::take(&mut self.work);
        work.unit.images.clone_from(&self.images.borrow());
        let (all, checked) = (self.checks != Checks::Adaptive, &self.checked);
        let control = if self.runtime.cpu.is_null() {
            self.control
        } else {
            // SAFETY: during a run the runtime points at its live CPU, which
            // nothing writes while the owner translates.
            unsafe { (*self.runtime.cpu).x87.control }
        };
        translate::translate(
            &mut work,
            memory,
            eip,
            &self.stops,
            &|pc| all || checked.contains(&pc),
            self.helpers.len() as u32,
            self.features,
            at,
            self.stubs,
            &|pc, word| {
                if self.bridge.is_null() {
                    return None;
                }
                // SAFETY: translation occurs only during the bridge's run.
                unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()
                    .and_then(|handler| handler.host_thunk(pc, word))
            },
            &|pc| {
                if self.bridge.is_null() {
                    return None;
                }
                // SAFETY: as for `host_thunk`.
                let handler = unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()?;
                let (target, original) = handler.redirect(pc)?;
                Some(translate::Redirect {
                    target,
                    original: original.to_vec(),
                    checks: handler.redirect_checks(pc).to_vec(),
                    body: handler.direct_body(pc),
                })
            },
            &|pc, code| {
                if self.bridge.is_null() {
                    return Vec::new();
                }
                // SAFETY: admission runs synchronously on the execution owner.
                unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()
                    .map_or_else(Vec::new, |handler| handler.native_region(pc, code))
            },
            control,
        );
        if work.retired() == 0 {
            self.diagnostics.empty_units[usize::from(work.unit.trace.failure & 3)] += 1;
        }
        let entry = self.place(
            eip,
            work.words(),
            work.sites(),
            work.retired(),
            work.marks(),
        );
        if let Some(entry) = entry {
            debug_assert_eq!(entry.unit, at);
            if !work.unit.tables.is_empty() {
                self.table_sources.insert(eip);
            }
            for table in &work.unit.tables {
                for (index, &target) in table.targets.iter().enumerate() {
                    let slot = (eip, at + table.offset + index * 8, at + table.stub);
                    self.rows.entry(target).or_default().push(slot);
                }
            }
            let mut targets: Vec<_> = work
                .unit
                .tables
                .iter()
                .flat_map(|t| t.targets.iter().copied())
                .collect();
            targets.sort_unstable();
            targets.dedup();
            for target in targets {
                if let Some(entry) = self.units.get(&target) {
                    self.resolve_tables(memory, target, entry.unit);
                }
            }
            self.helpers.extend(work.helpers());
            self.helper_counts.resize(self.helpers.len(), 0);
            let points = work
                .links()
                .iter()
                .map(|link| (at + link.offset, link.eip, link.main));
            self.points.extend(points);
            // The spans are sorted, so a page repeats only next to itself.
            let mut last = None;
            for &(start, len) in work.code() {
                for page in [start & !0xfff, (start + len - 1) & !0xfff] {
                    if last != Some(page) {
                        last = Some(page);
                        self.pages.entry(page).or_default().push(eip);
                    }
                }
            }
        }
        self.uses_permissions |= entry.is_some() && work.code.uses_permissions;
        // Code discovery can protect a host page shared with ordinary data.
        // Refresh permission snapshots before linked code resumes.
        if self.uses_permissions {
            self.classify(memory);
        }
        self.work = work;
        entry
    }

    /// Appends a unit for `eip` whose `sites` are at byte offsets.
    fn place(
        &mut self,
        eip: u32,
        words: &[u32],
        sites: &[(usize, Site)],
        retired: u32,
        marks: &[crate::jit_map::Mark],
    ) -> Option<Entry> {
        let unit = self.cache.append(words)?;
        let from = self.faults.sites.len().saturating_sub(1);
        self.faults
            .sites
            .extend(sites.iter().map(|&(offset, site)| (unit + offset, site)));
        if !self.faults.sites[from..].is_sorted_by_key(|&(at, _)| at) {
            self.faults.sites.sort_unstable_by_key(|&(at, _)| at);
        }
        let entry = Entry { unit, retired };
        crate::jit_map::record(unit, words.len() * 4, eip, retired, marks);
        self.units.insert(eip, entry);
        self.placed.push((unit, eip));
        Some(entry)
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
        let at = native.cache.next();
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
        };
        let words = [
            load,
            sub_imm(X, Reg::new(9), Reg::new(9), 1, false),
            movz(W, Reg::new(0), Exit::Dispatch as u16, 0),
            movz(W, Reg::new(1), next as u16, 0),
            movk(W, Reg::new(1), (next >> 16) as u16, 16),
            b((native.stubs.exit as i64 - (at + 20) as i64) as i32),
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
            native.table[runtime::table_slot(target)] = [u64::from(target), 42];
        }
        native.discard_switch_target(targets[0]);
        assert!(native.table.iter().all(|entry| *entry == EMPTY));
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
