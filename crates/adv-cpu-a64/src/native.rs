//! Translated execution on AArch64 hosts.
//!
//! Tier-1 units come from `translate::jit`. A dispatch does not leave translated code: the
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
//! Code invalidation: the store registers the front end's dependencies with
//! `watch_code`, which write-protects them in the window, so any store to
//! them reaches the interpreter or Rust and marks the page dirty. Dirty
//! pages drop their units before the next dispatch.
//!
//! The execution plane spans `run` and `store`. The `plan` arbiter records
//! decisions, and the store publishes links. The Tier-1 translator and `compile` produce
//! typed artifacts. `offload` alone consumes worker jobs.
//! See A64-OPT-11, A64-OPT-12, A64-OPT-13 and A64-OPT-14.

use crate::translate::lower::stubs;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64};

use adv_cpu_core::CpuState;
use adv_cpu_core::backend::OfferGate;
use adv_cpu_core::memory::accessible;
use adv_cpu_core::memory::{Memory, READ, WRITE};
use adv_cpu_core::trap::{Bridge, call_host, call_trap};

use crate::runtime::{self, Enter, Exit, Runtime, alias_base};
use crate::translate::front::{self, Unit, trace::Redirect};
use crate::translate::jit::Features;
use crate::translate::lower::Stubs;
use crate::translate::lower::resident::ResidentCold;
use crate::translate::opt;

use store::{EMPTY, FaultCapture, Faults, InterpreterCause, ShadowStack, on_fault};

pub(crate) mod compile;
mod offload;
mod plan;
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
pub(crate) const SLICE: u64 = runtime::COMPUTATION_SLICE as u64;
pub(crate) use plan::PROMOTION;

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

#[cfg(test)]
pub(crate) use plan::CHECK_AFTER;

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

/// One trip through translated code.
/// How runs ended and what the cache went through, for reports.
#[derive(Clone, Debug, Default)]
pub(crate) struct Diagnostics {
    pub first_visits: u64,
    pub untracked: u64,
    pub first_declined: u64,
    pub first_quick_decodes: u64,
    pub first_full_decodes: u64,
    pub empty: u64,
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
    pub helper_exits: u64,
    /// Generated exits by raw reason when the JIT map is enabled.
    pub exit_counts: std::collections::BTreeMap<u16, u64>,
    pub x87_value_completions: u64,
    pub x87_value_dispatches: u64,
    /// Proofs whose head loop splits, and how its blocks ran.
    pub split_proofs: u64,
    pub split: offload::SplitCounts,
    /// Detached optimized forms entered by the ordinary owner run loop.
    #[cfg_attr(not(test), expect(dead_code, reason = "reported by Debug"))]
    pub optimized_runs: u64,
    /// Admitted resident entries when the JIT map is enabled.
    pub resident_entries: u64,
    /// Settled heads with a detached proof and a declined resident form.
    pub proof_without_resident: u64,
    #[cfg_attr(not(test), expect(dead_code, reason = "reported by Debug"))]
    pub alone_evaluations: u64,
    /// Training events delivered after a site has stopped sampling links.
    pub rust_decisions_on_decided_sites: u64,
    /// Resident form exits that went through Rust (A64-OPT-9).
    pub resident_cold_exits: u64,
    pub resident_cold_exits_by_reason: std::collections::BTreeMap<ResidentCold, u64>,
    pub resident_cuts_by_reason: std::collections::BTreeMap<ResidentCold, u64>,
    pub resident_refusals_by_reason: std::collections::BTreeMap<ResidentCold, u64>,
    pub resident_demotions_by_reason: std::collections::BTreeMap<ResidentCold, u64>,
    pub resident_page_crossings: u64,
    pub plan_events: std::collections::BTreeMap<&'static str, u64>,
    pub sensor_decisions: std::collections::BTreeMap<&'static str, u64>,
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
    internal_discovered: Vec<BackEdgeSample>,
}

struct BackEdgeSample {
    generation: u64,
    source: u32,
    unit: usize,
    at: CpuState,
}

unsafe extern "C" fn plan_observation(
    runtime: *mut Runtime,
    eip: u32,
    point: usize,
    kind: u16,
) -> u32 {
    // SAFETY: the threshold stub publishes the head's state and keeps the
    // live Tier-1 registers on its stack while the owner records the sample.
    let (owner, cpu) = unsafe { (&mut *((*runtime).owner as *mut Owner), &*(*runtime).cpu) };
    let native = unsafe { &mut *owner.native };
    if kind == runtime::CHECK_OFFER {
        native.count_plan_event("offer_check");
        let job = native.plans.job(eip).unwrap();
        job.defer_offer();
        return u32::from(job.offer_due(native.published_gate(), ticks));
    }
    if kind == runtime::RESIDENT_VERSION_MISS {
        native.count_plan_event("resident_version_miss");
        let mut at = cpu.clone();
        at.eip = eip;
        native.retry_resident(&at, point);
        return 0;
    }
    if kind == runtime::RESIDENT_LINK_SAMPLE {
        native.count_plan_event("resident_link_sample");
        native.sample_resident_link(cpu, eip, point);
        return 0;
    }
    native.count_plan_event("internal_discovery");
    let mut at = cpu.clone();
    let edge = native.code.internal_edges[&eip]
        .iter()
        .find(|edge| edge.point == point)
        .unwrap();
    at.eip = eip;
    owner.internal_discovered.push(BackEdgeSample {
        generation: native.code.generation,
        source: edge.source,
        unit: edge.unit,
        at,
    });
    native.stop_counting(eip);
    0
}

/// Performs the checked access. Refusal records the fault site and returns
/// its publication continuation and register capture. Code stores replay
/// at the owner so the write can invalidate affected units.
unsafe extern "C" fn access(
    runtime: *mut Runtime,
    address: u32,
    site: usize,
    value: u32,
    kind: u32,
) -> runtime::AccessResult {
    // SAFETY: as in `dispatch`.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    native.diagnostics.accesses += 1;
    let width = (kind & 31) as usize;
    // SAFETY: the runtime is live; the cold path shares this transfer field.
    let vector = unsafe { &mut (*runtime).value_frame.operand };
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
        Some(value) => runtime::AccessResult {
            value: u64::from(value),
            recovery: std::ptr::null_mut(),
        },
        None => {
            native.diagnostics.refused += 1;
            let faults = &native.code.faults;
            faults.capture.pc.set(site);
            let resume = faults.site(site).map_or(0, |site| site.resume);
            runtime::AccessResult {
                value: if resume != 0 {
                    site.wrapping_add_signed(resume as isize) as u64
                } else {
                    0
                },
                recovery: faults.capture.registers.as_ptr(),
            }
        }
    }
}

/// Evaluates the prepared operation on only the supplied values.
unsafe extern "C" fn value(runtime: *mut Runtime, index: u32) -> u32 {
    // SAFETY: the generated call belongs to the current owner run.
    let runtime = unsafe { &mut *runtime };
    let owner = unsafe { &mut *(runtime.owner as *mut Owner) };
    let native = unsafe { &mut *owner.native };
    native.note_value_call(index);
    let helper = &native.code.helpers[index as usize];
    let operation = helper.value.as_ref().expect("a prepared value instruction");
    let x87 = operation.x87_operands();
    if let Some(x87) = &x87
        && !x87.restore_fx
    {
        // SAFETY: generated code publishes the x87 sub-state before calling.
        runtime
            .value_frame
            .x87
            .clone_from(unsafe { &(*runtime.cpu).x87 });
    }
    match operation.evaluate(&mut runtime.value_frame) {
        adv_cpu_core::floating::ValueOutcome::Complete => {
            if let Some(x87) = x87 {
                if native.code.slow_paths.is_some() {
                    native.diagnostics.x87_value_completions += 1;
                }
                // SAFETY: the run owns the CPU through its value call.
                let cpu = unsafe { &mut *runtime.cpu };
                cpu.x87.clone_from(&runtime.value_frame.x87);
                cpu.refresh_mmx();
                if x87.restore_fx {
                    cpu.xmm = runtime.value_frame.xmm;
                    cpu.mxcsr = runtime.value_frame.mxcsr;
                }
            }
            0
        }
        adv_cpu_core::floating::ValueOutcome::Exception(vector) => u32::from(vector),
        adv_cpu_core::floating::ValueOutcome::Unsupported => runtime::VALUE_UNSUPPORTED,
        adv_cpu_core::floating::ValueOutcome::Store { .. } => runtime::VALUE_STORE_READY,
    }
}

/// Access stub for a LOCK read-modify-write in shared storage.
unsafe extern "C" fn locked_access(
    runtime: *mut Runtime,
    index: u32,
    nzcv: u32,
    parity: u32,
    adjust: u32,
    direction: u32,
) -> u32 {
    // SAFETY: the call is made by the current owner against its live frame.
    let runtime = unsafe { &mut *runtime };
    let owner = unsafe { &mut *(runtime.owner as *mut Owner) };
    let native = unsafe { &mut *owner.native };
    native.note_value_call(index);
    let operation = native.code.helpers[index as usize]
        .locked_value
        .expect("prepared locked operation");
    let frame = &mut runtime.value_frame;
    frame.eflags = (nzcv >> 24 & 0xc0)
        | (!nzcv >> 29 & 1)
        | (nzcv >> 17 & 0x800)
        | (u32::from((parity as u8).count_ones().is_multiple_of(2)) << 2)
        | (adjust & 0x10)
        | (direction << 10);
    u32::from(
        operation
            .update(
                unsafe { &mut *owner.memory },
                frame.address,
                &mut frame.regs,
                &mut frame.eflags,
            )
            .is_err(),
    )
}

impl Native {
    fn note_value_call(&mut self, index: u32) {
        #[cfg(test)]
        {
            let pc = self.code.helpers[index as usize].pc;
            *self.code.value_counts.entry(pc).or_default() += 1;
        }
        if let Some(paths) = &mut self.code.slow_paths {
            let pc = self.code.helpers[index as usize].pc;
            *paths.values.entry(pc).or_default() += 1;
        }
    }
}

/// Transfer the staged value operand independently of semantic evaluation.
unsafe extern "C" fn value_access(runtime: *mut Runtime, width: u32, phase: u32) -> u32 {
    // SAFETY: the emitted calls run on the memory's owner using its live frame.
    let runtime = unsafe { &mut *runtime };
    let owner = unsafe { &mut *(runtime.owner as *mut Owner) };
    let memory = unsafe { &mut *owner.memory };
    let at = runtime.value_frame.address;
    let bytes = &mut runtime.value_frame.operand[..width as usize];
    let ready = match phase {
        0 => {
            adv_cpu_core::floating::check_access(memory, 0, at, bytes.len(), false).is_ok()
                && memory.read_into(at, bytes).is_ok()
        }
        1 => {
            adv_cpu_core::floating::check_access(memory, 0, at, bytes.len(), true).is_ok()
                && !memory.stores_code(at, bytes.len())
        }
        3 => !memory.stores_code(at, bytes.len()) && memory.write(at, bytes).is_ok(),
        _ => {
            if memory.write(at, bytes).is_err() {
                return runtime::VALUE_ACCESS_REFUSED;
            }
            // SAFETY: the write completed before the staged x87 result commits.
            let cpu = unsafe { &mut *runtime.cpu };
            cpu.x87.clone_from(&runtime.value_frame.x87);
            cpu.refresh_mmx();
            let native = unsafe { &mut *owner.native };
            if native.code.slow_paths.is_some() {
                native.diagnostics.x87_value_completions += 1;
            }
            true
        }
    };
    if ready {
        0
    } else {
        runtime::VALUE_ACCESS_REFUSED
    }
}

/// Called by the `call` stub: EIP in bits 0-31, thunk in 32-47, INT length
/// in 48-51, and bit 63 for an unrecognized INT (A64-CTL-12).
unsafe extern "C" fn call(runtime: *mut Runtime, site: u64, _countdown: u64) -> u64 {
    const LEAVE: u32 = Exit::Dispatch.raw() + 1;
    // SAFETY: as in `dispatch`.
    let owner = unsafe { &mut *((*runtime).owner as *mut Owner) };
    let (native, memory) = unsafe { (&mut *owner.native, &mut *owner.memory) };
    let cpu = unsafe { &mut *(*runtime).cpu };
    let pc = site as u32;
    let len = (site >> 48 & 15) as u8;
    if owner.bridge.is_null() {
        cpu.eip = pc;
        return u64::from(Exit::Event(runtime::Event::Software { vector: 0x2e, len }).raw() + 1);
    }
    if native.pending_work() {
        cpu.eip = pc;
        return (-1i64 as u64) << 32 | u64::from(LEAVE);
    }
    // SAFETY: as in `dispatch`.
    let bridge = unsafe { &mut *owner.bridge.cast::<Bridge<'_>>() };
    // The INT has committed its next PC.
    let next = pc.wrapping_add(u32::from(len));
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
    code: store::CodeStore<compile::ResidentRecovery>,
    decodes: front::Decodes,
    front_units: FxHashMap<u32, front::Site>,
    plans: plan::Arbiter,
    pub(crate) images: std::rc::Rc<std::cell::RefCell<Vec<(u32, u32)>>>,
    bridge: *mut std::ffi::c_void,
    pub(crate) interrupt: Option<Arc<std::sync::atomic::AtomicU32>>,
    pub(crate) pending_mask: u32,
    /// Whether entries run once before they are translated.
    pub(crate) first_visits: bool,
    pub(crate) checks: Checks,
    /// Faults at unchecked sites so far.
    pub(crate) unchecked_faults: u64,
    /// The run's wall deadline as a virtual counter value.
    pub(crate) deadline: Option<u64>,
    /// Diagnostic counts of how runs end; see `Diagnostics`.
    pub(crate) diagnostics: Diagnostics,
    pub(crate) retired_forms: Option<[u64; 5]>,
    /// The memory epoch last classified, and the all-slow test table.
    classified_epoch: Option<u64>,
    classes: Option<Arc<Vec<u8>>>,
    #[cfg(test)]
    classifications: u64,
    /// Stop addresses seen so far; units end before all of them.
    stops: Vec<u32>,
    /// Budget exits an entry needs before it is promoted; zero translates
    /// every entry with loop traces.
    pub(crate) promotion: u8,
    pub(crate) count_resident_entries: bool,
    /// The frame's pace when the owner last reached a loop whose blocks
    /// split (A64-TR-9).
    pub(crate) pace: std::cell::Cell<Option<adv_cpu_core::backend::Pace>>,
    offload: offload::Offload,
    worker_jobs: FxHashMap<u32, offload::WorkerJob<Arc<Unit>>>,
    scoped_jobs: offload::ScopedJobs<Arc<Unit>>,
    pub(crate) scoped_offer: Option<runtime::ScopedOffer>,
    /// Where `opt_exit` saves an inline form's spill cells.
    inline_saved: Vec<u128>,
    /// Builds optimized forms, which `install_optimized` adopts.
    compiler: compile::Compiler,
    pub(crate) features: Features,
    runtime: Box<Runtime>,
    /// Live timestamp bridge, including runs without a trap handler.
    pub(crate) clock: *mut std::ffi::c_void,
}

impl Native {
    pub(crate) fn value_histogram(&self) -> FxHashMap<u32, u64> {
        self.code
            .slow_paths
            .as_ref()
            .map_or_else(FxHashMap::default, |paths| {
                paths
                    .values
                    .iter()
                    .map(|(&eip, &count)| (eip, count))
                    .collect()
            })
    }

    pub(crate) fn note_interpretation(
        &mut self,
        eip: u32,
        event: runtime::Event,
        invalid_encoding: bool,
    ) {
        use runtime::Event;
        let Some(paths) = &mut self.code.slow_paths else {
            return;
        };
        let cause = match event {
            Event::Replay => InterpreterCause::HostFault,
            Event::CheckedAccess => InterpreterCause::OperandFault,
            Event::DebugFlags => InterpreterCause::DebugFlags,
            Event::PendingX87 => InterpreterCause::Trap(16),
            Event::System => InterpreterCause::System,
            Event::Fetch if invalid_encoding => InterpreterCause::InvalidEncoding,
            Event::Fetch => InterpreterCause::Fetch,
            _ => unreachable!("completed event"),
        };
        *paths.interpreter.entry((eip, cause)).or_default() += 1;
    }

    #[cfg(test)]
    pub(crate) fn value_calls_at(&self, pc: u32) -> u64 {
        self.code.value_counts.get(&pc).copied().unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn span_counts(
        &self,
        head: u32,
    ) -> [u64; crate::translate::lower::resident::COUNTERS] {
        let mut counts = self.code.version_stats.as_ref().unwrap()[&head]
            .spans
            .each_ref()
            .map(Cell::get);
        if let Some(feedback) = self
            .code
            .resident(head)
            .and_then(|site| site.row_feedback.as_deref())
        {
            counts[21] = feedback.failures[0].get();
            counts[22] = feedback.failures[1..].iter().map(Cell::get).sum();
        }
        counts
    }

    pub(crate) fn version_diagnostics(&self) -> Option<String> {
        use std::fmt::Write as _;
        let stats = self.code.version_stats.as_ref()?;
        let mut entries: Vec<_> = stats.iter().collect();
        entries.sort_unstable_by_key(|(eip, _)| **eip);
        let mut output = String::new();
        for (&eip, stats) in entries {
            let mut counts = stats.spans.each_ref().map(Cell::get);
            if let Some(feedback) = self
                .code
                .resident(eip)
                .and_then(|site| site.row_feedback.as_deref())
            {
                let reasons = feedback.failures.each_ref().map(Cell::get);
                counts[21] = reasons[0];
                counts[22] = reasons[1..].iter().sum();
                let _ = write!(
                    output,
                    " row failures {eip:#010x} [target {}, range {}, alias {}, parameter {}, count {}, span {}]",
                    reasons[0], reasons[1], reasons[2], reasons[3], reasons[4], reasons[5]
                );
            }
            let (jobs, offers, splits) = (0..plan::VERSIONS as u8)
                .filter_map(|version| self.scoped_jobs.get(&(eip, version)))
                .fold((0, 0u64, 0), |(jobs, offers, splits), job| {
                    (
                        jobs + 1,
                        offers + job.policy.scope_offers.get(),
                        splits + job.policy.scope_splits.get(),
                    )
                });
            if jobs != 0 {
                let declines = offers.saturating_sub(splits);
                let _ = write!(
                    output,
                    " row splits {eip:#010x} [offers {offers}, declines {declines}, splits {splits}]"
                );
            }
            if counts[23] != 0 {
                let _ = write!(
                    output,
                    " row arrivals {eip:#010x} [total {}, short {}]",
                    counts[23], counts[24]
                );
            }
            if counts.iter().any(|&n| n != 0) {
                let _ = write!(
                    output,
                    " span {eip:#010x}=[entries {}, mmx misses {}, range misses {}, batches {}, ordered classes {:?}, row entries {}, target misses {}, row misses {}]",
                    counts[0],
                    counts[1],
                    counts[2],
                    counts[3],
                    &counts[4..20],
                    counts[20],
                    counts[21],
                    counts[22]
                );
            }
            let versions = self
                .code
                .tier1_chain(eip)
                .map_or(0, |chain| chain.versions.len());
            let arrivals =
                std::array::from_fn::<_, { store::CHAIN_LEN }, _>(|n| stats.arrivals[n].get());
            let _ = write!(
                output,
                " {eip:#010x}=[builds {}, misses {}, joins {}, versions {versions}, arrivals {arrivals:?}, resident next {}, resident tier1 {}]",
                stats.built,
                stats.misses,
                stats.joins,
                stats.resident_misses[0].get(),
                stats.resident_misses[1].get()
            );
        }
        for (&reason, &count) in &self.diagnostics.exit_counts {
            let exit = Exit::from_raw(u32::from(reason)).expect("recorded exit");
            let _ = write!(output, " exit {exit:?}={count}");
        }
        if let Some(paths) = &self.code.slow_paths {
            for (&(eip, cause), &count) in &paths.interpreter {
                let _ = write!(output, " interpreter {eip:#010x} {cause:?}={count}");
            }
            for (&eip, &count) in &paths.values {
                let _ = write!(output, " value {eip:#010x}={count}");
            }
        }
        Some(output)
    }
}

impl Native {
    /// `window` is the host address of guest address 0 in the guest's
    /// `GuestWindow`, which must outlive this.
    pub fn new(window: usize) -> Result<Self, String> {
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
            let result = unsafe { access(runtime, address, site, value, kind) };
            if result.recovery.is_null() {
                result.value
            } else {
                1 << 32
            }
        }

        let mut stubs = stubs::stubs();
        let access_entry = stubs.code.len() * 4;
        stubs
            .code
            .extend(opt::back::access_stub(resident_access as *const () as u64));
        let (cache, base) = store::place_stubs(CACHE_LEN, &stubs.code)?;
        crate::jit_map::record_stubs(
            base,
            base + 4 * stubs.code.len(),
            &[
                (stubs.enter, "enter"),
                (stubs.opt_enter, "opt_enter"),
                (stubs.exit, "exit"),
                (stubs.resident_exit, "resident_exit"),
                (stubs.fault, "fault"),
                (stubs.dispatch, "dispatch"),
                (stubs.lookup, "lookup"),
                (stubs.lookup_slow, "lookup_slow"),
                (stubs.ret_miss, "ret_miss"),
                (stubs.shadow_reset, "shadow_reset"),
                (stubs.access, "access"),
                (access_entry, "resident_access"),
                (stubs.call, "call"),
            ],
        );
        // SAFETY: `enter` and `opt_enter` follow the C calling convention with
        // the signature of `Enter`; see the entry emitters in `translate::lower::stubs`.
        let enter = Enter(base + stubs.enter);
        let opt_enter = Enter(base + stubs.opt_enter);
        let mut table = empty_table();
        let shadow = ShadowStack::new()?;
        let mut runtime = Box::new(Runtime::new(std::ptr::null_mut(), window as u64));
        runtime.table = table.as_mut_ptr() as u64;
        runtime.resident_access = (base + access_entry) as u64;
        runtime.plan_observation = plan_observation as *const () as u64;
        runtime.shadow_top = shadow.top();
        runtime.shadow = shadow.top();
        Ok(Self {
            decodes: front::Decodes::default(),
            front_units: FxHashMap::default(),
            code: store::CodeStore {
                resident_exit: base + stubs.resident_exit,
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
                    call: base + stubs.call,
                    fault: base + stubs.fault,
                    opt_exit: base + stubs.opt_exit,
                },
                chains: FxHashMap::default(),
                version_stats: crate::jit_map::enabled().then(FxHashMap::default),
                slow_paths: crate::jit_map::enabled().then(store::SlowPaths::default),
                #[cfg(test)]
                value_counts: FxHashMap::default(),
                pages: FxHashMap::default(),
                links: FxHashMap::default(),
                linked: Vec::new(),
                points: Vec::new(),
                back_points: FxHashSet::default(),
                direct_points: FxHashMap::default(),
                point_facts: FxHashMap::default(),
                internal_edges: FxHashMap::default(),
                tier1_words: FxHashMap::default(),
                uses_permissions: false,
                placed: Vec::new(),
                helpers: Vec::new(),
                generation: 0,
                table,
                shadow,
            },
            plans: plan::Arbiter::new(
                std::env::var_os("ADV_A64_BASELINE").is_some_and(|value| value == "1"),
            ),
            images: Default::default(),
            bridge: std::ptr::null_mut(),
            interrupt: None,
            pending_mask: u32::MAX,
            first_visits: true,
            checks: Checks::Adaptive,
            unchecked_faults: 0,
            deadline: None,
            diagnostics: Diagnostics::default(),
            classified_epoch: None,
            #[cfg(test)]
            classifications: 0,
            classes: None,
            retired_forms: None,
            stops: Vec::new(),
            promotion: PROMOTION,
            count_resident_entries: crate::jit_map::enabled(),
            pace: std::cell::Cell::new(None),
            offload: offload::Offload::default(),
            worker_jobs: FxHashMap::default(),
            scoped_jobs: FxHashMap::default(),
            scoped_offer: None,
            inline_saved: Vec::new(),
            compiler: compile::Compiler::new(),
            features: features(),
            runtime,
            clock: std::ptr::null_mut(),
        })
    }

    pub(crate) fn count_forms(&mut self, enabled: bool) {
        self.retired_forms = enabled.then_some([0; 5]);
        if enabled {
            crate::jit_map::enable_memory();
        }
    }

    pub(crate) fn set_body_services(&mut self, services: adv_cpu_core::body::Services) {
        self.runtime.body.services = services;
    }

    pub(crate) fn set_spin_yield(&mut self, enabled: bool) {
        self.runtime.spin_yield = u32::from(enabled);
    }

    pub(crate) fn translation_decodes(&self) -> front::DecodeCounts {
        self.decodes.counts
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
        self.code.tier1(eip).map(|entry| entry.unit)
    }

    /// Guest instructions whose accesses check their page.
    pub(crate) fn checked_count(&self) -> usize {
        self.plans.checked_count()
    }

    fn front_unit(
        &mut self,
        memory: &mut Memory,
        eip: u32,
        loops: bool,
        summarize: bool,
        observed: Option<&[u32; 8]>,
    ) -> Arc<Unit> {
        let site = self.front_units.entry(eip).or_default();
        let mut guarded = None;
        let shape = if observed.is_some() {
            &mut guarded
        } else if loops {
            &mut site.hot
        } else {
            &mut site.cold
        };
        let unit = shape.get_or_insert_with(|| {
            let handler = if self.bridge.is_null() {
                None
            } else {
                // SAFETY: preparation stays within this owner's bridge call.
                unsafe { &*self.bridge.cast::<Bridge<'_>>() }
                    .handler
                    .as_ref()
            };
            let mut unit = Unit::default();
            unit.images.clone_from(&self.images.borrow());
            unit.build_cached(
                memory,
                eip,
                &self.stops,
                &|pc, word| handler.and_then(|handler| handler.host_thunk(pc, word)),
                &|pc| {
                    let handler = handler?;
                    let (target, original) = handler.redirect(pc)?;
                    Some(Redirect {
                        target,
                        original: original.to_vec(),
                        checks: handler.redirect_checks(pc).to_vec(),
                        body: handler.direct_body(pc),
                    })
                },
                (loops, observed),
                &mut self.decodes,
            );
            self.code
                .watch_pages(memory, eip, unit.code(), &mut self.decodes);
            unit.prepare_regions(memory, &|pc, code| {
                handler.map_or_else(Vec::new, |handler| handler.native_region(pc, code))
            });
            Arc::new(unit)
        });
        if summarize && unit.trace.x86.is_empty() {
            Arc::make_mut(unit).summarize(&mut self.decodes);
        }
        Arc::clone(unit)
    }

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
            memory: self.runtime.body.memory,
        };
        self.classified_epoch = Some(memory.epoch);
    }
}

#[cfg(test)]
mod tests {
    use adv_cpu_core::memory::Memory;

    use super::store::{Linked, recover};
    use super::*;
    use crate::encode::Size::{W, X};
    use crate::encode::{Reg, b, ldr, ldr_reg, ldur, movk, movz, sub_imm};
    use crate::translate::{self, Carry, Operand, Recipe, Sense, Site};

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
            overflow: crate::translate::lower::flags::Overflow::Host,
            recipe: Recipe::Canonical,
            simd_dirty: 0,
            resume: 0,
            opt_record: None,
            checked: false,
        };
        let words = [
            load,
            sub_imm(X, Reg::new(9), Reg::new(9), 1, false),
            movz(W, Reg::new(0), Exit::Dispatch.word(), 0),
            movz(W, Reg::new(1), next as u16, 0),
            movk(W, Reg::new(1), (next >> 16) as u16, 16),
            b((native.code.stubs.exit as i64 - (at + 20) as i64) as i32),
        ];
        let unit = native
            .code
            .install_unit(eip, &words, &[(0, site)], 1, &[], Default::default())
            .unwrap();
        native.code.set_tier1(
            eip,
            store::Tier1Chain {
                unit: Arc::default(),
                mask: 0,
                helpers: 0,
                versions: vec![store::Version {
                    key: crate::facts::Key::default(),
                    entry: unit,
                }],
            },
        );
    }

    // spec: A64A-PLACE-2 A64A-ROLE-5
    #[test]
    fn proved_link_enters_past_the_guards() {
        use super::store::{Tier1Chain, Version};
        use crate::facts::Key;

        let (_, mut native) = native();
        let point = native.code.cache.append(&[b(4)]).unwrap();
        let eip = 0x1010;
        let key = Key::x87(0x027f, false);
        let version = native
            .code
            .install_unit(
                eip,
                &[b(4), b(4), b(4)],
                &[],
                1,
                &[],
                crate::translate::lower::VersionLayout {
                    body: 4,
                    miss: Some(8),
                },
            )
            .unwrap();
        native.code.set_tier1(
            eip,
            Tier1Chain {
                unit: Arc::default(),
                mask: key.mask(),
                helpers: 0,
                versions: vec![Version {
                    key,
                    entry: version,
                }],
            },
        );
        native.code.point_facts.insert(point, key);
        native.patch(point, eip, version.unit);
        assert_eq!(
            native.code.cache.word(point),
            store::branch(point, version.body)
        );
        assert!(native.code.invalidate_unit(eip));
        assert!(!native.code.chains.contains_key(&eip));
        assert_eq!(native.code.cache.word(point), b(4));
    }

    // spec: A64A-PLACE-2
    #[test]
    fn empty_fact_key_emits_the_original_integer_unit() {
        use crate::facts::Key;

        let (mut memory, native, stop) = resident_loop(&[0x31, 0xc0, 0x40]);
        let mut work = compile::Workspace::default();
        let at = native.code.cache.next();
        compile::translate(
            &mut work,
            &mut memory,
            HEAD,
            &[stop],
            &|_| false,
            0,
            Features::default(),
            at,
            native.code.stubs,
            &|_, _| None,
            &|_| None,
            &|_, _| Vec::new(),
            0x027f,
            false,
        );
        let original = work.words().to_vec();
        assert_eq!(work.unit.fact_mask(), 0);
        translate::jit::lower_tier1(
            &mut work.code,
            &work.unit,
            HEAD,
            &|_| false,
            0,
            Features::default(),
            at,
            native.code.stubs,
            work.resident_links,
            Key::default(),
            0,
        );
        assert_eq!(work.words(), original);
        assert_eq!(work.code.version.body, 0);
        assert_eq!(work.code.version.miss, None);
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

    // spec: A64A-FALL-3
    #[test]
    fn fact_transfer_at_an_unkeyed_entry_reuses_the_tier1_body() {
        use crate::facts::{self, Key};

        let (mut memory, mut native, _) = resident_loop(STORE_LOOP);
        native.code.version_stats = Some(FxHashMap::default());
        let ordinary = native.translate(&mut memory, HEAD, Key::default()).unwrap();
        assert_eq!(ordinary.unit, ordinary.body);
        let observed = Key::read(&resident_cpu(HEAD, 1), facts::ALL);
        assert_eq!(
            native.miss_version(&mut memory, HEAD, observed).unwrap(),
            ordinary.body,
        );
        assert!(native.code.version_stats.as_ref().unwrap().is_empty());
    }

    // spec: A64A-ROLE-2, A64A-PLACE-4
    #[test]
    fn tier1_and_resident_rebuilds_reuse_the_site_unit() {
        let (mut memory, mut native, _) = resident_loop(STORE_LOOP);
        native
            .translate(&mut memory, HEAD, crate::facts::Key::default())
            .unwrap();
        let first = native.translation_decodes();
        native.plans.mark_checked(HEAD);
        native.replace_unit(&mut memory, HEAD, crate::facts::Key::default());
        assert_eq!(native.translation_decodes(), first);

        native.discover(&mut memory, HEAD, HEAD, resident_cpu(HEAD, 20));
        native.install_optimized(true);
        let discovered = native.translation_decodes();
        assert_eq!(
            discovered, first,
            "resident discovery shares the cold prefix"
        );
        assert!(!native.code.resident(HEAD).unwrap().versions.is_empty());
        native.rebuild_resident_versions(HEAD);
        native.install_optimized(true);
        assert_eq!(native.translation_decodes(), discovered);

        native.clear();
        native
            .translate(&mut memory, HEAD, crate::facts::Key::default())
            .unwrap();
        assert_eq!(native.translation_decodes(), discovered);

        memory.protect(HEAD, 4096, READ | WRITE).unwrap();
        memory.write(HEAD, &[0x89, 0x17]).unwrap(); // mov [edi],edx
        memory
            .protect(HEAD, 4096, READ | adv_cpu_core::EXEC)
            .unwrap();
        native.invalidate(&mut memory);
        assert!(!native.front_units.contains_key(&HEAD));
        native
            .translate(&mut memory, HEAD, crate::facts::Key::default())
            .unwrap();
        assert!(native.translation_decodes().quick > discovered.quick);
    }

    // spec: A64A-ROLE-2
    #[test]
    fn promotion_decodes_only_the_new_trace_suffix() {
        let mut body = vec![0x90; translate::MAX_INSTRUCTIONS];
        body.extend_from_slice(&[0x49, 0x75, (-(body.len() as i32 + 3)) as u8, 0xcc]);
        let (mut memory, mut native, _) = resident_loop(&body);
        native.promotion = 1;
        native.first_visits = true;
        native
            .translate(&mut memory, HEAD, crate::facts::Key::default())
            .unwrap();
        let cold = native.translation_decodes();
        assert_eq!(cold.quick, translate::MAX_INSTRUCTIONS as u64);
        native.sample(&mut memory, &resident_cpu(HEAD, 20));
        let hot = native.translation_decodes();
        assert_eq!(hot.quick - cold.quick, 2);
        assert_eq!(hot.full, cold.full);
        assert_eq!(hot.code_reads - cold.code_reads, 2);
    }

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

    // spec: A64-OPT-11 A64-OPT-12 A64-OPT-14
    #[test]
    fn worker_only_links_offer_again_after_backoff() {
        let (mut memory, mut native, stop) = resident_loop(STORE_LOOP);
        native.prepare_job(&mut memory, stop, &resident_cpu(HEAD, 20));
        native.set_computation_offer(true, None);
        let gate = native.code.offer(HEAD).unwrap().0;
        assert!(native.plans.resident(HEAD).unwrap().declined());
        native.plans.job(HEAD).unwrap().reoffer.set(3);
        for left in [2, 1] {
            let mut state = resident_cpu(OUTSIDE, 20);
            let result = enter(&mut native, &mut memory, stop, &mut state);
            assert_eq!((result.exit, state.eip), (Exit::Dispatch, stop));
            assert_eq!(native.plans.job(HEAD).unwrap().reoffer.get(), left);
        }
        let mut state = resident_cpu(OUTSIDE, 20);
        let result = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((result.exit, state.eip), (Exit::Computation, HEAD));
        assert_eq!(native.plans.job(HEAD).unwrap().reoffer.get(), plan::REOFFER);
        let link = outside_link(&native, HEAD);
        assert_eq!(native.code.cache.word(link.point), link.branch(gate));
        assert_eq!(native.diagnostics.optimized_runs, 0);
        assert_eq!(native.diagnostics.rust_decisions_on_decided_sites, 0);
    }

    // spec: A64-OPT-12 A64-OPT-13
    #[test]
    fn baseline_plans_preserve_tier1_results_and_counters() {
        let run = |baseline_only, count| {
            let (mut memory, mut native, stop) = resident_loop(STORE_LOOP);
            native.plans.baseline_only = baseline_only;
            native.count_resident_entries = true;
            let mut state = resident_cpu(OUTSIDE, count);
            let first = enter(&mut native, &mut memory, stop, &mut state);
            let mut retired = u64::from(first.retired);
            while state.eip != stop {
                retired += u64::from(enter(&mut native, &mut memory, stop, &mut state).retired);
            }
            let diag = &native.diagnostics;
            assert_eq!(diag.rust_decisions_on_decided_sites, 0);
            assert_eq!(diag.optimized_runs, 0);
            assert_eq!(diag.alone_evaluations, 0);
            if baseline_only {
                assert!(native.plans.resident(HEAD).is_none());
                assert_eq!(diag.resident_entries, 0);
            }
            let memory = memory
                .read_data(OUTPUT, (count * 4) as usize)
                .unwrap()
                .to_vec();
            (state, retired, memory, diag.tier1_counts())
        };
        let (state, retired, memory, counters) = run(false, 8);
        let baseline = run(true, 8);
        assert_eq!(
            (state, retired, memory),
            (baseline.0, baseline.1, baseline.2)
        );
        assert_eq!(counters, baseline.3);

        let enabled = run(false, plan::HOT + 1);
        let baseline = run(true, plan::HOT + 1);
        assert_eq!(
            (enabled.0, enabled.1, enabled.2),
            (baseline.0, baseline.1, baseline.2)
        );
    }

    // spec: A64-OPT-8, A64-OPT-9
    #[test]
    fn resident_links_survive_tier1_promotion() {
        let (mut memory, mut native, stop) = resident_loop(STORE_LOOP);
        let invalidations = native.diagnostics.global_invalidations;
        let mut state = resident_cpu(OUTSIDE + 16, plan::HOT);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 2 + 4 * plan::HOT));
        assert_eq!(native.plans.counter_at(HEAD).unwrap().get(), 1);
        assert!(!native.plans.resident(HEAD).is_some());
        let unit = native.code.tier1(HEAD).unwrap().unit;
        let table = runtime::table_slot(HEAD);
        assert_eq!(native.code.table[table], [u64::from(HEAD), unit as u64]);

        let mut state = resident_cpu(OUTSIDE, 5);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 22));
        let edge = &native.code.internal_edges[&HEAD][0];
        let (back, transition, original_target, resident) =
            (edge.point, edge.transition, edge.target, edge.link);
        let link = outside_link(&native, HEAD);
        let (outside, unresolved) = (link.point, link.original);
        let arrival = link.direct.unwrap();
        assert_eq!(native.code.resident(HEAD).unwrap().versions.len(), 1);
        let resident_entry = native.code.resident(HEAD).unwrap().entry().unwrap();
        assert_eq!(
            native.code.cache.word(back),
            store::branch(back, transition)
        );
        assert_eq!(
            native.code.cache.word(resident),
            store::branch(resident, resident_entry)
        );
        assert_eq!(
            native.code.cache.word(outside),
            store::branch(outside, arrival)
        );
        assert_eq!(native.code.table[table][1], unit as u64);

        native.promotion = 1;
        native.sample(
            &mut memory,
            &CpuState {
                eip: HEAD,
                ..CpuState::default()
            },
        );
        let new = native.code.tier1(HEAD).unwrap().unit;
        assert_ne!(new, unit);
        assert_eq!(native.diagnostics.promotions, 1);
        assert_eq!(
            native.code.cache.word(back),
            store::branch(back, transition)
        );
        assert_eq!(
            native.code.cache.word(resident),
            store::branch(resident, resident_entry)
        );
        assert_eq!(
            native.code.cache.word(outside),
            store::branch(outside, arrival)
        );
        assert_eq!(native.code.table[table][1], new as u64);
        let fallback = native.code.resident(HEAD).unwrap().versions[0].cold;
        assert_eq!(
            native.code.cache.word(fallback),
            store::branch(fallback, new)
        );
        assert_eq!(native.diagnostics.global_invalidations, invalidations);

        memory.protect(HEAD, 0x4000, READ | WRITE).unwrap();
        memory.write(HEAD, &[0x90]).unwrap();
        native.invalidate(&mut memory);
        assert!(!native.plans.resident(HEAD).is_some());
        assert_eq!(
            native.code.cache.word(back),
            store::branch(back, original_target)
        );
        assert_eq!(native.code.cache.word(outside), unresolved);
        assert_eq!(native.code.table[table], EMPTY);
    }

    // spec: A64A-ROLE-5 A64A-PLACE-2 A64A-PLACE-3 A64-OPT-8
    #[test]
    fn shared_chain_settled_arrivals_keep_their_word_budget() {
        fn target(native: &Native, point: usize) -> usize {
            let word = native.code.cache.word(point);
            assert_eq!(word >> 26, 0b000101);
            point.wrapping_add_signed(((word << 6) as i32 >> 4) as isize)
        }

        let (mut memory, mut native, stop) = resident_loop(STORE_LOOP);
        native.promotion = 0;
        native.count_resident_entries = true;
        assert!(native.force_resident(&mut memory, HEAD));
        let ordinary = native.code.tier1(HEAD).unwrap();
        let resident = native.code.resident(HEAD).unwrap();
        let (probe, checked) = (resident.versions[0].probe, resident.versions[0].entry);
        native.code.settle_sample(HEAD);
        assert_eq!(target(&native, probe), checked);

        for (start, retired) in [(HEAD, 4), (OUTSIDE + 16, 6), (OUTSIDE + 16, 6)] {
            let before = native.diagnostics.resident_entries;
            let mut state = resident_cpu(start, 1);
            let run = enter(&mut native, &mut memory, stop, &mut state);
            assert_eq!((state.eip, run.retired), (stop, retired));
            assert_eq!(native.diagnostics.resident_entries, before);
        }
        assert_eq!(native.code.tier1(HEAD).unwrap().body, ordinary.unit);
        let slot = &native.code.table[runtime::table_slot(HEAD)];
        assert_eq!(*slot, [u64::from(HEAD), ordinary.unit as u64]);

        let before = native.diagnostics.resident_entries;
        let mut state = resident_cpu(OUTSIDE, 1);
        assert_eq!(enter(&mut native, &mut memory, stop, &mut state).retired, 6);
        assert_eq!(native.diagnostics.resident_entries, before + 1);
        let outside = outside_link(&native, HEAD);
        let transition = target(&native, outside.point);
        assert_eq!(Some(transition), outside.direct);
        assert_eq!(target(&native, transition + 4), probe);
        // Branch, ADR, branch, settled sensor. The former trampoline
        // added one more branch before the same checked form.
        let outside_words = 1 + (transition + 8 - transition) / 4 + 1;
        assert!(outside_words <= 5);

        let edge = native.code.internal_edges[&HEAD]
            .iter()
            .find(|edge| edge.back)
            .unwrap();
        assert_eq!(target(&native, edge.point), edge.transition);
        assert_eq!(target(&native, edge.link), probe);
        let publication_words = (edge.link - edge.transition) / 4;
        let back_words = 1 + publication_words + 1 + 1;
        assert!(back_words <= 1 + publication_words + 1 + 2);

        let version = &native.code.resident(HEAD).unwrap().versions[0];
        let (key, proved) = (version.key, version.proved.unwrap());
        let point = outside_link(&native, HEAD).point;
        native.code.point_facts.insert(point, key);
        native
            .code
            .repoint_resident_edges(HEAD, Some(probe), false, false);
        native.code.sync();
        assert_eq!(target(&native, transition + 4), proved);
        let offer_words = native
            .plans
            .job(HEAD)
            .map_or(0, |job| if job.splits { 5 } else { 8 });
        assert!(target(&native, proved + (offer_words + 3) * 4) > checked);
        let mut state = resident_cpu(OUTSIDE, 1);
        assert_eq!(enter(&mut native, &mut memory, stop, &mut state).retired, 6);

        let stubs = native.code.stubs;
        assert_eq!((stubs.lookup_slow - stubs.lookup) / 4, 8);
        assert_eq!((stubs.lookup - stubs.dispatch) / 4 - 2, 19);
        assert_eq!(
            native.code.cache.word(stubs.lookup + 7 * 4),
            crate::encode::br(crate::encode::Reg::new(17))
        );
        assert_eq!(
            native.code.cache.word(stubs.dispatch + 18 * 4),
            crate::encode::br(crate::encode::Reg::new(0))
        );
        assert_eq!(
            native.code.cache.word(native.code.enter.0 + 37 * 4),
            crate::encode::br(crate::encode::Reg::new(17))
        );
    }

    // spec: A64-OPT-9
    #[test]
    fn resident_discovery_builds_a_pointer_loop_without_a_proof() {
        let (mut memory, mut native, stop) = resident_loop(&[0x8b, 0x36, 0x49, 0x75, 0xfb, 0xcc]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        memory.write(DATA, &DATA.to_le_bytes()).unwrap();
        let mut state = resident_cpu(OUTSIDE, plan::HOT + 1);
        state.regs[6] = DATA;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 2 + 3 * (plan::HOT + 1)));
        assert!(!native.worker_jobs.contains_key(&HEAD));
        let head = native.code.resident(HEAD).unwrap();
        let edge = &native.code.internal_edges[&HEAD][0];
        assert_eq!(head.versions.len(), 1);
        assert_eq!(
            native.code.cache.word(edge.point),
            store::branch(edge.point, edge.transition)
        );
        assert_eq!(
            native.code.cache.word(edge.link),
            store::branch(edge.link, head.entry().unwrap())
        );
        let outside = outside_link(&native, HEAD);
        let transition = outside.direct.unwrap();
        assert_eq!(
            native.code.cache.word(outside.point),
            store::branch(outside.point, transition)
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
        let mut state = resident_cpu(HEAD, plan::HOT);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 1 + 12 * plan::HOT));
        assert_eq!(native.plans.counter_at(head).unwrap().get(), 1);
        assert!(!native.plans.resident(head).is_some());

        let mut state = resident_cpu(HEAD, 2);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 25));
        assert!(native.code.tier1(head).is_none());
        let edges = &native.code.internal_edges[&head];
        assert!(edges.iter().any(|edge| !edge.back));
        for edge in edges {
            assert_eq!(
                native.code.cache.word(edge.point),
                store::branch(edge.point, edge.transition)
            );
        }
        assert_eq!(
            native.code.resident(head).unwrap().versions[0].at.regs[1],
            1
        );

        let mut state = resident_cpu(HEAD, 3);
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 37));
        assert_eq!(
            native.code.resident(head).unwrap().versions[0]
                .slot
                .entries
                .get(),
            1
        );
        assert!(native.code.tier1(head).is_none());

        let mut state = resident_cpu(HEAD, 2);
        state.mxcsr ^= 0x2000;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 25));
        assert!(native.code.tier1(head).is_some());

        memory.protect(HEAD, 0x4000, READ | WRITE).unwrap();
        memory.write(head, &[0xcc]).unwrap();
        native.invalidate(&mut memory);
        assert!(!native.plans.resident(head).is_some());
        for edge in &native.code.internal_edges[&head] {
            assert_eq!(
                native.code.cache.word(edge.point),
                store::branch(edge.point, edge.target)
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
        assert!(run.retired > 3 * plan::HOT);
        assert!(native.plans.counter_at(HEAD).is_none());
        assert!(!native.plans.resident(HEAD).is_some());
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
        assert!(
            native.code.resident(HEAD).unwrap().versions[0]
                .plain_sites
                .is_empty()
        );
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
        let head = native.code.resident(HEAD).unwrap();
        assert_eq!(head.sample.get(), plan::SAMPLE - 1);
        assert_eq!(head.versions[0].slot.retry.get(), plan::MISSES);
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
            &mut resident_cpu(OUTSIDE, plan::HOT + 1),
        );
        for mode in 1..=2 {
            let mut state = resident_cpu(OUTSIDE, 10);
            state.mxcsr = 0x1f80 | mode << 13;
            for _ in 0..4 {
                enter(&mut native, &mut memory, stop, &mut state);
                if native.code.resident(HEAD).unwrap().versions.len() == mode as usize + 1 {
                    break;
                }
                state.eip = OUTSIDE;
            }
            assert_eq!(
                native.code.resident(HEAD).unwrap().versions.len(),
                mode as usize + 1
            );
            let versions = &native.code.resident(HEAD).unwrap().versions;
            let previous = &versions[mode as usize - 1];
            assert_eq!(
                native.code.cache.word(previous.next),
                store::branch(previous.next, versions[mode as usize].entry)
            );
            assert_eq!(
                native.code.cache.word(previous.miss),
                store::branch(previous.miss, versions[mode as usize].entry)
            );
        }
        let resident = native.code.resident(HEAD).unwrap();
        let before: Vec<_> = resident
            .versions
            .iter()
            .map(|v| v.slot.retry.get())
            .collect();
        resident.sample.set(8);
        let mut state = resident_cpu(OUTSIDE, 2);
        state.mxcsr = 0x7f80;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, run.retired), (stop, 10));
        let resident = native.code.resident(HEAD).unwrap();
        assert_eq!(resident.versions.len(), plan::VERSIONS);
        assert!(
            resident
                .versions
                .iter()
                .zip(before)
                .all(|(v, before)| v.slot.retry.get() == before)
        );
        let last = resident.versions.last().unwrap();
        assert_eq!(
            native.code.cache.word(last.next),
            store::branch(last.next, native.code.resident_fallback(HEAD).unwrap())
        );

        for _ in 0..2 * plan::SAMPLE {
            let mut state = resident_cpu(OUTSIDE, 10);
            state.mxcsr = 0x5f80;
            enter(&mut native, &mut memory, stop, &mut state);
        }
        let resident = native.code.resident(HEAD).unwrap();
        let transition = outside_link(&native, HEAD).direct.unwrap();
        let patch = transition + 4;
        assert_eq!(
            native.code.cache.word(patch),
            store::branch(patch, resident.versions[2].entry)
        );
    }

    // spec: A64A-FALL-3 A64-OPT-8
    #[test]
    fn a_second_resident_version_needs_no_link_sample_word() {
        let (mut memory, mut native, stop) = resident_loop(&[0x8b, 0x36, 0x49, 0x75, 0xfb, 0xcc]);
        native.code.version_stats = Some(FxHashMap::default());
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        memory.write(DATA, &DATA.to_le_bytes()).unwrap();
        assert!(native.force_resident(&mut memory, HEAD));
        assert!(native.code.offer(HEAD).is_none());

        for _ in 0..plan::MISSES {
            let mut state = resident_cpu(OUTSIDE, 2);
            state.regs[6] = DATA;
            state.mxcsr ^= 0x2000;
            let run = enter(&mut native, &mut memory, stop, &mut state);
            assert_eq!((state.eip, state.regs[6], run.retired), (stop, DATA, 8));
        }
        assert_eq!(native.code.resident(HEAD).unwrap().versions.len(), 2);
        let mut state = resident_cpu(OUTSIDE, 2);
        state.regs[6] = DATA;
        state.mxcsr ^= 0x2000;
        let run = enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!((state.eip, state.regs[6], run.retired), (stop, DATA, 8));
        let misses = &native.code.version_stats.as_ref().unwrap()[&HEAD].resident_misses;
        assert!(misses[0].get() > 0);
        assert!(misses[1].get() >= u64::from(plan::MISSES));
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
            Exit::Event(crate::runtime::Event::Replay)
        );
        let old = native.code.tier1(HEAD).unwrap().unit;
        for _ in 1..CHECK_AFTER {
            assert_eq!(
                enter(&mut native, &mut memory, stop, &mut state).exit,
                Exit::Event(crate::runtime::Event::Replay)
            );
        }
        let new = native.code.tier1(HEAD).unwrap().unit;
        assert_ne!(new, old);
        assert!(native.plans.checked(HEAD));
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
            native
                .translate(
                    &mut memory,
                    entry,
                    crate::facts::Key::read(&CpuState::default(), crate::facts::ALL),
                )
                .unwrap();
            native.discover(&mut memory, entry, entry, resident_cpu(entry, 4));
        }
        memory.protect(stale, 0x4000, READ | WRITE).unwrap();
        memory.write(stale, &[0x90]).unwrap();
        native.invalidate(&mut memory);
        native.install_optimized(true);
        for eip in [HEAD, other] {
            assert_eq!(native.code.resident(eip).unwrap().versions.len(), 1);
        }
        assert!(!native.plans.resident(stale).is_some());
        assert!(!native.worker_jobs.contains_key(&stale));
    }

    // spec: A64-OPT-8, A64-OPT-9
    #[test]
    fn a_plain_page_miss_rebuilds_without_the_fact() {
        // mov eax,[esi]; add ebx,eax; dec ecx; jne HEAD; int3
        let (mut memory, mut native, stop) =
            resident_loop(&[0x8b, 0x06, 0x01, 0xc3, 0x49, 0x75, 0xf9, 0xcc]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        native.plans.mark_checked(HEAD);
        assert!(native.force_resident(&mut memory, HEAD));
        let first = &native.code.resident(HEAD).unwrap().versions[0];
        assert!(first.plain_sites.contains(&HEAD));
        let old = first.entry;

        memory.unmap(DATA, 0x4000).unwrap();
        memory.map_shared_audio(DATA, 0x1000).unwrap();
        let mut state = resident_cpu(OUTSIDE, 2);
        state.regs[6] = DATA;
        enter(&mut native, &mut memory, stop, &mut state);
        assert_eq!(
            native.code.resident(HEAD).unwrap().versions[0].slot.env[0].get(),
            2
        );
        for _ in 1..plan::MISSES.div_ceil(2) {
            let mut state = resident_cpu(OUTSIDE, 2);
            state.regs[6] = DATA;
            enter(&mut native, &mut memory, stop, &mut state);
        }
        let resident = native.plans.resident(HEAD).unwrap();
        assert!(resident.refused.contains(&HEAD));
        assert_ne!(native.code.resident(HEAD).unwrap().versions[0].entry, old);
        assert!(
            !native.code.resident(HEAD).unwrap().versions[0]
                .plain_sites
                .contains(&HEAD)
        );
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
        let first = &native.code.resident(HEAD).unwrap().versions[0];
        assert!(first.plain_sites.contains(&u32::MAX));
        let old = first.entry;
        state.eip = OUTSIDE;
        native.compiler = compile::Compiler::background();

        for _ in 0..2 {
            enter(&mut native, &mut memory, stop, &mut state.clone());
        }
        native.install_optimized(true);
        let resident = native.plans.resident(HEAD).unwrap();
        let site = (HEAD + 4, ResidentCold::FrameDisjoint);
        assert_eq!(resident.cold_counts[&site], 2);
        assert_eq!(native.diagnostics.resident_refusals_by_reason[&site.1], 1);
        assert_ne!(native.code.resident(HEAD).unwrap().versions[0].entry, old);
        assert!(
            !native.code.resident(HEAD).unwrap().versions[0]
                .plain_sites
                .contains(&u32::MAX)
        );

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
    fn unchecked_host_faults_rebuild_one_stream_as_checked() {
        let (mut memory, mut native, stop) = resident_loop(&[
            0x8b, 0x06, 0x8b, 0x56, 4, 0x83, 0xc6, 8, 0x89, 0x07, 0x83, 0xc7, 4, 0x49, 0x75, 0xf0,
            0xcc,
        ]);
        memory.map(DATA, 0x4000, READ | WRITE).unwrap();
        memory.map_shared_audio(DATA + 0x4000, 0x1000).unwrap();
        assert!(native.force_resident(&mut memory, HEAD));
        let old = native.code.resident(HEAD).unwrap().versions[0].entry;
        let mut state = resident_cpu(OUTSIDE, 3);
        state.regs[6] = DATA + 0x4000 - 4;
        enter(&mut native, &mut memory, stop, &mut state);
        let resident = native.plans.resident(HEAD).unwrap();
        assert_eq!(
            resident.cold_counts[&(HEAD + 2, ResidentCold::HostFault)],
            1
        );
        assert_eq!(native.code.resident(HEAD).unwrap().versions[0].entry, old);
        for expected in 1..=plan::CHECKED_FAULTS {
            let mut state = resident_cpu(OUTSIDE, 3);
            state.regs[6] = DATA + 0x4000;
            enter(&mut native, &mut memory, stop, &mut state);
            assert_eq!(native.diagnostics.resident_cold_exits, expected + 1);
            assert_eq!(state.eip, HEAD);
        }
        let resident = native.plans.resident(HEAD).unwrap();
        assert_eq!(resident.checked.len(), 2);
        assert!(resident.checked.contains(&HEAD) && resident.checked.contains(&(HEAD + 2)));
        assert_ne!(native.code.resident(HEAD).unwrap().versions[0].entry, old);
        let checked_entry = native.code.resident(HEAD).unwrap().versions[0].entry;

        for _ in 0..100 {
            let mut state = resident_cpu(OUTSIDE, 3);
            state.regs[6] = DATA + 0x4000 - 4;
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
        }
        assert_eq!(
            native.diagnostics.resident_cold_exits,
            plan::CHECKED_FAULTS + 1
        );
        assert_eq!(
            native.code.resident(HEAD).unwrap().versions[0].entry,
            checked_entry
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
        let old = native.code.resident(HEAD).unwrap().versions[0].entry;
        state.eip = OUTSIDE;
        for _ in 0..2 {
            enter(&mut native, &mut memory, stop, &mut state.clone());
        }
        let head = native.plans.resident(HEAD).unwrap();
        let cold = 2;
        assert_eq!(head.cold_counts.get(&(site, reason)), Some(&cold));
        assert!(head.cuts.contains(&site));
        assert_eq!(native.diagnostics.resident_cuts_by_reason[&reason], 1);
        assert_ne!(native.code.resident(HEAD).unwrap().versions[0].entry, old);

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
        // movzx eax,byte [esi]; mov [esi],eax
        repeated_optional_cold_site(
            (&[0x0f, 0xb6, 0x06, 0x89, 0x06], 3),
            ResidentCold::PageCross,
            false,
            |memory, state| {
                memory.map(DATA, 0x4000, READ | WRITE).unwrap();
                memory.protect(DATA + 0x1000, 0x3000, READ).unwrap();
                state.regs[6] = DATA + 0xfff;
            },
        );
    }

    // spec: A64-OPT-9, A64-OPT-14
    #[test]
    fn rare_page_cross_exits_keep_the_site_resident() {
        // movzx eax,byte [esi]; mov [esi],eax; dec ecx; jne HEAD; int3
        let (mut memory, mut native, stop) =
            resident_loop(&[0x0f, 0xb6, 0x06, 0x89, 0x06, 0x49, 0x75, 0xf8, 0xcc]);
        memory.map(DATA, 0x8000, READ | WRITE).unwrap();
        memory.protect(DATA + 0x4000, 0x4000, READ).unwrap();
        assert!(native.force_resident(&mut memory, HEAD));
        let entry = native.code.resident(HEAD).unwrap().versions[0].entry;
        let mut crossing = resident_cpu(OUTSIDE, 4);
        crossing.regs[6] = DATA + 0x3fff;
        enter(&mut native, &mut memory, stop, &mut crossing.clone());

        // One admitted invocation runs hundreds of iterations without crossing.
        let mut plain = resident_cpu(OUTSIDE, 512);
        plain.regs[6] = DATA;
        while plain.eip != stop {
            enter(&mut native, &mut memory, stop, &mut plain);
        }
        enter(&mut native, &mut memory, stop, &mut crossing);
        let head = native.plans.resident(HEAD).unwrap();
        assert_eq!(head.cold_counts[&(HEAD + 3, ResidentCold::PageCross)], 2);
        assert!(head.cuts.is_empty() && !head.declined());
        assert_eq!(native.code.resident(HEAD).unwrap().versions[0].entry, entry);
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
        for _ in 0..2 {
            enter(&mut native, &mut memory, stop, &mut state.clone());
        }
        assert!(native.plans.resident(HEAD).unwrap().declined());
        assert_eq!(
            native.diagnostics.resident_demotions_by_reason[&ResidentCold::BlockedAccess],
            1
        );
        for _ in 0..100 {
            let mut state = state.clone();
            assert_eq!(
                enter(&mut native, &mut memory, stop, &mut state).exit,
                Exit::Event(crate::runtime::Event::Replay)
            );
            assert_eq!(state.eip, HEAD);
        }
        assert_eq!(native.diagnostics.resident_cold_exits, 2);
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
        native
            .translate(
                &mut memory,
                HEAD,
                crate::facts::Key::read(&CpuState::default(), crate::facts::ALL),
            )
            .unwrap();
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
        let slot = &native.code.resident(HEAD).unwrap().versions[0].slot;
        assert_eq!(slot.entries.get(), 1);
        // Only the two instructions leading into the loop retire in Tier 1.
        assert_eq!(u64::from(run.retired) - slot.countdown[1].get(), 2);

        for _ in 1..300 {
            let mut state = resident_cpu(OUTSIDE, 2);
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
        }
        assert!(native.plans.resident(HEAD).unwrap().declined());
        assert_eq!(native.diagnostics.global_invalidations, invalidations);
        let outside = outside_link(&native, HEAD);
        assert_eq!(
            native.code.cache.word(outside.point),
            outside.branch(native.code.offer(HEAD).unwrap().0)
        );
        let fallback = native.code.offer(HEAD).unwrap().1;
        assert_eq!(
            native.code.cache.word(fallback),
            store::branch(fallback, native.code.tier1(HEAD).unwrap().unit)
        );
        let edge = &native.code.internal_edges[&HEAD][0];
        assert_eq!(
            native.code.cache.word(edge.point),
            store::branch(edge.point, edge.target)
        );
    }

    // spec: A64A-ROLE-8, A64A-PLACE-3, A64-OPT-8
    #[test]
    fn resident_probation_keeps_long_invocations() {
        let (mut memory, mut native, stop) = probation_loop(100);
        let invalidations = native.diagnostics.global_invalidations;
        let version = &native.code.resident(HEAD).unwrap().versions[0];
        assert!(version.probation);
        let patches = version.accounting.clone();
        let sensors = version.probation_sensors.clone();
        for _ in 0..300 {
            let mut state = resident_cpu(OUTSIDE, 100);
            while state.eip != stop {
                enter(&mut native, &mut memory, stop, &mut state);
            }
        }
        assert!(!native.plans.resident(HEAD).unwrap().declined());
        assert_eq!(native.diagnostics.global_invalidations, invalidations);
        assert!(
            patches
                .into_iter()
                .all(|(point, _)| native.code.cache.word(point) == crate::encode::NOP)
        );
        assert!(
            sensors
                .into_iter()
                .all(|point| native.code.cache.word(point)
                    == crate::encode::b((runtime::SENSOR_BYTES + 4) as i32))
        );
    }

    // spec: A64A-ROLE-8, A64A-PLACE-3
    #[test]
    fn cold_recoveries_finish_probation_without_a_normal_completion() {
        let (_memory, mut native, _) = probation_loop(2);
        let head = native.plans.resident_mut(HEAD).unwrap();
        head.checked.insert(HEAD);
        assert!(head.probation[0].is_some());
        for _ in 0..plan::PROBATION {
            native.resident_cold_exit((HEAD, 0), HEAD, ResidentCold::HostFault, false, 0, 0);
        }
        assert!(native.plans.resident(HEAD).unwrap().declined());
    }

    // spec: A64A-ROLE-8, A64-CTL-1, A64-OPT-8
    #[test]
    fn cache_clear_discards_pending_resident_events() {
        let (mut memory, mut native, _stop) = probation_loop(100);
        let sample = BackEdgeSample {
            generation: native.code.generation,
            source: HEAD,
            unit: native.code.tier1(HEAD).unwrap().unit,
            at: resident_cpu(HEAD, 100),
        };
        // Hold an optimized completion across the clear as well as the
        // deferred discovery. The next Tier-1 placement can reuse its address.
        native.rebuild_resident_versions(HEAD);
        native.clear();
        native
            .translate(&mut memory, HEAD, crate::facts::Key::default())
            .unwrap();
        assert_eq!(native.code.tier1(HEAD).unwrap().unit, sample.unit);
        assert!(native.code.resident(HEAD).is_none());
        assert!(!native.worker_jobs.contains_key(&HEAD));
        native.install_optimized(true);
        native.apply_back_edge_sample(&mut memory, sample);
        assert!(native.plans.resident(HEAD).is_none());
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
            assert_eq!(
                (run.exit, run.retired),
                (Exit::Event(crate::runtime::Event::Replay), 0),
                "{eip:#x}"
            );
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
                overflow: crate::translate::lower::flags::Overflow::Host,
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
