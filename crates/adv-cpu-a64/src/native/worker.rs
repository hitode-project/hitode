//! Execution under an owner-held code and memory lease. Only the owner
//! evaluates ranges and accepts completions.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use adv_cpu_core::CpuState;
use adv_cpu_core::backend::{Completion, Computation, Pace};
use adv_sync::WakeEvent;
use rustc_hash::FxHashMap;

use super::{
    ACTIVE, Active, Enter, Exit, FaultCapture, Memory, Runtime, Site, alias_base, record_branch,
};
use crate::executor::ExecutionContext;
use crate::runtime::COMPUTATION_SLICE;
use crate::translate::computation::{Evaluation, Plan, Proof, Range};
use crate::translate::opt::front::Assumed;
use crate::translate::opt::ir::Source;
use crate::translate::opt::recovery::{self, FlagRecipe, Host, Record};

/// A helper's wake latency until helpers have shown theirs: it starts
/// claiming this long after the owner.
const WAKE_NS: f64 = 50_000.0;
/// Blocks per thread, so the last block to finish leaves the others idle
/// for about a quarter of a thread's part. A block is at most one slice.
const BLOCKS_PER_THREAD: f64 = 4.0;
/// The owner's fixed work in a split besides its blocks: admission and merge.
const ROUND_NS: f64 = 10_000.0;
/// Entries in a row the optimized form must miss before another is built
/// for the state at the next, and how many more may be built.
const MISSES: u32 = 4;
const RESPECIALIZE: u8 = 2;
/// A miss limit no count reaches: a sign test of `misses - limit` stays
/// negative.
const NO_LIMIT: u32 = i32::MAX as u32;
/// MXCSR exception flags, sticky across a loop's blocks.
const MXCSR_FLAGS: u32 = 0x3f;

pub(super) struct Prepared {
    pub code: Code,
    pub proof: Proof,
    /// What the optimized form is built from, again when entry states
    /// keep missing its facts.
    pub trace: Arc<crate::translate::Trace>,
    /// Counters the head entry block updates in place (A64-OPT-8).
    pub slot: Box<Slot>,
    /// The inline form the head entry block enters, once installed.
    pub inline: Option<Inline>,
    /// Optimized forms requested, and whether one is being built.
    forms: std::cell::Cell<u8>,
    building: std::cell::Cell<bool>,
    skip: std::cell::Cell<u32>,
    mode: std::cell::Cell<Mode>,
    /// Instructions and time per outer iteration, once a job showed them.
    per_iteration: std::cell::Cell<Option<(u64, f64)>>,
    /// Serial ns of the last job that showed them.
    cost: std::cell::Cell<u64>,
}

/// Owner arrival counters shared by Rust and the head entry block, which
/// reads them through a fixed heap address.
#[repr(C)]
#[derive(Default)]
pub(super) struct Slot {
    /// Consecutive entries the optimized form did not admit, and the count
    /// at which an entry goes to Rust to build another.
    pub misses: std::cell::Cell<u32>,
    pub miss_limit: std::cell::Cell<u32>,
    /// Owner arrivals, and the one that nominates a shadow caller.
    pub runs: std::cell::Cell<u32>,
    pub nominate: std::cell::Cell<u32>,
    /// The serial cost of a split loop in host ticks, or 0 when it never
    /// splits.
    pub split_ticks: std::cell::Cell<u64>,
}

impl Slot {
    pub const MISSES: u32 = std::mem::offset_of!(Slot, misses) as u32;
    pub const RUNS: u32 = std::mem::offset_of!(Slot, runs) as u32;
    pub const SPLIT_TICKS: u32 = std::mem::offset_of!(Slot, split_ticks) as u32;
}

/// The installed inline form of a computation (A64-OPT-8).
pub(super) struct Inline {
    /// The word every link to the head branches to: a branch to the
    /// current entry block.
    pub trampoline: usize,
    pub records: Vec<Record<Host>>,
    pub flags: Vec<Option<FlagRecipe>>,
    pub assumed_mmx: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Whole(u8),
    Trial,
    Iteration,
    WholeOnly,
}

impl Prepared {
    /// `iterates` when the code can stop after one outer iteration.
    pub fn new(code: Code, iterates: bool, proof: Proof, trace: crate::translate::Trace) -> Self {
        let mode = if iterates {
            Mode::Whole(0)
        } else {
            Mode::WholeOnly
        };
        Self {
            code,
            proof,
            trace: Arc::new(trace),
            slot: Box::new(Slot {
                miss_limit: NO_LIMIT.into(),
                nominate: 8.into(),
                ..Slot::default()
            }),
            inline: None,
            forms: std::cell::Cell::new(0),
            building: std::cell::Cell::new(false),
            skip: std::cell::Cell::new(0),
            mode: std::cell::Cell::new(mode),
            per_iteration: std::cell::Cell::new(None),
            cost: std::cell::Cell::new(0),
        }
    }

    /// Counts an entry whose state the optimized form did or did not admit.
    pub fn admitted(&self, admitted: bool) {
        let misses = &self.slot.misses;
        misses.set(if admitted {
            0
        } else {
            (misses.get() + 1).min(NO_LIMIT)
        });
    }

    /// Whether to build a form for the state at this entry: the first one,
    /// or up to `RESPECIALIZE` more after `MISSES` entries it did not admit.
    pub fn wants_form(&self) -> bool {
        !self.building.get()
            && match self.forms.get() {
                0 => true,
                forms => forms <= RESPECIALIZE && self.slot.misses.get() >= MISSES,
            }
    }

    pub fn requested(&self) {
        self.forms.set(self.forms.get() + 1);
        self.building.set(true);
        self.slot.misses.set(0);
        self.limit_misses();
    }

    pub fn built(&self) {
        self.building.set(false);
        self.limit_misses();
    }

    /// Entry blocks send the miss that `wants_form` would act on to Rust.
    fn limit_misses(&self) {
        let respecializes = !self.building.get() && (1..=RESPECIALIZE).contains(&self.forms.get());
        self.slot
            .miss_limit
            .set(if respecializes { MISSES } else { NO_LIMIT });
    }

    /// The code and whether a job stops after one outer iteration.
    fn selected(&self) -> (&Code, bool) {
        (
            &self.code,
            matches!(self.mode.get(), Mode::Trial | Mode::Iteration),
        )
    }

    /// The serial ns a job of this computation last took, 0 until one ran.
    pub fn cost(&self) -> u64 {
        self.cost.get()
    }

    pub fn worth_offering(&self) -> bool {
        let skip = self.skip.get();
        if skip == 0 {
            true
        } else {
            self.skip.set(skip - 1);
            false
        }
    }

    /// Counts an owner arrival that runs in place. Returns whether it is
    /// the 8th, the 64th or a multiple of 4096, which nominate a caller.
    pub fn record_owner_run(&self) -> bool {
        let slot = &self.slot;
        let n = slot.runs.get().wrapping_add(1);
        slot.runs.set(n);
        let due = n == slot.nominate.get();
        if due {
            slot.nominate.set(match n {
                ..8 => 8,
                8..64 => 64,
                _ => (n | 4095).wrapping_add(1),
            });
        }
        due
    }

    fn set_cost(&self, ns: u64) {
        self.cost.set(ns);
        let ticks = if self.proof.splits() && ns > 0 {
            let hz = u128::from(adv_cpu_core::backend::host_tick_hz());
            (u128::from(ns) * hz)
                .div_ceil(1_000_000_000)
                .min(u128::from(u64::MAX)) as u64
        } else {
            0
        };
        self.slot.split_ticks.set(ticks);
    }

    fn observe(&self, entry: u32, iteration: bool, done: &Done) {
        let completion = &done.completion;
        if iteration {
            if self.mode.get() == Mode::Trial
                && !completion.requires_owner
                && completion.context.as_ref().unwrap().cpu().eip == entry
            {
                self.mode
                    .set(if completion.elapsed >= Duration::from_micros(50) {
                        Mode::Iteration
                    } else {
                        Mode::WholeOnly
                    });
            }
            return;
        }
        if let Mode::Whole(recalls) = self.mode.get()
            && done.deadline_recall
        {
            self.mode.set(if recalls >= 1 {
                self.skip.set(0);
                Mode::Trial
            } else {
                Mode::Whole(recalls + 1)
            });
        }
        if completion.requires_owner || matches!(self.mode.get(), Mode::Trial | Mode::Iteration) {
            return;
        }
        // Short jobs do not amortize admission and queueing. Give them
        // another trial after about one ordinary slice.
        let skip = if completion.elapsed < Duration::from_micros(50) {
            (u64::from(COMPUTATION_SLICE) / completion.retired.max(1)).max(1) as u32
        } else {
            0
        };
        self.skip.set(skip);
    }
}

#[derive(Clone)]
pub(super) struct Code {
    pub entry: usize,
    pub end: usize,
    pub sites: Arc<[(usize, Site)]>,
    /// The optimized form, run instead when a job meets its assumptions.
    pub optimized: Option<Arc<Optimized>>,
}

/// An optimized form of the same unit (A64-OPT-1).
pub(super) struct Optimized {
    pub code: Code,
    pub entry: Vec<Source>,
    pub assumed: Assumed,
    pub records: Vec<Record<Host>>,
    pub flags: Vec<Option<FlagRecipe>>,
    pub cells: usize,
}

pub(super) struct Faults<'a> {
    pub code: &'a Code,
    pub stub: usize,
    pub capture: &'a FaultCapture,
}

#[derive(Default)]
pub(super) struct ExecuteScratch {
    pub params: Vec<u128>,
    pub saved: Vec<u128>,
    pub capture: FaultCapture,
}

#[derive(Clone, Copy)]
pub(super) struct Entry {
    pub enter: Enter,
    pub opt_enter: Enter,
    pub window: u64,
    pub alias: u64,
    pub fault: usize,
}

struct Task {
    id: u32,
    context: ExecutionContext,
    entry: Entry,
    code: Code,
    once: bool,
    signals: Signals,
    done: SyncSender<Done>,
    wake: Option<WakeEvent>,
}

struct Done {
    completion: Completion<Option<ExecutionContext>>,
    deadline_recall: bool,
    /// Whether the optimized form admitted the entry state, if there is one.
    admitted: Option<bool>,
}

const RECALL: u32 = 1;
const OWNER_NEEDED: u32 = 2;
/// The owner's own job also stops for its ordinary work.
const LOCAL: u32 = RECALL | OWNER_NEEDED;

pub(super) struct Notifications {
    pub pending: (Arc<AtomicU32>, u32),
    pub wake: Option<WakeEvent>,
}

#[derive(Clone)]
struct Signals {
    recall: Arc<AtomicU32>,
    pending: (Arc<AtomicU32>, u32),
}

impl Signals {
    fn requested(&self) -> bool {
        self.recall.load(Ordering::Acquire) != 0
            || self.pending.0.load(Ordering::Acquire) & self.pending.1 != 0
    }
}

fn run(
    runtime: &mut Runtime,
    id: u32,
    mut context: ExecutionContext,
    entry: Entry,
    (code, head_exit): (&Code, u32),
    (signals, recall_mask): (&Signals, u32),
    scratch: &mut ExecuteScratch,
) -> Done {
    runtime.countdown = u64::from(COMPUTATION_SLICE);
    runtime.recall = signals.recall.as_ptr() as u64;
    runtime.recall_mask = recall_mask;
    runtime.computation_pending = signals.pending.0.as_ptr() as u64;
    runtime.computation_pending_mask = signals.pending.1;
    runtime.head_exit = head_exit;
    runtime.deferred = 0;
    let started = Instant::now();
    let cpu = context.cpu_mut();
    cpu.settle_flags();
    if code.optimized.as_deref().is_none_or(|o| o.assumed.mmx) || cpu.mmx_mode != 0 {
        cpu.refresh_mmx();
    }
    let optimized = admit(code, cpu, entry.alias, &mut scratch.params);
    let (exit, retired) = execute(runtime, cpu, entry, code, optimized, scratch, true);
    let elapsed = started.elapsed();
    record_branch(cpu, runtime.branch, cpu.eip);
    context.finish_computation(exit);
    Done {
        admitted: code.optimized.as_ref().map(|_| optimized.is_some()),
        deadline_recall: (exit == Exit::Recall || runtime.deferred != 0)
            && signals.pending.0.load(Ordering::Acquire)
                & signals.pending.1
                & adv_sync::pending::DEADLINE
                != 0,
        completion: Completion {
            id,
            context: Some(context),
            retired,
            optimized: if optimized.is_some() { retired } else { 0 },
            elapsed,
            helpers: Duration::ZERO,
            requires_owner: exit != Exit::Dispatch || runtime.branch != 0,
        },
    }
}

/// The optimized form of `code` if it has one whose entry facts hold at
/// `cpu`, filling `params`. `alias` reaches the admitted frame.
pub(super) fn admit<'a>(
    code: &'a Code,
    cpu: &CpuState,
    alias: u64,
    params: &mut Vec<u128>,
) -> Option<&'a Optimized> {
    let o = code.optimized.as_deref()?;
    // SAFETY: admission granted the frame's ranges.
    crate::translate::opt::admit_into(&o.entry, &o.assumed, cpu, params, |addr, width| unsafe {
        let mut bytes = [0; 8];
        let at = alias.wrapping_add(u64::from(addr)) as *const u8;
        std::ptr::copy_nonoverlapping(at, bytes.as_mut_ptr(), usize::from(width));
        u64::from_le_bytes(bytes)
    })
    .then_some(o)
}

/// Enters `code`, or `optimized`, its admitted form, on this thread. The
/// caller sets the countdown, recall, head exit and limit fields of
/// `runtime`, and holds the admitted ranges unshared. Returns the exit,
/// with an optimized one resolved to `Recall` or `Dispatch`, and the
/// instructions retired.
pub(super) fn execute(
    runtime: &mut Runtime,
    cpu: &mut CpuState,
    entry: Entry,
    code: &Code,
    optimized: Option<&Optimized>,
    scratch: &mut ExecuteScratch,
    sync_icache: bool,
) -> (Exit, u64) {
    let host = |addr: u32| entry.alias.wrapping_add(u64::from(addr)) as *mut u8;
    let entry_esp = cpu.regs[ESP];
    let code = optimized.map_or(code, |o| &o.code);
    if let Some(o) = optimized {
        if scratch.saved.len() < o.cells {
            scratch.saved.resize(o.cells, 0);
        }
        runtime.opt_params = scratch.params.as_ptr() as u64;
        runtime.opt_save = scratch.saved.as_mut_ptr() as u64;
        runtime.opt_cells = o.cells as u64;
    }
    scratch.capture.pc.set(0);
    let faults = Faults {
        code,
        stub: entry.fault,
        capture: &scratch.capture,
    };
    let countdown = runtime.countdown;
    runtime.cpu = cpu;
    runtime.window = entry.window;
    runtime.shadow = entry.alias;
    runtime.completed = 0;
    runtime.opt_recalled = 0;
    runtime.branch = 0;
    runtime.fault_simd = faults.capture.simd.as_ptr() as u64;
    let previous = ACTIVE.replace(Active::Worker(std::ptr::from_ref(&faults).cast()));
    // SAFETY: the caller retains this code and mapping through the call.
    // The proved form touches only its admitted ranges and this thread's state.
    unsafe {
        if sync_icache {
            // Instruction-cache maintenance is done by the publishing owner.
            // Its ISB does not synchronize a helper thread's instruction stream.
            std::arch::asm!("isb", options(nostack, preserves_flags));
        }
        let target_enter = if optimized.is_some() {
            entry.opt_enter
        } else {
            entry.enter
        };
        target_enter(runtime, code.entry);
    }
    ACTIVE.set(previous);
    runtime.cpu = std::ptr::null_mut();
    let mut exit = Exit::from_raw(runtime.exit).expect("computation exit");
    // An optimized form may leave with a negative countdown.
    let mut retired = (runtime.completed + countdown).wrapping_sub(runtime.countdown);
    if exit == Exit::Optimized {
        let o = optimized.expect("optimized entry");
        let record = &o.records[runtime.opt_record as usize];
        // SAFETY: as above, the frame lies in the leased ranges.
        let refund = recovery::materialize(
            record,
            &runtime.opt_dump,
            &scratch.saved,
            &o.flags,
            cpu,
            |offset, bytes| unsafe {
                let at = host(entry_esp.wrapping_add_signed(offset));
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
            },
        );
        retired = retired.wrapping_sub(u64::from(refund));
        // Every exit but a recall, failures included, continues where the
        // owner dispatches.
        exit = if runtime.opt_recalled != 0 {
            Exit::Recall
        } else {
            Exit::Dispatch
        };
    }
    if exit == Exit::Fault {
        let site = super::site_at(&code.sites, faults.capture.pc.get()).expect("faulting site");
        faults.capture.recover(cpu, site, runtime.temps);
        retired -= u64::from(site.refund);
    }
    if optimized.is_none_or(|o| o.assumed.mmx) || cpu.mmx_mode != 0 {
        cpu.refresh_mmx();
    }
    (exit, retired)
}

/// How split loops ran, for reports.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SplitCounts {
    pub splits: u64,
    pub blocks: u64,
    pub wakes: u64,
    /// Helpers that found no block left to claim.
    pub idle_wakes: u64,
    /// Total time from submitting blocks to each helper's first claim.
    pub claim_ns: u64,
}

enum Job {
    Whole(Box<Task>),
    Blocks(Arc<Blocks>, SyncSender<Drained>),
}

/// An admitted head loop split into blocks of consecutive iterations
/// (A64-TR-9). Threads claim blocks in order, so completed blocks are a
/// prefix. Only the last block runs past the loop. A loop that writes its
/// frame runs each thread's blocks on a private copy below ESP.
struct Blocks {
    next: AtomicU32,
    stop: std::sync::atomic::AtomicBool,
    /// Thread indices: the owner is 0.
    threads: AtomicU32,
    count: u32,
    size: u32,
    plan: Plan,
    start: ExecutionContext,
    entry: Entry,
    code: Code,
    signals: Signals,
    submitted: Instant,
}

#[derive(Default)]
struct Drained {
    retired: u64,
    optimized: u64,
    elapsed: Duration,
    sticky: u32,
    blocks: u32,
    /// From submission to its first claim.
    claimed_after: Duration,
    /// The last block's completion and its thread.
    last: Option<(Completion<Option<ExecutionContext>>, u32)>,
}

pub(super) const ESP: usize = 4;

/// The distance between private frames, in cache lines, which keeps each copy of the span
/// below every byte live at the real frame, including those between ESP and
/// a span that starts above it.
fn frame_stride((lo, hi): (i32, i32)) -> u32 {
    ((hi - lo.min(0)) as u32).next_multiple_of(64)
}

impl Blocks {
    /// The context at the head before outer iteration `first`.
    fn at(&self, first: u32) -> ExecutionContext {
        let mut context = self.start.clone();
        let cpu = context.cpu_mut();
        for (reg, affine) in cpu.regs.iter_mut().zip(self.plan.regs) {
            if let Some((start, step)) = affine {
                *reg = start.wrapping_add(step.wrapping_mul(first));
            }
        }
        context
    }

    fn host(&self, addr: u32) -> *mut u8 {
        self.entry.alias.wrapping_add(u64::from(addr)) as *mut u8
    }

    /// Stores the affine frame dwords before iteration `first` in the frame
    /// at `esp`.
    fn slots(&self, esp: u32, first: u32) {
        for &(offset, start, step) in self.plan.slots.iter().flatten() {
            let value = start.wrapping_add(step.wrapping_mul(first));
            let addr = esp.wrapping_add(offset as u32);
            // SAFETY: admission granted the frame span for writing.
            unsafe { self.host(addr).cast::<u32>().write_unaligned(value) };
        }
    }

    /// Copies the frame span from the frame at `from` to the frame at `to`.
    fn copy(&self, (lo, hi): (i32, i32), from: u32, to: u32) {
        let at = |esp: u32| self.host(esp.wrapping_add(lo as u32));
        // SAFETY: admission granted both spans, which are a stride apart.
        unsafe { std::ptr::copy_nonoverlapping(at(from), at(to), (hi - lo) as usize) };
    }

    /// The context before iteration `first` on thread `thread`'s frame.
    fn enter(&self, first: u32, thread: u32) -> ExecutionContext {
        let mut context = self.at(first);
        if let Some(frame) = self.plan.frame {
            let esp = self.start.cpu().regs[ESP];
            let private = esp.wrapping_sub((thread + 1) * frame_stride(frame));
            self.copy(frame, esp, private);
            self.slots(private, first);
            context.cpu_mut().regs[ESP] = private;
        }
        context
    }

    /// Moves the last block's frame and ESP back to the real frame, or
    /// stores the affine frame dwords of a prefix ending before `first`.
    fn settle(&self, cpu: &mut adv_cpu_core::CpuState, last: Option<u32>, first: u32) {
        let Some(frame) = self.plan.frame else {
            return;
        };
        let esp = self.start.cpu().regs[ESP];
        match last {
            Some(thread) => {
                let delta = (thread + 1) * frame_stride(frame);
                self.copy(frame, esp.wrapping_sub(delta), esp);
                cpu.regs[ESP] = cpu.regs[ESP].wrapping_add(delta);
            }
            None => self.slots(esp, first),
        }
    }

    fn drain(&self, runtime: &mut Runtime, scratch: &mut ExecuteScratch) -> Drained {
        let started = Instant::now();
        let thread = self.threads.fetch_add(1, Ordering::Relaxed);
        let mut out = Drained::default();
        // A block ignores recalls; a request only ends further claims.
        let quiet = Signals {
            recall: self.signals.recall.clone(),
            pending: (self.signals.pending.0.clone(), 0),
        };
        while !self.stop.load(Ordering::Acquire) {
            if self.signals.requested() {
                self.stop.store(true, Ordering::Release);
                break;
            }
            let index = self.next.fetch_add(1, Ordering::AcqRel);
            if index >= self.count {
                break;
            }
            if out.blocks == 0 {
                out.claimed_after = started - self.submitted;
            }
            out.blocks += 1;
            let last = index + 1 == self.count;
            let iterations = if last { 0 } else { self.size };
            let context = self.enter(index * self.size, thread);
            let done = run(
                runtime,
                0,
                context,
                self.entry,
                (&self.code, iterations),
                (&quiet, 0),
                scratch,
            )
            .completion;
            let cpu = done.context.as_ref().expect("block context").cpu();
            assert!(
                last || (!done.requires_owner && cpu.eip == self.start.cpu().eip),
                "a proved block left its iterations"
            );
            out.retired += done.retired;
            out.optimized += done.optimized;
            out.sticky |= cpu.mxcsr & MXCSR_FLAGS;
            if last {
                out.last = Some((done, thread));
            }
        }
        out.elapsed = started.elapsed();
        out
    }
}

struct Pool {
    send: SyncSender<Job>,
    active: Arc<AtomicUsize>,
    limit: usize,
}

impl Pool {
    fn get() -> Option<&'static Self> {
        static POOL: OnceLock<Option<Pool>> = OnceLock::new();
        POOL.get_or_init(Self::new).as_ref()
    }

    fn new() -> Option<Self> {
        let limit = std::thread::available_parallelism()
            .ok()?
            .get()
            .saturating_sub(1)
            .min(adv_cpu_core::PROCESSORS as usize - 1);
        if limit == 0 {
            return None;
        }
        let (send, recv) = sync_channel::<Job>(limit);
        let recv = Arc::new(Mutex::new(recv));
        let active = Arc::new(AtomicUsize::new(0));
        let mut started = 0;
        for _ in 0..limit {
            let recv = recv.clone();
            let active = active.clone();
            if std::thread::Builder::new()
                .name("guest computation".into())
                .spawn(move || {
                    // These jobs produce the same interactive frame as the owner.
                    #[cfg(target_os = "macos")]
                    unsafe {
                        libc::pthread_set_qos_class_self_np(
                            libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE,
                            0,
                        );
                    }
                    crate::cache::execution_thread();
                    let mut runtime = Runtime::new(std::ptr::null_mut(), 0);
                    let mut scratch = ExecuteScratch::default();
                    loop {
                        let job = recv.lock().unwrap().recv();
                        let task = match job {
                            Ok(Job::Whole(task)) => *task,
                            Ok(Job::Blocks(blocks, done)) => {
                                let drained = blocks.drain(&mut runtime, &mut scratch);
                                active.fetch_sub(1, Ordering::Release);
                                let _ = done.send(drained);
                                continue;
                            }
                            Err(_) => break,
                        };
                        let result = run(
                            &mut runtime,
                            task.id,
                            task.context,
                            task.entry,
                            (&task.code, u32::from(task.once)),
                            (&task.signals, RECALL),
                            &mut scratch,
                        );
                        if result.completion.requires_owner {
                            task.signals
                                .recall
                                .fetch_or(OWNER_NEEDED, Ordering::Release);
                        }
                        active.fetch_sub(1, Ordering::Release);
                        let _ = task.done.send(result);
                        if let Some(wake) = task.wake {
                            wake.notify();
                        }
                    }
                })
                .is_ok()
            {
                started += 1;
            }
        }
        (started != 0).then_some(Self {
            send,
            active,
            limit: started,
        })
    }

    fn idle(&self) -> usize {
        self.limit - self.active.load(Ordering::Acquire)
    }

    fn submit(&self, task: Job) -> bool {
        if self
            .active
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |active| {
                (active < self.limit).then_some(active + 1)
            })
            .is_err()
        {
            return false;
        }
        if self.send.try_send(task).is_err() {
            self.active.fetch_sub(1, Ordering::Release);
            return false;
        }
        true
    }
}

const PAGE_GRANTS: usize = 256;

struct PageGrants {
    slots: [u64; PAGE_GRANTS],
}

impl Default for PageGrants {
    fn default() -> Self {
        Self {
            slots: [0; PAGE_GRANTS],
        }
    }
}

impl PageGrants {
    fn clear(&mut self) {
        self.slots.fill(0);
    }

    #[inline]
    fn allows(&mut self, memory: &Memory, start: u32, end: u64, write: bool) -> bool {
        if end <= u64::from(start) {
            return false;
        }
        let Ok(last_byte) = u32::try_from(end - 1) else {
            return false;
        };
        let first = start >> 12;
        let last = last_byte >> 12;
        let need_bit = 1u64 << if write { 33 } else { 32 };
        for page in first..=last {
            let tag = u64::from(page) + 1;
            let idx = ((page ^ (page >> 8)) as usize) & (PAGE_GRANTS - 1);
            let slot = self.slots[idx];
            if (slot & 0xffff_ffff) == tag && (slot & need_bit) != 0 {
                continue;
            }
            let page_addr = page << 12;
            if memory.computation_range(page_addr, 4096, write) {
                let base = if (slot & 0xffff_ffff) == tag {
                    slot
                } else {
                    tag
                };
                self.slots[idx] = base | need_bit;
                continue;
            }
            let Ok(len) = usize::try_from(end - u64::from(start)) else {
                return false;
            };
            return memory.computation_range(start, len, write);
        }
        true
    }
}

#[derive(Default)]
pub(super) struct Admission {
    eval: Evaluation,
    grants: PageGrants,
    generation: (u64, u64),
    spare: Vec<Vec<Range>>,
}

impl Admission {
    fn sync_generation(&mut self, memory: &Memory) {
        let generation = (memory.identity(), memory.map_generation());
        if self.generation != generation {
            self.generation = generation;
            self.grants.clear();
        }
    }

    /// Whether the owner may run the computation at `cpu` itself, with no
    /// job active: its ranges evaluate and are ordinary memory.
    pub(super) fn alone(
        &mut self,
        proof: &Proof,
        cpu: &CpuState,
        memory: &Memory,
        alias: u64,
    ) -> bool {
        self.sync_generation(memory);
        let grants = &mut self.grants;
        let Some((ranges, outputs)) = proof.evaluate_alone(cpu, &mut self.eval, |addr| {
            parameter(memory, grants, alias, &[], addr)
        }) else {
            return false;
        };
        ranges.iter().chain(outputs).all(|range| {
            self.grants
                .allows(memory, range.start, range.end, range.write)
        })
    }

    fn ranges<'a>(
        &'a mut self,
        proof: &Proof,
        cpu: &adv_cpu_core::CpuState,
        memory: &Memory,
        active: &[ActiveTask],
    ) -> Option<&'a [Range]> {
        self.sync_generation(memory);
        let alias = alias_base(memory);
        let grants = &mut self.grants;
        let ranges = proof.evaluate(cpu, &mut self.eval, |addr| {
            parameter(memory, grants, alias, active, addr)
        })?;
        ranges
            .iter()
            .all(|range| {
                !conflicts(range, active)
                    && self
                        .grants
                        .allows(memory, range.start, range.end, range.write)
            })
            .then_some(ranges)
    }
}

impl Admission {
    /// After `ranges`, the blocks of its head loop, if it splits.
    fn plan(
        &mut self,
        proof: &Proof,
        cpu: &adv_cpu_core::CpuState,
        memory: &Memory,
        active: &[ActiveTask],
    ) -> Option<Plan> {
        let alias = alias_base(memory);
        let grants = &mut self.grants;
        proof.plan(cpu, &mut self.eval, |addr| {
            parameter(memory, grants, alias, active, addr)
        })
    }
}

impl Admission {
    /// Whether the evaluated computation may also write `region`, which it
    /// does not otherwise touch.
    fn private(&mut self, memory: &Memory, active: &[ActiveTask], region: &Range) -> bool {
        let overlaps = |other: &Range| {
            u64::from(region.start) < other.end && u64::from(other.start) < region.end
        };
        !self.eval.admitted().iter().any(overlaps)
            && !conflicts(region, active)
            && self
                .grants
                .allows(memory, region.start, region.end, region.write)
    }
}

/// One ordinary four-byte parameter cell, after checking active write leases.
#[inline]
fn parameter(
    memory: &Memory,
    grants: &mut PageGrants,
    alias: u64,
    active: &[ActiveTask],
    addr: u32,
) -> Option<u32> {
    let range = Range {
        start: addr,
        end: u64::from(addr) + 4,
        write: false,
    };
    if (!active.is_empty() && conflicts(&range, active))
        || !grants.allows(memory, range.start, range.end, false)
    {
        return None;
    }
    // SAFETY: `grants.allows(..., false)` verified `range` is committed,
    // readable, ordinary non-shared storage within `memory`'s private alias.
    Some(u32::from_le(unsafe {
        std::ptr::read_unaligned(alias.wrapping_add(u64::from(addr)) as *const u32)
    }))
}

struct ActiveTask {
    id: u32,
    entry: u32,
    iteration: bool,
    ranges: Vec<Range>,
}

fn conflicts(range: &Range, active: &[ActiveTask]) -> bool {
    // Admission allows only the one private reservation. Guest offsets in
    // that reservation cannot alias a different ordinary guest offset.
    active.iter().flat_map(|job| &job.ranges).any(|other| {
        (range.write || other.write)
            && u64::from(range.start) < other.end
            && u64::from(other.start) < range.end
    })
}

pub(super) struct Session<'a> {
    entry: Entry,
    prepared: &'a FxHashMap<u32, Prepared>,
    memory: &'a Memory,
    admission: &'a mut Admission,
    retired: &'a mut u64,
    forms: &'a mut Option<[u64; 5]>,
    pool: &'static Pool,
    active: Vec<ActiveTask>,
    signals: Signals,
    wake: Option<WakeEvent>,
    send: SyncSender<Done>,
    recv: Receiver<Done>,
    runtime: Runtime,
    pace: Option<Pace>,
    counts: &'a mut SplitCounts,
}

impl<'a> Session<'a> {
    pub(super) fn new(
        entry: Entry,
        prepared: &'a FxHashMap<u32, Prepared>,
        memory: &'a Memory,
        admission: &'a mut Admission,
        (retired, forms): (&'a mut u64, &'a mut Option<[u64; 5]>),
        notifications: Notifications,
        (pace, counts): (Option<Pace>, &'a mut SplitCounts),
    ) -> Option<Self> {
        let pool = Pool::get()?;
        let (send, recv) = sync_channel(pool.limit);
        Some(Self {
            entry,
            prepared,
            memory,
            admission,
            retired,
            forms,
            pool,
            wake: notifications.wake,
            send,
            recv,
            active: Vec::with_capacity(pool.limit),
            signals: Signals {
                recall: Arc::new(AtomicU32::new(0)),
                pending: notifications.pending,
            },
            runtime: Runtime::new(std::ptr::null_mut(), entry.window),
            pace,
            counts,
        })
    }

    fn prepare(&mut self, id: u32, context: &ExecutionContext) -> Option<(&Code, &[Range], bool)> {
        let cpu = context.cpu();
        if self.signals.requested() || self.active.iter().any(|job| job.id == id) {
            return None;
        }
        if cpu.eflags & ((1 << 8) | (1 << 16)) != 0 || cpu.rep.is_some() || context.has_resume() {
            return None;
        }
        let prepared = self.prepared.get(&cpu.eip)?;
        let ranges = self
            .admission
            .ranges(&prepared.proof, cpu, self.memory, &self.active)?;
        let (code, iteration) = prepared.selected();
        Some((code, ranges, iteration))
    }

    /// Runs the admitted head loop as blocks of about one budget slice, on
    /// the owner and the fewest idle workers that save what the frame's
    /// pace needs of its serial time.
    fn split(
        &mut self,
        id: u32,
        context: &ExecutionContext,
        code: &Code,
        plan: Plan,
        (retired, ns): (u64, f64),
        pace: Pace,
    ) -> Option<Completion<Option<ExecutionContext>>> {
        let slice = (u64::from(COMPUTATION_SLICE) / retired).clamp(1, u64::from(u32::MAX)) as u32;
        let serial = f64::from(plan.iterations) * ns;
        let most = self.pool.idle().min(plan.iterations as usize - 1);
        let counts = &*self.counts;
        let wake = match counts.wakes - counts.idle_wakes {
            0 => WAKE_NS,
            claimed => counts.claim_ns as f64 / claimed as f64,
        };
        let need = pace.need(serial as u64) as f64;
        let helpers = helpers(serial, (wake, f64::from(slice) * ns), need, most);
        if helpers == 0 {
            return None;
        }
        let threads = (helpers + 1) as f64 * BLOCKS_PER_THREAD;
        let size = slice.min((f64::from(plan.iterations) / threads).ceil() as u32);
        let count = plan.iterations.div_ceil(size);
        // Private frames lie below ESP, one stride per thread.
        if let Some(frame) = plan.frame {
            let stride = frame_stride(frame);
            let below = (helpers as u32 + 1) * stride;
            let start = context.cpu().regs[ESP]
                .wrapping_sub(below)
                .wrapping_add(frame.0 as u32);
            let region = Range {
                start,
                end: u64::from(start) + u64::from(below - stride) + (frame.1 - frame.0) as u64,
                write: true,
            };
            if !self.admission.private(self.memory, &self.active, &region) {
                return None;
            }
        }
        let started = Instant::now();
        let mut start = context.clone();
        start.cpu_mut().settle_flags();
        let blocks = Arc::new(Blocks {
            next: AtomicU32::new(0),
            stop: std::sync::atomic::AtomicBool::new(false),
            threads: AtomicU32::new(0),
            count,
            size,
            plan,
            start,
            entry: self.entry,
            code: code.clone(),
            signals: self.signals.clone(),
            submitted: started,
        });
        let (send, recv) = sync_channel(helpers);
        let submitted = (0..helpers)
            .take_while(|_| self.pool.submit(Job::Blocks(blocks.clone(), send.clone())))
            .count();
        let mut total = blocks.drain(&mut self.runtime, &mut ExecuteScratch::default());
        let mut helped = Duration::ZERO;
        self.counts.splits += 1;
        self.counts.wakes += submitted as u64;
        for _ in 0..submitted {
            let drained = recv.recv().expect("block helper");
            if drained.blocks == 0 {
                self.counts.idle_wakes += 1;
            } else {
                self.counts.claim_ns += drained.claimed_after.as_nanos() as u64;
            }
            total.blocks += drained.blocks;
            total.retired += drained.retired;
            total.optimized += drained.optimized;
            total.sticky |= drained.sticky;
            total.last = total.last.or(drained.last);
            helped += drained.elapsed;
        }
        // A recall stops claims: the completed prefix returns at the head.
        let claimed = blocks.next.load(Ordering::Acquire).min(count);
        let iterations = if claimed == count {
            plan.iterations
        } else {
            claimed * size
        };
        let (mut completion, last) = match total.last.take() {
            Some((done, thread)) => (done, Some(thread)),
            None => (
                Completion {
                    id,
                    context: Some(blocks.at(iterations)),
                    retired: 0,
                    optimized: 0,
                    elapsed: Duration::ZERO,
                    helpers: Duration::ZERO,
                    requires_owner: false,
                },
                None,
            ),
        };
        blocks.settle(
            completion.context.as_mut().unwrap().cpu_mut(),
            last,
            iterations,
        );
        self.counts.blocks += u64::from(total.blocks);
        completion.id = id;
        completion.retired = total.retired;
        completion.optimized = total.optimized;
        completion.elapsed = started.elapsed();
        completion.helpers = helped;
        completion.context.as_mut().unwrap().cpu_mut().mxcsr |= total.sticky;
        learn(&self.prepared[&context.cpu().eip], &completion, iterations);
        Some(completion)
    }

    fn account(
        &mut self,
        entry: u32,
        iteration: bool,
        done: Done,
    ) -> Completion<Option<ExecutionContext>> {
        let prepared = &self.prepared[&entry];
        prepared.observe(entry, iteration, &done);
        if let Some(admitted) = done.admitted {
            prepared.admitted(admitted);
        }
        let retired = done.completion.retired;
        *self.retired += retired;
        if let Some(forms) = self.forms {
            forms[0] += retired;
        }
        done.completion
    }

    fn accept(&mut self, done: Done) -> Completion<Option<ExecutionContext>> {
        let index = self
            .active
            .iter()
            .position(|job| job.id == done.completion.id)
            .expect("active computation");
        let job = self.active.swap_remove(index);
        self.admission.spare.push(job.ranges);
        self.account(job.entry, job.iteration, done)
    }
}

/// The fewest helpers whose blocks cut `serial` ns by `need` ns, or all
/// that are worth waking, given the `wake` latency and the most a `slice`
/// block takes. One is worth waking only if its part of the loop exceeds
/// four wake latencies.
fn helpers(serial: f64, (wake, slice): (f64, f64), need: f64, most: usize) -> usize {
    if need <= 0.0 {
        return 0;
    }
    let worth = ((serial - wake) / (4.0 * wake) - 1.0).max(0.0) as usize;
    let most = most.min(worth);
    let target = serial - need;
    (1..=most)
        .find(|&h| {
            let part = (serial - wake) / (h + 1) as f64;
            wake + part + (part / BLOCKS_PER_THREAD).min(slice) + ROUND_NS <= target
        })
        .unwrap_or(most)
}

/// Records the instructions and time per outer iteration of a job that ran
/// `iterations` of them.
fn learn(prepared: &Prepared, done: &Completion<Option<ExecutionContext>>, iterations: u32) {
    if iterations == 0 || done.requires_owner {
        return;
    }
    let n = u64::from(iterations);
    let ns = (done.elapsed + done.helpers).as_nanos() as f64;
    prepared.set_cost(ns as u64);
    prepared
        .per_iteration
        .set(Some(((done.retired / n).max(1), ns / n as f64)));
}

impl Computation<Option<ExecutionContext>> for Session<'_> {
    fn submit(&mut self, id: u32, context: &Option<ExecutionContext>) -> bool {
        let Some(context) = context.as_ref() else {
            return false;
        };
        let guest_entry = context.cpu().eip;
        if self.active.len() == self.pool.limit {
            return false;
        }
        let mut grant = self.admission.spare.pop().unwrap_or_default();
        grant.clear();
        let Some((code, ranges, iteration)) = self.prepare(id, context) else {
            self.admission.spare.push(grant);
            return false;
        };
        grant.extend_from_slice(ranges);
        let code = code.clone();
        if self.pool.submit(Job::Whole(Box::new(Task {
            id,
            context: context.clone(),
            code,
            once: iteration,
            entry: self.entry,
            signals: self.signals.clone(),
            done: self.send.clone(),
            wake: self.wake.clone(),
        }))) {
            self.active.push(ActiveTask {
                id,
                entry: guest_entry,
                iteration,
                ranges: grant,
            });
            true
        } else {
            self.admission.spare.push(grant);
            false
        }
    }

    fn run_local(
        &mut self,
        id: u32,
        context: &Option<ExecutionContext>,
    ) -> Option<Completion<Option<ExecutionContext>>> {
        let context = context.as_ref()?;
        let guest_entry = context.cpu().eip;
        let (code, _, iteration) = self.prepare(id, context)?;
        let code = code.clone();
        let prepared = &self.prepared[&guest_entry];
        let plan = prepared
            .proof
            .splits()
            .then(|| {
                self.admission
                    .plan(&prepared.proof, context.cpu(), self.memory, &self.active)
            })
            .flatten();
        if let Some(plan) = plan
            && let Some(pace) = self.pace
            && let Some(per_iteration) = prepared.per_iteration.get()
            && let Some(completion) = self.split(id, context, &code, plan, per_iteration, pace)
        {
            let done = Done {
                completion,
                deadline_recall: false,
                admitted: None,
            };
            return Some(self.account(guest_entry, false, done));
        }
        let mut scratch = ExecuteScratch::default();
        let done = run(
            &mut self.runtime,
            id,
            context.clone(),
            self.entry,
            (&code, u32::from(iteration)),
            (&self.signals, LOCAL),
            &mut scratch,
        );
        if let Some(plan) = plan
            && !done.completion.requires_owner
        {
            let eip = done.completion.context.as_ref().unwrap().cpu().eip;
            let iterations = match (eip == guest_entry, iteration) {
                (false, _) => plan.iterations,
                (true, true) => 1,
                (true, false) => 0,
            };
            learn(prepared, &done.completion, iterations);
        }
        Some(self.account(guest_entry, iteration, done))
    }

    fn poll(&mut self) -> Option<Completion<Option<ExecutionContext>>> {
        self.recv.try_recv().ok().map(|done| self.accept(done))
    }

    fn wait(&mut self, timeout: Option<Duration>) -> Option<Completion<Option<ExecutionContext>>> {
        if self.active.is_empty() {
            return None;
        }
        let done = match timeout {
            Some(timeout) => self.recv.recv_timeout(timeout).ok()?,
            None => self.recv.recv().ok()?,
        };
        Some(self.accept(done))
    }

    fn recall(&mut self) {
        self.signals.recall.fetch_or(RECALL, Ordering::Release);
    }
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        self.recall();
        while self.wait(None).is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use adv_cpu_core::memory::{EXEC, READ, WRITE};
    use iced_x86::code_asm::*;

    use super::*;
    use crate::Executor;
    use crate::native::Native;
    use crate::translate::{self, Workspace};

    const CODE: u32 = 0x1_0000;
    const INPUT: u32 = 0x2_0000;
    const OUTPUT: u32 = 0x4_0000;
    const COUNT: u32 = 40_000;
    const SPACE: u32 = 0x4_0000;

    /// The pool is process-wide: tests that need its workers take turns,
    /// or a parallel test's blocks can fill it.
    fn exclusive_pool() -> std::sync::MutexGuard<'static, ()> {
        static POOL: Mutex<()> = Mutex::new(());
        POOL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fixture(a: &mut CodeAssembler) -> (Executor, Native) {
        let words = a.assemble(u64::from(CODE)).unwrap();
        let mut executor = Executor::interpreted_address_space().unwrap();
        let memory = executor.memory_mut();
        memory.map(CODE, 0x4000, READ | WRITE).unwrap();
        memory.write(CODE, &words).unwrap();
        memory.protect(CODE, 0x4000, READ | EXEC).unwrap();
        let mut native = Native::new(memory.window_base().unwrap()).unwrap();
        // These tests nominate from the first budget exit; promotion has its own test.
        native.promotion = 0;
        (executor, native)
    }

    fn state(
        executor: &mut Executor,
        eip: u32,
        output: u32,
        count: u32,
    ) -> Option<ExecutionContext> {
        executor.state.eip = eip;
        executor.state.regs[1] = count;
        executor.state.regs[6] = INPUT;
        executor.state.regs[7] = output;
        Some(executor.save_context().unwrap())
    }

    // spec: A64-CTL-7 A64-CTL-9 A64-CTL-10 A64-TR-9
    #[test]
    fn disjoint_jobs_keep_their_prefixes_across_recall_and_refill() {
        let _pool = exclusive_pool();
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        let mut loop_head = a.create_label();
        a.set_label(&mut loop_head).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(loop_head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let memory = executor.memory_mut();
        // Only the workers use the first source: it is one guest page in
        // a mixed host page on a 16 KiB host.
        memory.map(INPUT, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(INPUT, &0x1234_5678u32.to_le_bytes()).unwrap();
        memory.map(INPUT + 0x4000, 0x4000, READ | WRITE).unwrap();
        memory
            .map(OUTPUT, (SPACE * 3) as usize, READ | WRITE | EXEC)
            .unwrap();
        let head = CODE + 1;
        let mut cpu = adv_cpu_core::CpuState {
            eip: CODE,
            ..Default::default()
        };
        cpu.regs[1] = COUNT;
        cpu.regs[6] = INPUT + 0x4000;
        cpu.regs[7] = OUTPUT + SPACE * 2;
        native.first_visits = false;
        // Warm the ordinary incoming trace before the scheduler first
        // requests a candidate. It includes the loop, without a rendezvous.
        let warm = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(warm.exit, Exit::Dispatch);
        assert_eq!(cpu.eip, head);
        native.set_computation_offer(true, None);
        cpu.eip = CODE;
        let warm = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(warm.exit, Exit::Dispatch);
        assert_eq!(cpu.eip, head);
        let entry = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(entry.exit, Exit::Computation);
        assert_eq!(entry.retired, 0);
        cpu.eip = CODE;
        let incoming = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(incoming.exit, Exit::Dispatch);
        assert_eq!((cpu.eip, incoming.retired), (head, 1));

        let first = state(&mut executor, head, OUTPUT, COUNT);
        let second = state(&mut executor, head, OUTPUT + SPACE, COUNT);
        let mut retired = 0;
        let mut got = None;
        let notifications = (Arc::new(AtomicU32::new(0)), 0);
        native.with_computation(
            executor.memory_mut(),
            &mut retired,
            notifications,
            None,
            &mut |scope| {
                assert!(scope.submit(1, &first));
                assert!(!scope.submit(2, &first));
                let local = scope
                    .run_local(2, &second)
                    .expect("disjoint local computation");
                assert!(!local.requires_owner);
                assert_eq!(local.retired, u64::from(COUNT) * 5);
                assert_eq!(local.context.as_ref().unwrap().cpu().regs[1], 0);
                let worker = scope
                    .wait(Some(Duration::from_secs(5)))
                    .expect("worker completion");
                assert_eq!(worker.id, 1);
                assert_eq!(worker.retired, u64::from(COUNT) * 5);
                assert_eq!(worker.context.as_ref().unwrap().cpu().regs[1], 0);
                got = Some((local, worker));
            },
        );
        let (local, worker) = got.expect("resident pool");
        assert_eq!(retired, local.retired + worker.retired);
        let memory = executor.memory();
        assert_eq!(
            memory.read(OUTPUT + 4 * (COUNT - 1), 4).unwrap(),
            [0x78, 0x56, 0x34, 0x12]
        );
        let local_end = OUTPUT + SPACE + local.retired as u32 / 5 * 4;
        assert_eq!(
            memory.read(local_end - 4, 4).unwrap(),
            [0x78, 0x56, 0x34, 0x12]
        );
        assert_eq!(memory.read(local_end, 4).unwrap(), [0; 4]);

        // With the owner deadline pending, the same local entry finishes the
        // iteration after the first exhausted budget and returns at its head
        // (A64-CTL-8), with the exact completed prefix.
        let local = run(
            &mut Runtime::new(std::ptr::null_mut(), 0),
            3,
            second.unwrap(),
            Entry {
                enter: native.enter,
                opt_enter: native.opt_enter,
                window: native.runtime.window,
                alias: super::super::alias_base(executor.memory()),
                fault: native.stubs.fault,
            },
            (&native.computation.get(&head).unwrap().code, 0),
            (
                &Signals {
                    recall: Arc::new(AtomicU32::new(0)),
                    pending: (
                        Arc::new(AtomicU32::new(adv_sync::pending::DEADLINE)),
                        adv_sync::pending::DEADLINE,
                    ),
                },
                LOCAL,
            ),
            &mut ExecuteScratch::default(),
        )
        .completion;
        assert!(!local.requires_owner);
        assert_eq!(local.retired, u64::from(COMPUTATION_SLICE / 5 * 5 + 5));
        let left = local.context.as_ref().unwrap().cpu().regs[1];
        assert_eq!(u64::from(COUNT - left) * 5, local.retired);

        let memory = executor.memory_mut();
        memory.write(head, &[0xcc]).unwrap();
        native.invalidate(memory);
        assert!(!native.computation.contains_key(&head));
    }

    // spec: A64-TR-9
    #[test]
    fn helpers_save_the_share_past_their_wake() {
        // A 400 us loop that must lose 10%: one helper once wakes are known
        // to take 12 us, none while a wake is assumed to take 50 us.
        assert_eq!(helpers(400_000.0, (12_000.0, 1e9), 40_000.0, 7), 1);
        assert_eq!(helpers(400_000.0, (50_000.0, 1e9), 40_000.0, 7), 0);
        // Half of it needs two; a held key takes every helper worth one.
        assert_eq!(helpers(400_000.0, (12_000.0, 1e9), 200_000.0, 7), 2);
        assert_eq!(helpers(400_000.0, (12_000.0, 1e9), 400_000.0, 7), 7);
        assert_eq!(helpers(400_000.0, (12_000.0, 1e9), 0.0, 7), 0);
    }

    const SRC: u32 = 0x100_0000;
    const DST: u32 = 0x200_0000;
    const N: u32 = 1 << 20;

    /// Offers the head loop at CODE, runs it from `start` once whole and
    /// once split, and checks that both leave the same state and memory
    /// `check`. `reset` restores memory before each run.
    fn whole_then_split(
        executor: &mut Executor,
        native: &mut Native,
        start: &Option<ExecutionContext>,
        stop: u32,
        reset: impl Fn(&mut Memory),
        check: &[(u32, usize)],
    ) -> (u64, ExecutionContext) {
        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        let mut cpu = start.as_ref().unwrap().cpu().clone();
        let offer = native
            .run(
                &mut cpu,
                executor.memory_mut(),
                stop,
                1 << 16,
                std::ptr::null_mut(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(offer.exit, Exit::Computation);
        assert!(native.computation[&CODE].proof.splits());
        native.pace.set(Some(Pace::default()));
        let mut results = Vec::new();
        let mut memories = Vec::new();
        // The first job shows the iteration's cost; the second splits.
        for _ in 0..2 {
            reset(executor.memory_mut());
            let mut retired = 0;
            native.with_computation(
                executor.memory_mut(),
                &mut retired,
                (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
                None,
                &mut |scope| results.push(scope.run_local(1, start).expect("admitted loop")),
            );
            assert_eq!(retired, results.last().unwrap().retired);
            let memory = executor.memory();
            memories.push(
                check
                    .iter()
                    .map(|&(at, len)| memory.read(at, len).unwrap().to_vec())
                    .collect::<Vec<_>>(),
            );
        }
        assert!(memories[0] == memories[1], "split memory differs");
        let split = native.diagnostics.split;
        assert_eq!(split.splits, 1);
        assert!(split.wakes >= 1 && split.blocks > 2);
        let [whole, blocks] = results.as_slice() else {
            unreachable!()
        };
        assert_eq!(whole.helpers, Duration::ZERO);
        assert!(!blocks.requires_owner);
        assert_eq!(blocks.retired, whole.retired);
        let (whole, blocks) = (
            whole.context.as_ref().unwrap(),
            blocks.context.as_ref().unwrap(),
        );
        assert_eq!(blocks.cpu().eip, stop);
        assert_eq!(blocks.cpu(), whole.cpu());
        (results[0].retired, whole.clone())
    }

    /// Blocks of the offered loop, stopped after claiming `claimed` of 8.
    fn stopped_blocks(
        executor: &Executor,
        native: &Native,
        start: &Option<ExecutionContext>,
        claimed: u32,
    ) -> Blocks {
        let (proof, cpu) = (
            &native.computation[&CODE].proof,
            start.as_ref().unwrap().cpu(),
        );
        let mut scratch = Evaluation::default();
        let read = |addr| {
            let bytes = executor.memory().read(addr, 4).ok()?;
            Some(u32::from_le_bytes(bytes.try_into().ok()?))
        };
        proof.evaluate(cpu, &mut scratch, read).unwrap();
        let plan = proof.plan(cpu, &mut scratch, read);
        Blocks {
            next: AtomicU32::new(claimed),
            stop: std::sync::atomic::AtomicBool::new(true),
            threads: AtomicU32::new(0),
            count: 8,
            size: N / 8,
            plan: plan.expect("blocks"),
            start: start.clone().unwrap(),
            entry: Entry {
                enter: native.enter,
                opt_enter: native.opt_enter,
                window: native.runtime.window,
                alias: super::super::alias_base(executor.memory()),
                fault: native.stubs.fault,
            },
            code: native.computation[&CODE].code.clone(),
            signals: Signals {
                recall: Arc::new(AtomicU32::new(0)),
                pending: (Arc::new(AtomicU32::new(0)), 0),
            },
            submitted: Instant::now(),
        }
    }

    // spec: A64-CTL-7 A64-CTL-8 A64-TR-9
    #[test]
    fn a_single_context_runs_its_head_loop_as_exact_blocks() {
        let _pool = exclusive_pool();
        if Pool::get().is_none_or(|pool| pool.limit < 2) {
            return;
        }
        // head: dst[i] = (src[i] + dst[i]) >> 1, in place.
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, dword_ptr(edi)).unwrap();
        a.shr(eax, 1).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let len = N as usize * 4;
        let memory = executor.memory_mut();
        memory.map(SRC, len, READ | WRITE).unwrap();
        memory.map(DST, len, READ | WRITE).unwrap();
        let src: Vec<u8> = (0..N).flat_map(|i| (i * 7).to_le_bytes()).collect();
        let dst: Vec<u8> = (0..N).flat_map(|i| (i ^ 0x5555).to_le_bytes()).collect();
        memory.write(SRC, &src).unwrap();
        let expected: Vec<u8> = (0..N)
            .flat_map(|i| ((i * 7 + (i ^ 0x5555)) >> 1).to_le_bytes())
            .collect();
        executor.state.eip = CODE;
        executor.state.regs[1] = N;
        executor.state.regs[6] = SRC;
        executor.state.regs[7] = DST;
        executor.state.mxcsr = 0x1f80;
        let start = Some(executor.save_context().unwrap());
        let reset = |memory: &mut Memory| memory.write(DST, &dst).unwrap();
        let (retired, _) = whole_then_split(
            &mut executor,
            &mut native,
            &start,
            stop,
            reset,
            &[(DST, len)],
        );
        assert_eq!(retired, u64::from(N) * 8);
        assert_eq!(executor.memory().read(DST, len).unwrap(), expected);

        // A recalled split returns the completed prefix at the head.
        let prefix = stopped_blocks(&executor, &native, &start, 3).at(3 * N / 8);
        let cpu = prefix.cpu();
        assert_eq!((cpu.eip, cpu.regs[1]), (CODE, N - 3 * N / 8));
        assert_eq!(
            (cpu.regs[6], cpu.regs[7]),
            (SRC + 3 * N / 2, DST + 3 * N / 2)
        );
    }

    // spec: A64-OPT-1 A64-OPT-6
    #[test]
    fn an_admitted_x87_loop_runs_its_optimized_form_exactly() {
        const N: u32 = 4096;
        // head: dst[i] *= src[i] in binary64.
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.fld(qword_ptr(esi)).unwrap();
        a.fmul(qword_ptr(edi)).unwrap();
        a.fstp(qword_ptr(edi)).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let len = N as usize * 8;
        let value = |i: u32, scale: f64| 1.0 + f64::from(i) / scale;
        let memory = executor.memory_mut();
        memory.map(SRC, len, READ | WRITE).unwrap();
        memory.map(DST, len, READ | WRITE).unwrap();
        let src: Vec<u8> = (0..N)
            .flat_map(|i| value(i, 1024.0).to_le_bytes())
            .collect();
        let dst: Vec<u8> = (0..N).flat_map(|i| value(i, 3.0).to_le_bytes()).collect();
        memory.write(SRC, &src).unwrap();
        memory.write(DST, &dst).unwrap();
        let expected: Vec<u8> = (0..N)
            .flat_map(|i| (value(i, 1024.0) * value(i, 3.0)).to_le_bytes())
            .collect();
        executor.state.eip = CODE;
        executor.state.regs[1] = N;
        executor.state.regs[6] = SRC;
        executor.state.regs[7] = DST;
        executor.state.mxcsr = 0x1f80;
        executor.state.x87.control = 0x27f;
        executor.state.x87.status = 0x20;
        let start = Some(executor.save_context().unwrap());
        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        let mut cpu = start.as_ref().unwrap().cpu().clone();
        let offer = native
            .run(
                &mut cpu,
                executor.memory_mut(),
                stop,
                1 << 16,
                std::ptr::null_mut(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(offer.exit, Exit::Computation);
        let mut retired = 0;
        let mut done = None;
        native.with_computation(
            executor.memory_mut(),
            &mut retired,
            (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
            None,
            &mut |scope| done = scope.run_local(1, &start),
        );
        let done = done.expect("admitted loop");
        assert_eq!(done.retired, u64::from(N) * 7);
        assert_eq!(done.optimized, done.retired);
        assert_eq!(executor.memory().read(DST, len).unwrap(), expected);
        let cpu = done.context.as_ref().unwrap().cpu();
        assert_eq!(cpu.eip, stop);
        assert_eq!(
            (cpu.regs[1], cpu.regs[6], cpu.regs[7]),
            (0, SRC + N * 8, DST + N * 8)
        );
        assert_eq!(cpu.x87.tag, 0xffff);
    }

    // spec: A64-OPT-1 A64-TR-9 A64F-FP-4
    #[test]
    fn an_admitted_mmx_pixel_loop_splits_and_matches_serial() {
        let _pool = exclusive_pool();
        if Pool::get().is_none_or(|pool| pool.limit < 2) {
            return;
        }
        // head: dst[i] = ((src[i] * alpha) >> 8) + dst[i] per 8-bit channel.
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.movd(mm0, dword_ptr(esi)).unwrap();
        a.movd(mm1, dword_ptr(edi)).unwrap();
        a.punpcklbw(mm0, mm7).unwrap();
        a.punpcklbw(mm1, mm7).unwrap();
        a.pmullw(mm0, mm6).unwrap();
        a.psrlw(mm0, 8).unwrap();
        a.paddw(mm0, mm1).unwrap();
        a.packuswb(mm0, mm7).unwrap();
        a.movd(dword_ptr(edi), mm0).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let len = N as usize * 4;
        let memory = executor.memory_mut();
        memory.map(SRC, len, READ | WRITE).unwrap();
        memory.map(DST, len, READ | WRITE).unwrap();
        let src: Vec<u8> = (0..N)
            .flat_map(|i| (i.wrapping_mul(0x1357_2461)).to_le_bytes())
            .collect();
        let dst: Vec<u8> = (0..N)
            .flat_map(|i| (i ^ 0x2468_1357).to_le_bytes())
            .collect();
        memory.write(SRC, &src).unwrap();
        let expected: Vec<u8> = src
            .iter()
            .zip(&dst)
            .map(|(&s, &d)| {
                ((u16::from(s) * 0x40) >> 8)
                    .saturating_add(u16::from(d))
                    .min(255) as u8
            })
            .collect();
        executor.state.eip = CODE;
        executor.state.regs[1] = N;
        executor.state.regs[6] = SRC;
        executor.state.regs[7] = DST;
        executor.state.mxcsr = 0x1f80;
        executor.state.x87.control = 0x27f;
        executor.state.x87.status = 5 << 11;
        executor.state.x87.tag = 0x5555;
        executor.state.x87.regs = std::array::from_fn(|n| {
            adv_cpu_core::X87Register::from_parts(
                if n == 7 { 0 } else { 0x0040_0040_0040_0040 },
                0x3fff,
            )
        });
        executor.state.refresh_mmx();
        let start = Some(executor.save_context().unwrap());
        let reset = |memory: &mut Memory| memory.write(DST, &dst).unwrap();
        let (retired, final_context) = whole_then_split(
            &mut executor,
            &mut native,
            &start,
            stop,
            reset,
            &[(DST, len)],
        );
        assert_eq!(retired, u64::from(N) * 13);
        assert_eq!(executor.memory().read(DST, len).unwrap(), expected);
        let cpu = final_context.cpu();
        assert_eq!(cpu.x87.top(), 0);
        assert_eq!(cpu.x87.tag, 0);
        assert_eq!(cpu.x87.regs[0].sign_exponent(), 0xffff);
        assert_eq!(cpu.x87.regs[1].sign_exponent(), 0xffff);
        assert_eq!(cpu.x87.regs[6].sign_exponent(), 0x3fff);
        assert_eq!(cpu.x87.regs[7].sign_exponent(), 0x3fff);
    }

    // spec: A64-OPT-1 A64-TR-9 A64F-FP-4
    #[test]
    fn an_mmx_accumulator_loop_refuses_split_and_runs_serially() {
        const COUNT: u32 = 4096;
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.paddw(mm0, qword_ptr(esi)).unwrap();
        a.movq(qword_ptr(edi), mm0).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let len = COUNT as usize * 8;
        let memory = executor.memory_mut();
        memory.map(SRC, len, READ | WRITE).unwrap();
        memory.map(DST, len, READ | WRITE).unwrap();
        let mut acc = [1u16, 2, 3, 4];
        let mut src = Vec::with_capacity(len);
        let mut expected_dst = Vec::with_capacity(len);
        for i in 0..COUNT {
            for (lane, acc_lane) in acc.iter_mut().enumerate() {
                let w = ((i * 5 + lane as u32) as u16).wrapping_mul(17);
                *acc_lane = acc_lane.wrapping_add(w);
                src.extend_from_slice(&w.to_le_bytes());
                expected_dst.extend_from_slice(&acc_lane.to_le_bytes());
            }
        }
        memory.write(SRC, &src).unwrap();
        let expected_mm0 = u64::from_le_bytes(
            acc.iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        );
        let seed_mm0 = u64::from_le_bytes(
            [1u16, 2, 3, 4]
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<_>>()
                .try_into()
                .unwrap(),
        );
        executor.state.eip = CODE;
        executor.state.regs[1] = COUNT;
        executor.state.regs[6] = SRC;
        executor.state.regs[7] = DST;
        executor.state.mxcsr = 0x1f80;
        executor.state.x87.control = 0x27f;
        executor.state.x87.regs[0] = adv_cpu_core::X87Register::from_parts(seed_mm0, 0x3fff);
        executor.state.refresh_mmx();
        let start = Some(executor.save_context().unwrap());
        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        let mut cpu = start.as_ref().unwrap().cpu().clone();
        let offer = native
            .run(
                &mut cpu,
                executor.memory_mut(),
                stop,
                1 << 16,
                std::ptr::null_mut(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(offer.exit, Exit::Computation);
        assert!(!native.computation[&CODE].proof.splits());
        let mut retired = 0;
        let mut done = None;
        native.with_computation(
            executor.memory_mut(),
            &mut retired,
            (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
            None,
            &mut |scope| done = scope.run_local(1, &start),
        );
        let done = done.expect("admitted serial loop");
        assert_eq!(done.retired, u64::from(COUNT) * 6);
        assert_eq!(done.optimized, done.retired);
        assert_eq!(executor.memory().read(DST, len).unwrap(), expected_dst);
        let cpu = done.context.as_ref().unwrap().cpu();
        assert_eq!(cpu.mmx[0], expected_mm0);
        assert_eq!(cpu.x87.regs[0].sign_exponent(), 0xffff);
    }

    // spec: A64-OPT-1 A64F-FP-4
    #[test]
    fn pending_x87_exception_denies_mmx_computation_admission() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.paddw(mm0, qword_ptr(esi)).unwrap();
        a.movq(qword_ptr(edi), mm0).unwrap();
        a.add(esi, 8).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let words = a.assemble(u64::from(CODE)).unwrap();
        let stop = CODE + words.len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        executor
            .memory_mut()
            .map(SRC, 0x4000, READ | WRITE)
            .unwrap();
        executor
            .memory_mut()
            .map(DST, 0x4000, READ | WRITE)
            .unwrap();
        executor.state.eip = CODE;
        executor.state.regs[1] = 4;
        executor.state.regs[6] = SRC;
        executor.state.regs[7] = DST;
        executor.state.mxcsr = 0x1f80;
        executor.state.x87.control = 0x27f;
        executor.state.x87.status = 0;
        executor.state.refresh_mmx();
        let clean = Some(executor.save_context().unwrap());
        executor.state.x87.control = 0x27e;
        executor.state.x87.status = 0x81;
        let pending = Some(executor.save_context().unwrap());
        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        let mut cpu = clean.as_ref().unwrap().cpu().clone();
        let offer = native
            .run(
                &mut cpu,
                executor.memory_mut(),
                stop,
                1 << 16,
                std::ptr::null_mut(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(offer.exit, Exit::Computation);
        // Install the optimized form first, then enter with a pending x87 exception.
        let mut retired = 0;
        let mut clean_done = None;
        native.with_computation(
            executor.memory_mut(),
            &mut retired,
            (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
            None,
            &mut |scope| clean_done = scope.run_local(1, &clean),
        );
        assert!(clean_done.unwrap().optimized > 0);
        assert!(native.computation[&CODE].code.optimized.is_some());
        let mut pending_done = None;
        native.with_computation(
            executor.memory_mut(),
            &mut retired,
            (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
            None,
            &mut |scope| pending_done = scope.run_local(2, &pending),
        );
        let done = pending_done.unwrap();
        assert_eq!(done.optimized, 0);
        assert_eq!(done.retired, 0);
        assert!(done.requires_owner);
        let mut owner_cpu = done.context.unwrap().cpu().clone();
        assert_eq!(owner_cpu.eip, CODE);
        // Owner fallback runs the ordinary path and leaves to raise #MF at CODE.
        native.set_computation_offer(false, None);
        let fallback = native
            .run(
                &mut owner_cpu,
                executor.memory_mut(),
                stop,
                1000,
                std::ptr::null_mut(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            (fallback.exit, fallback.retired, owner_cpu.eip),
            (Exit::Interpret, 0, CODE)
        );
        let mut native_exec = Executor::address_space().unwrap();
        let mut interp_exec = Executor::interpreted_address_space().unwrap();
        interp_exec.interpreter_only = true;
        for exec in [&mut native_exec, &mut interp_exec] {
            exec.memory_mut().map(CODE, 0x4000, READ | WRITE).unwrap();
            exec.memory_mut().write(CODE, &words).unwrap();
            exec.memory_mut()
                .protect(CODE, 0x4000, READ | EXEC)
                .unwrap();
            exec.memory_mut().map(SRC, 0x4000, READ | WRITE).unwrap();
            exec.memory_mut().map(DST, 0x4000, READ | WRITE).unwrap();
            exec.state = owner_cpu.clone();
        }
        let native_out = native_exec.run(stop, 1000).unwrap();
        let interp_out = interp_exec.run(stop, 1000).unwrap();
        assert_eq!(
            native_out,
            adv_cpu_core::RunOutcome::Trap {
                pc: CODE,
                vector: 16
            }
        );
        assert_eq!(native_out, interp_out);
        native_exec.state.fuel = 0;
        interp_exec.state.fuel = 0;
        assert_eq!(native_exec.state, interp_exec.state);
    }

    // spec: A64-OPT-1 A64-OPT-6 A64F-FP-4 A64-VER-1
    #[test]
    fn mmx_optimized_matches_interpreter_across_flagm_cuts_and_faults() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.movd(mm0, dword_ptr(esi)).unwrap();
        a.movd(mm1, dword_ptr(edi)).unwrap();
        a.punpcklbw(mm0, mm7).unwrap();
        a.punpcklbw(mm1, mm7).unwrap();
        a.pmullw(mm0, mm6).unwrap();
        a.psrlw(mm0, 8).unwrap();
        a.paddw(mm0, mm1).unwrap();
        a.packuswb(mm0, mm7).unwrap();
        a.movd(dword_ptr(edi), mm0).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let words = a.assemble(u64::from(CODE)).unwrap();
        let stop = CODE + words.len() as u32 - 1;
        let host = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize).max(0x4000);
        for flagm in [false, true] {
            for emms_before in [false, true] {
                // Case 1: budget cut (deadline recall) + full resume vs interpreter.
                const COUNT: u32 = 8192;
                let len = (COUNT as usize * 4).max(host);
                let mut a_prog = CodeAssembler::new(32).unwrap();
                let mut h = a_prog.create_label();
                a_prog.set_label(&mut h).unwrap();
                a_prog.movd(mm0, dword_ptr(esi)).unwrap();
                a_prog.movd(mm1, dword_ptr(edi)).unwrap();
                a_prog.punpcklbw(mm0, mm7).unwrap();
                a_prog.punpcklbw(mm1, mm7).unwrap();
                a_prog.pmullw(mm0, mm6).unwrap();
                a_prog.psrlw(mm0, 8).unwrap();
                a_prog.paddw(mm0, mm1).unwrap();
                a_prog.packuswb(mm0, mm7).unwrap();
                a_prog.movd(dword_ptr(edi), mm0).unwrap();
                a_prog.add(esi, 4).unwrap();
                a_prog.add(edi, 4).unwrap();
                a_prog.dec(ecx).unwrap();
                a_prog.jne(h).unwrap();
                a_prog.int3().unwrap();
                let src: Vec<u8> = (0..COUNT)
                    .flat_map(|i| i.wrapping_mul(0x1357_2461).to_le_bytes())
                    .collect();
                let dst: Vec<u8> = (0..COUNT)
                    .flat_map(|i| (i ^ 0x2468_1357).to_le_bytes())
                    .collect();
                let init = |exec: &mut Executor, count: u32, start_src: u32| {
                    exec.memory_mut().write(SRC, &src).unwrap();
                    exec.memory_mut().write(DST, &dst).unwrap();
                    exec.state.eip = CODE;
                    exec.state.eflags = 0x202;
                    exec.state.regs = [0; 8];
                    exec.state.regs[1] = count;
                    exec.state.regs[6] = start_src;
                    exec.state.regs[7] = DST;
                    exec.state.mxcsr = 0x1f80;
                    exec.state.x87.control = 0x27f;
                    if emms_before {
                        exec.state.x87.status = 0;
                        exec.state.x87.tag = 0xffff;
                    } else {
                        exec.state.x87.status = 5 << 11;
                        exec.state.x87.tag = 0x5555;
                    }
                    exec.state.x87.regs = std::array::from_fn(|n| {
                        adv_cpu_core::X87Register::from_parts(
                            if n == 7 { 0 } else { 0x0040_0040_0040_0040 },
                            0x3fff,
                        )
                    });
                    exec.state.refresh_mmx();
                };
                let (mut executor, mut native) = fixture(&mut a_prog);
                native.features.flagm = flagm;
                executor.memory_mut().map(SRC, len, READ | WRITE).unwrap();
                executor.memory_mut().map(DST, len, READ | WRITE).unwrap();
                init(&mut executor, COUNT, SRC);
                let mut interp = Executor::interpreted_address_space().unwrap();
                interp.interpreter_only = true;
                interp.memory_mut().map(CODE, 0x4000, READ | WRITE).unwrap();
                interp.memory_mut().write(CODE, &words).unwrap();
                interp
                    .memory_mut()
                    .protect(CODE, 0x4000, READ | EXEC)
                    .unwrap();
                interp.memory_mut().map(SRC, len, READ | WRITE).unwrap();
                interp.memory_mut().map(DST, len, READ | WRITE).unwrap();
                init(&mut interp, COUNT, SRC);

                let start = Some(executor.save_context().unwrap());
                native.candidates.push(CODE);
                native.set_computation_offer(true, None);
                let mut cpu = start.as_ref().unwrap().cpu().clone();
                let offer = native
                    .run(
                        &mut cpu,
                        executor.memory_mut(),
                        stop,
                        1 << 16,
                        std::ptr::null_mut(),
                    )
                    .unwrap()
                    .unwrap();
                assert_eq!(offer.exit, Exit::Computation);
                native.install_optimized(true);
                let prepared = &native.computation[&CODE];
                assert!(prepared.code.optimized.is_some());
                let entry = Entry {
                    enter: native.enter,
                    opt_enter: native.opt_enter,
                    window: native.runtime.window,
                    alias: super::super::alias_base(executor.memory()),
                    fault: native.stubs.fault,
                };
                let signals = Signals {
                    recall: Arc::new(AtomicU32::new(0)),
                    pending: (
                        Arc::new(AtomicU32::new(adv_sync::pending::DEADLINE)),
                        adv_sync::pending::DEADLINE,
                    ),
                };
                let mut retired = 0;
                let cut = run(
                    &mut Runtime::new(std::ptr::null_mut(), 0),
                    1,
                    start.clone().unwrap(),
                    entry,
                    (&prepared.code, 0),
                    (&signals, LOCAL),
                    &mut ExecuteScratch::default(),
                )
                .completion;
                assert!(cut.optimized > 0 && cut.retired < u64::from(COUNT) * 13);
                assert_eq!(
                    interp.run(stop, cut.retired).unwrap(),
                    adv_cpu_core::RunOutcome::Budget
                );
                interp.state.refresh_mmx();
                let mid = cut.context.as_ref().unwrap().cpu();
                assert_eq!(
                    (mid.eip, mid.regs, mid.eflags),
                    (interp.state.eip, interp.state.regs, interp.state.eflags)
                );
                assert_eq!(
                    (mid.x87.top(), mid.x87.tag),
                    (interp.state.x87.top(), interp.state.x87.tag)
                );
                assert_eq!(mid.mmx[2..], interp.state.mmx[2..]);
                assert_eq!(mid.x87.regs[2..], interp.state.x87.regs[2..]);
                assert_eq!(
                    executor.memory().read(DST, COUNT as usize * 4).unwrap(),
                    interp.memory().read(DST, COUNT as usize * 4).unwrap()
                );
                let mut rest = None;
                native.with_computation(
                    executor.memory_mut(),
                    &mut retired,
                    (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
                    None,
                    &mut |scope| rest = scope.run_local(2, &cut.context),
                );
                let rest = rest.expect("resumed mmx job");
                assert_eq!(rest.optimized, rest.retired);
                assert_eq!(cut.retired + rest.retired, u64::from(COUNT) * 13);
                assert_eq!(
                    interp.run(stop, 200_000).unwrap(),
                    adv_cpu_core::RunOutcome::ReachedStop
                );
                interp.state.refresh_mmx();
                interp.state.fuel = 0;
                assert_eq!(rest.context.as_ref().unwrap().cpu(), &interp.state);
                assert_eq!(
                    executor.memory().read(DST, COUNT as usize * 4).unwrap(),
                    interp.memory().read(DST, COUNT as usize * 4).unwrap()
                );

                // Case 2: unmapped page-crossing fault on iter 1 vs iter 2+.
                for iter1 in [true, false] {
                    let start_src = if iter1 {
                        SRC + len as u32 - 2
                    } else {
                        SRC + len as u32 - 6
                    };
                    init(&mut executor, 8, start_src);
                    init(&mut interp, 8, start_src);
                    let f_start = executor.save_context().unwrap();
                    let clean_signals = Signals {
                        recall: Arc::new(AtomicU32::new(0)),
                        pending: (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
                    };
                    let fault_entry = Entry {
                        alias: native.runtime.window,
                        ..entry
                    };
                    let done = run(
                        &mut Runtime::new(std::ptr::null_mut(), 0),
                        3,
                        f_start,
                        fault_entry,
                        (&native.computation[&CODE].code, 0),
                        (&clean_signals, LOCAL),
                        &mut ExecuteScratch::default(),
                    );
                    assert_eq!(done.admitted, Some(true));
                    let faulted = done.completion;
                    assert!(!faulted.requires_owner);
                    assert_eq!(faulted.retired, if iter1 { 0 } else { 13 });
                    let outcome = interp.run(stop, 10_000).unwrap();
                    assert!(matches!(
                        outcome,
                        adv_cpu_core::RunOutcome::ReadFault { .. }
                    ));
                    interp.state.refresh_mmx();
                    let fault_cpu = faulted.context.as_ref().unwrap().cpu();
                    assert_eq!(
                        (fault_cpu.eip, fault_cpu.regs, fault_cpu.eflags),
                        (interp.state.eip, interp.state.regs, interp.state.eflags)
                    );
                    assert_eq!(
                        (fault_cpu.x87.top(), fault_cpu.x87.tag),
                        (interp.state.x87.top(), interp.state.x87.tag)
                    );
                    if iter1 {
                        interp.state.fuel = 0;
                        interp.state.fault_addr = 0;
                        assert_eq!(fault_cpu, &interp.state);
                    } else {
                        assert_eq!(fault_cpu.mmx[2..], interp.state.mmx[2..]);
                        assert_eq!(fault_cpu.x87.regs[2..], interp.state.x87.regs[2..]);
                    }
                    assert_eq!(
                        executor.memory().read(DST, 32).unwrap(),
                        interp.memory().read(DST, 32).unwrap()
                    );
                }
            }
        }
    }

    // spec: A64-TR-9
    #[test]
    fn blocks_run_frame_writing_iterations_on_private_frames() {
        let _pool = exclusive_pool();
        fn word(executor: &Executor, addr: u32) -> u32 {
            u32::from_le_bytes(executor.memory().read(addr, 4).unwrap().try_into().unwrap())
        }
        if Pool::get().is_none_or(|pool| pool.limit < 2) {
            return;
        }
        const STACK: u32 = 0x300_0000;
        const ESP_START: u32 = STACK + 0x8000;
        // head: the source pointer lives in an affine frame dword, the sum
        // in a private one, and a push and pop move ESP inside the body.
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.mov(ebx, dword_ptr(esp + 12)).unwrap();
        a.mov(eax, dword_ptr(ebx)).unwrap();
        a.add(dword_ptr(esp + 12), 4).unwrap();
        a.mov(dword_ptr(esp + 8), eax).unwrap();
        a.mov(edx, dword_ptr(esp + 8)).unwrap();
        a.add(edx, dword_ptr(edi)).unwrap();
        a.shr(edx, 1).unwrap();
        a.push(edx).unwrap();
        a.pop(eax).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let len = N as usize * 4;
        let memory = executor.memory_mut();
        memory.map(SRC, len, READ | WRITE).unwrap();
        memory.map(DST, len, READ | WRITE).unwrap();
        memory.map(STACK, 0x1_0000, READ | WRITE).unwrap();
        let src: Vec<u8> = (0..N).flat_map(|i| (i * 7).to_le_bytes()).collect();
        let dst: Vec<u8> = (0..N).flat_map(|i| (i ^ 0x5555).to_le_bytes()).collect();
        memory.write(SRC, &src).unwrap();
        executor.state.eip = CODE;
        executor.state.regs[1] = N;
        executor.state.regs[4] = ESP_START;
        executor.state.regs[7] = DST;
        executor.state.mxcsr = 0x1f80;
        let start = Some(executor.save_context().unwrap());
        let reset = |memory: &mut Memory| {
            memory.write(DST, &dst).unwrap();
            memory.write(STACK, &[0; 0x1_0000]).unwrap();
            memory.write(ESP_START + 12, &SRC.to_le_bytes()).unwrap();
        };
        // The real frame span is [ESP - 4, ESP + 16).
        let check = [(DST, len), (ESP_START - 4, 20)];
        let (_, whole) = whole_then_split(&mut executor, &mut native, &start, stop, reset, &check);
        assert_eq!(whole.cpu().regs[4], ESP_START);
        let last = ((N - 1) * 7 + ((N - 1) ^ 0x5555)) >> 1;
        assert_eq!(word(&executor, DST + 4 * (N - 1)), last);
        assert_eq!(word(&executor, ESP_START + 12), SRC + 4 * N);
        assert_eq!(word(&executor, ESP_START - 4), last);

        // A recalled split stores the prefix's affine dwords in the frame.
        reset(executor.memory_mut());
        let blocks = stopped_blocks(&executor, &native, &start, 3);
        assert!(blocks.plan.frame.is_some());
        let mut prefix = blocks.at(3 * N / 8);
        blocks.settle(prefix.cpu_mut(), None, 3 * N / 8);
        assert_eq!(prefix.cpu().regs[4], ESP_START);
        assert_eq!(word(&executor, ESP_START + 12), SRC + 3 * N / 2);
    }

    // spec: A64-TR-9
    #[test]
    fn private_frames_stay_below_the_words_at_esp() {
        let _pool = exclusive_pool();
        fn word(executor: &Executor, addr: u32) -> u32 {
            u32::from_le_bytes(executor.memory().read(addr, 4).unwrap().try_into().unwrap())
        }
        if Pool::get().is_none_or(|pool| pool.limit < 2) {
            return;
        }
        const STACK: u32 = 0x300_0000;
        const ESP_START: u32 = STACK + 0x8000;
        // Saved registers sit at [ESP, ESP + 16); the loop's frame span,
        // [ESP + 16, ESP + 0x44), is narrower than a stride but ends above it.
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(esp + 16), eax).unwrap();
        a.mov(edx, dword_ptr(esp + 16)).unwrap();
        a.mov(dword_ptr(edi), edx).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.sub(dword_ptr(esp + 0x40), 1).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let len = N as usize * 4;
        let memory = executor.memory_mut();
        memory.map(SRC, len, READ | WRITE).unwrap();
        memory.map(DST, len, READ | WRITE).unwrap();
        memory.map(STACK, 0x1_0000, READ | WRITE).unwrap();
        let src: Vec<u8> = (0..N).flat_map(|i| (i * 7).to_le_bytes()).collect();
        memory.write(SRC, &src).unwrap();
        executor.state.eip = CODE;
        executor.state.regs[4] = ESP_START;
        executor.state.regs[6] = SRC;
        executor.state.regs[7] = DST;
        executor.state.mxcsr = 0x1f80;
        let start = Some(executor.save_context().unwrap());
        let saved: Vec<u8> = (1..=4u32)
            .flat_map(|i| (i * 0x1111_1111).to_le_bytes())
            .collect();
        let reset = |memory: &mut Memory| {
            memory.write(DST, &vec![0; len]).unwrap();
            memory.write(STACK, &[0; 0x1_0000]).unwrap();
            memory.write(ESP_START, &saved).unwrap();
            memory.write(ESP_START + 0x40, &N.to_le_bytes()).unwrap();
        };
        let check = [(DST, len), (ESP_START, 0x44)];
        let (_, whole) = whole_then_split(&mut executor, &mut native, &start, stop, reset, &check);
        assert_eq!(whole.cpu().regs[4], ESP_START);
        assert_eq!(executor.memory().read(ESP_START, 16).unwrap(), saved);
        assert_eq!(word(&executor, DST + 4 * (N - 1)), (N - 1) * 7);
    }

    // spec: A64-CTL-9 A64-TR-9
    #[test]
    fn a_warm_inner_budget_nominates_the_enclosing_loop() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut outer = a.create_label();
        let mut inner = a.create_label();
        // A row longer than RECALL_GRACE budgets.
        const ROW: u32 = (crate::runtime::RECALL_GRACE + 4) * COMPUTATION_SLICE / 4;
        a.nop().unwrap();
        a.set_label(&mut outer).unwrap();
        a.mov(ecx, ROW).unwrap();
        a.set_label(&mut inner).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(inner).unwrap();
        a.dec(ebx).unwrap();
        a.jne(outer).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let memory = executor.memory_mut();
        memory.map(OUTPUT, ROW as usize * 8, READ | WRITE).unwrap();
        let outer = CODE + 1;
        let inner = outer + 5;
        let mut cpu = adv_cpu_core::CpuState {
            eip: CODE,
            ..Default::default()
        };
        cpu.regs[3] = 2;
        cpu.regs[7] = OUTPUT;
        native.first_visits = false;
        let warm = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!((warm.exit, cpu.eip), (Exit::Dispatch, inner));
        native.set_computation_offer(true, None);
        let warm = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!((warm.exit, cpu.eip), (Exit::Dispatch, inner));
        assert_eq!(native.nominated.map(|(head, _)| head), Some(outer));
        native.candidates.push(inner);
        cpu.eip = outer;
        let offer = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(offer.exit, Exit::Computation);
        assert!(!native.candidates.contains(&inner));

        // A recall waits RECALL_GRACE budgets for the head, then publishes an
        // interior PC. Resume the prefix on the owner and offer only at the
        // original proved head.
        let mut context = executor.save_context().unwrap();
        cpu.regs[3] = 2;
        cpu.regs[7] = OUTPUT;
        context.cpu_mut().clone_from(&cpu);
        native.cache.sync();
        let recalled = run(
            &mut Runtime::new(std::ptr::null_mut(), 0),
            1,
            context,
            Entry {
                enter: native.enter,
                opt_enter: native.opt_enter,
                window: native.runtime.window,
                alias: super::super::alias_base(executor.memory()),
                fault: native.stubs.fault,
            },
            (&native.computation[&outer].code, 0),
            (
                &Signals {
                    recall: Arc::new(AtomicU32::new(RECALL)),
                    pending: (Arc::new(AtomicU32::new(0)), 0),
                },
                RECALL,
            ),
            &mut ExecuteScratch::default(),
        )
        .completion;
        assert!(recalled.requires_owner);
        cpu = recalled.context.unwrap().cpu().clone();
        assert_eq!(cpu.eip, inner);
        native.set_computation_offer(true, Some(cpu.eip));
        let mut serial = 0;
        for _ in 0..8 {
            let result = native
                .run(
                    &mut cpu,
                    executor.memory_mut(),
                    stop,
                    1 << 16,
                    std::ptr::null_mut(),
                )
                .unwrap()
                .unwrap();
            if result.exit == Exit::Computation {
                assert_eq!(cpu.eip, outer);
                assert!(serial > 0);
                return;
            }
            assert_eq!(result.exit, Exit::Dispatch);
            serial += result.retired;
        }
        panic!("serial continuation did not offer the prepared head");
    }

    // spec: A64-OPT-1
    #[test]
    fn page_grant_cache_invalidates_on_protection_and_watch_code() {
        let _pool = exclusive_pool();
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        let mut loop_head = a.create_label();
        a.set_label(&mut loop_head).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(loop_head).unwrap();
        let head = CODE + 1;
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        {
            let memory = executor.memory_mut();
            memory.map(INPUT, 0x2_0000, READ | WRITE).unwrap();
            memory.map(OUTPUT, 0x2_0000, READ | WRITE).unwrap();
        }
        native.first_visits = false;
        native.set_computation_offer(true, None);
        let mut init_regs = [0u32; 8];
        init_regs[1] = 20_000;
        init_regs[6] = INPUT;
        init_regs[7] = OUTPUT;
        let mut cpu = CpuState {
            eip: CODE,
            regs: init_regs,
            ..Default::default()
        };
        for _ in 0..500 {
            if native.diagnostics.optimized_runs > 0 {
                break;
            }
            if cpu.eip == stop {
                cpu.eip = CODE;
                cpu.regs[1] = 20_000;
                cpu.regs[6] = INPUT;
                cpu.regs[7] = OUTPUT;
            }
            let _ = alone(&mut native, &mut executor, &mut cpu, stop, 1 << 16);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            native.diagnostics.optimized_runs > 0,
            "loop did not promote and run optimized"
        );
        let prepared = &native.computation[&head];
        let memory = executor.memory();
        let alias = super::alias_base(memory);

        // Initial check and sliding pointers inside the first page and across
        // the second page must all be admitted via PageGrants.
        cpu.eip = head;
        cpu.regs[1] = 4;
        cpu.regs[6] = INPUT;
        cpu.regs[7] = OUTPUT;
        assert!(native.admission.alone(&prepared.proof, &cpu, memory, alias));
        cpu.regs[6] = INPUT + 64;
        cpu.regs[7] = OUTPUT + 64;
        assert!(native.admission.alone(&prepared.proof, &cpu, memory, alias));
        cpu.regs[6] = INPUT + 0x1000;
        cpu.regs[7] = OUTPUT + 0x1000;
        assert!(native.admission.alone(&prepared.proof, &cpu, memory, alias));

        // Removing read permission on the cached first input page invalidates it.
        executor.memory_mut().protect(INPUT, 4096, 0).unwrap();
        cpu.regs[6] = INPUT + 64;
        cpu.regs[7] = OUTPUT + 64;
        assert!(
            !native
                .admission
                .alone(&prepared.proof, &cpu, executor.memory(), alias)
        );

        // Restoring permissions re-admits; watching code inside the output page
        // rejects writes into the watched line while respecting exact ranges.
        executor
            .memory_mut()
            .protect(INPUT, 4096, READ | WRITE)
            .unwrap();
        assert!(
            native
                .admission
                .alone(&prepared.proof, &cpu, executor.memory(), alias)
        );

        executor.memory_mut().watch_code(OUTPUT + 128, 64);
        cpu.regs[6] = INPUT + 128;
        cpu.regs[7] = OUTPUT + 128;
        assert!(
            !native
                .admission
                .alone(&prepared.proof, &cpu, executor.memory(), alias)
        );
    }

    // spec: A64-OPT-1 A64-OPT-5
    #[test]
    fn shadow_caller_absorbs_distant_inner_candidate() {
        let _pool = exclusive_pool();
        let outer = CODE;
        let inner = CODE + 0x6000;
        let src = 0x10_0000u32;
        let dst = 0x20_0000u32;
        let stack_base = 0x30_0000u32;
        let stack = stack_base + 0x2000;

        let mut oa = CodeAssembler::new(32).unwrap();
        let mut row_loop = oa.create_label();
        oa.set_label(&mut row_loop).unwrap();
        oa.push(ecx).unwrap();
        oa.push(esi).unwrap();
        oa.push(edi).unwrap();
        oa.mov(ecx, 16).unwrap();
        oa.call(u64::from(inner)).unwrap();
        oa.pop(edi).unwrap();
        oa.pop(esi).unwrap();
        oa.pop(ecx).unwrap();
        oa.add(esi, 64).unwrap();
        oa.add(edi, 64).unwrap();
        oa.dec(ecx).unwrap();
        oa.jne(row_loop).unwrap();
        let outer_code = oa.assemble(u64::from(outer)).unwrap();
        let stop = outer + outer_code.len() as u32;
        oa.int3().unwrap();
        let outer_with_stop = oa.assemble(u64::from(outer)).unwrap();

        let mut ia = CodeAssembler::new(32).unwrap();
        let mut inner_loop = ia.create_label();
        ia.set_label(&mut inner_loop).unwrap();
        ia.mov(eax, dword_ptr(esi)).unwrap();
        ia.add(eax, 3).unwrap();
        ia.mov(dword_ptr(edi), eax).unwrap();
        ia.add(esi, 4).unwrap();
        ia.add(edi, 4).unwrap();
        ia.dec(ecx).unwrap();
        ia.jne(inner_loop).unwrap();
        ia.ret().unwrap();
        let inner_code = ia.assemble(u64::from(inner)).unwrap();

        let mut executor = Executor::interpreted_address_space().unwrap();
        {
            let memory = executor.memory_mut();
            memory.map(outer, 0x8000, READ | WRITE).unwrap();
            memory.write(outer, &outer_with_stop).unwrap();
            memory.write(inner, &inner_code).unwrap();
            memory.protect(outer, 0x8000, READ | EXEC).unwrap();
            memory.map(src, 0x4_0000, READ | WRITE).unwrap();
            memory.map(dst, 0x4_0000, READ | WRITE).unwrap();
            memory.map(stack_base, 0x4000, READ | WRITE).unwrap();
            for i in 0..0x1000u32 {
                let addr = src + i * 4;
                memory.write(addr, &i.to_le_bytes()).unwrap();
            }
        }
        let mut native = Native::new(executor.memory().window_base().unwrap()).unwrap();
        native.first_visits = false;
        native.set_computation_offer(true, None);
        native.promotion = 1;

        // First promote `inner` via budget exits returning to `stop` so `ret_pc` is never a stop.
        let mut cpu = CpuState::default();
        for _ in 0..8 {
            executor
                .memory_mut()
                .write(stack, &stop.to_le_bytes())
                .unwrap();
            cpu.eip = inner;
            cpu.regs[1] = 20_000;
            cpu.regs[4] = stack;
            cpu.regs[6] = src;
            cpu.regs[7] = dst;
            let _ = alone(&mut native, &mut executor, &mut cpu, stop, 1 << 20);
        }
        for _ in 0..200 {
            executor
                .memory_mut()
                .write(stack, &stop.to_le_bytes())
                .unwrap();
            cpu.eip = inner;
            cpu.regs[1] = 16;
            cpu.regs[4] = stack;
            cpu.regs[6] = src;
            cpu.regs[7] = dst;
            let _ = alone(&mut native, &mut executor, &mut cpu, stop, 1 << 20);
            if native.diagnostics.optimized_runs > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            native.candidates.contains(&inner),
            "inner loop did not become a candidate"
        );

        // Now call `outer` repeatedly with short row counts (< SLICE); shadow caller
        // sampling must discover `outer`, absorb `inner`, and retire `inner` as a candidate.
        let run_to_stop = |native: &mut Native, executor: &mut Executor, cpu: &mut CpuState| {
            let mut steps = 0usize;
            while cpu.eip != stop {
                steps += 1;
                assert!(steps < 500, "too many dispatches before stop");
                let _ = alone(native, executor, cpu, stop, 1 << 20);
            }
            steps
        };
        for _ in 0..200 {
            cpu.eip = outer;
            cpu.regs[1] = 32;
            cpu.regs[4] = stack;
            cpu.regs[6] = src;
            cpu.regs[7] = dst;
            run_to_stop(&mut native, &mut executor, &mut cpu);
            assert_eq!(cpu.eip, stop);
            if native.candidates.contains(&outer) && !native.candidates.contains(&inner) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            native.candidates.contains(&outer),
            "outer caller across >4KiB gap was not promoted"
        );
        assert!(
            !native.candidates.contains(&inner),
            "absorbed inner loop remained a separate candidate"
        );

        // Execute the promoted outer loop cleanly and verify it runs to `stop`
        // in one optimized dispatch with exact output memory.
        executor
            .memory_mut()
            .write(dst, &vec![0u8; 32 * 64])
            .unwrap();
        let before_runs = native.diagnostics.optimized_runs;
        cpu.eip = outer;
        cpu.regs[1] = 32;
        cpu.regs[4] = stack;
        cpu.regs[6] = src;
        cpu.regs[7] = dst;
        let steps = run_to_stop(&mut native, &mut executor, &mut cpu);
        assert_eq!(steps, 1);
        assert_eq!(cpu.eip, stop);
        assert!(native.diagnostics.optimized_runs > before_runs);
        for row in 0..32u32 {
            for col in 0..16u32 {
                let off = row * 64 + col * 4;
                let bytes = executor.memory().read(dst + off, 4).unwrap();
                let got = u32::from_le_bytes(bytes.try_into().unwrap());
                let expected = (off / 4) + 3;
                assert_eq!(got, expected, "row={row} col={col}");
            }
        }
    }

    /// No peer runs beside this context.
    struct Alone;

    impl adv_cpu_core::trap::ExecutionTrap for Alone {
        fn allow_computation(&mut self) -> bool {
            false
        }

        /// Never offering, it publishes a clear gate, so head entry blocks
        /// keep arrivals in translated code (A64-OPT-8).
        fn offer_gate(&self) -> Option<&adv_cpu_core::backend::OfferGate> {
            Some(&super::super::CLEAR_GATE)
        }

        fn call(&mut self, _: &mut CpuState, _: &mut Memory, _: u32) -> Result<bool, String> {
            unreachable!("no host calls")
        }
    }

    /// A copy loop of COUNT words from INPUT, entered at CODE with its
    /// head at CODE + 1, and the address after it.
    fn copy_loop() -> (Executor, Native, u32) {
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        let mut loop_head = a.create_label();
        a.set_label(&mut loop_head).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(loop_head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let memory = executor.memory_mut();
        memory.map(INPUT, 0x4000, READ | WRITE).unwrap();
        memory.write(INPUT, &0x1234_5678u32.to_le_bytes()).unwrap();
        memory.map(OUTPUT, SPACE as usize, READ | WRITE).unwrap();
        native.first_visits = false;
        native.set_computation_offer(true, None);
        (executor, native, stop)
    }

    fn start(cpu: &mut CpuState, eip: u32) {
        cpu.eip = eip;
        cpu.regs[1] = COUNT;
        cpu.regs[6] = INPUT;
        cpu.regs[7] = OUTPUT;
    }

    /// One owner run of the copy loop with no peer beside it.
    fn alone(
        native: &mut Native,
        executor: &mut Executor,
        cpu: &mut CpuState,
        stop: u32,
        fuel: u64,
    ) -> u32 {
        let mut handler = Alone;
        let memory = executor.memory_mut();
        let mut bridge = adv_cpu_core::trap::Bridge {
            timestamp: Default::default(),
            memory,
            error: None,
            trap: None,
            calls: 0,
            handler: Some(&mut handler),
            parked: false,
            deadline: None,
            clock_due: true,
            unclocked_bytes: 0,
            bulk_calls: 0,
            bulk_bytes: 0,
            cliffs: Default::default(),
            scratch: Default::default(),
        };
        let run = native
            .run(cpu, memory, stop, fuel, (&raw mut bridge).cast())
            .unwrap()
            .unwrap();
        assert_ne!(run.exit, Exit::Computation);
        assert!(u64::from(run.retired) <= fuel);
        run.retired
    }

    // spec: A64-OPT-1
    #[test]
    fn a_computation_without_a_computing_peer_runs_optimized_on_the_owner() {
        let (mut executor, mut native, stop) = copy_loop();
        let head = CODE + 1;
        let mut cpu = CpuState::default();
        let mut run = |native: &mut Native, cpu: &mut CpuState, fuel| {
            alone(native, &mut executor, cpu, stop, fuel)
        };

        // Budget exits nominate the loop and the next entry proves it.
        start(&mut cpu, CODE);
        while native.diagnostics.optimized_runs == 0 {
            assert_ne!(cpu.eip, stop, "the loop never ran optimized");
            run(&mut native, &mut cpu, 1 << 16);
        }
        assert!(native.computation[&head].code.optimized.is_some());

        // Fuel just above one budget: each run leaves at the first refill,
        // and the runs retire the loop exactly.
        start(&mut cpu, CODE);
        let before = native.diagnostics.optimized_runs;
        let mut retired = 0;
        while cpu.eip != stop {
            retired += run(&mut native, &mut cpu, (1 << 16) + 1000);
        }
        assert_eq!(retired, 1 + 5 * COUNT);
        assert!(native.diagnostics.optimized_runs > before + 1);

        // Unbounded fuel runs the whole loop at once; pending owner work
        // leaves at a checkpoint.
        start(&mut cpu, head);
        assert_eq!(run(&mut native, &mut cpu, 1 << 40), 5 * COUNT);
        assert_eq!(cpu.eip, stop);
        native.interrupt = Some(Arc::new(AtomicU32::new(adv_sync::pending::DEADLINE)));
        start(&mut cpu, head);
        assert!(run(&mut native, &mut cpu, 1 << 40) < 5 * COUNT);
        assert_ne!(cpu.eip, stop);
        assert_eq!(
            executor.memory().read(OUTPUT + 4 * (COUNT - 1), 4).unwrap(),
            [0x78, 0x56, 0x34, 0x12]
        );
    }

    // spec: A64-OPT-8
    #[test]
    fn translated_arrivals_enter_the_inline_form_through_the_head_block() {
        const INNER: u32 = 300;
        const OUTER: u32 = 400;
        // outer: three moves, the copy loop at `head`, then dec ebx; jne.
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        let mut outer = a.create_label();
        let mut head = a.create_label();
        a.set_label(&mut outer).unwrap();
        a.mov(ecx, INNER).unwrap();
        a.mov(esi, INPUT).unwrap();
        a.mov(edi, OUTPUT).unwrap();
        a.set_label(&mut head).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, ebx).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.dec(ebx).unwrap();
        a.jne(outer).unwrap();
        a.int3().unwrap();
        let code = a.assemble(u64::from(CODE)).unwrap();
        let stop = CODE + code.len() as u32 - 1;
        // nop, three 5-byte moves.
        let head = CODE + 16;
        let (mut executor, mut native) = fixture(&mut a);
        let memory = executor.memory_mut();
        memory.map(INPUT, 0x4000, READ | WRITE).unwrap();
        let input: Vec<u8> = (0..4 * INNER).map(|i| (i * 7) as u8).collect();
        memory.write(INPUT, &input).unwrap();
        memory.map(OUTPUT, 0x4000, READ | WRITE).unwrap();
        native.first_visits = false;
        native.candidates.push(head);
        native.set_computation_offer(true, None);
        let begin = |cpu: &mut CpuState| {
            *cpu = CpuState::default();
            cpu.eip = CODE;
            cpu.regs[3] = OUTER;
        };
        let mut cpu = CpuState::default();
        begin(&mut cpu);
        for _ in 0..100 {
            if native
                .computation
                .get(&head)
                .is_some_and(|prepared| prepared.inline.is_some())
            {
                break;
            }
            alone(&mut native, &mut executor, &mut cpu, stop, 1 << 16);
        }
        let heads: Vec<_> = native.computation.keys().collect();
        assert!(
            native
                .computation
                .get(&head)
                .is_some_and(|p| p.inline.is_some()),
            "no inline form at {head:#x}: {heads:x?}"
        );
        let check = |executor: &Executor, cpu: &CpuState| {
            assert_eq!(cpu.eip, stop);
            assert_eq!(cpu.regs[1], 0);
            assert_eq!(cpu.regs[3], 0);
            assert_eq!(cpu.regs[6], INPUT + 4 * INNER);
            assert_eq!(cpu.regs[7], OUTPUT + 4 * INNER);
            // The last outer pass adds ebx = 1 to each input word.
            let output = executor.memory().read(OUTPUT, 4 * INNER as usize).unwrap();
            for (i, word) in output.chunks(4).enumerate() {
                let x = u32::from_le_bytes(input[4 * i..4 * i + 4].try_into().unwrap());
                assert_eq!(word, (x + 1).to_le_bytes(), "word {i}");
            }
        };
        let expected = u64::from(1 + OUTER * (3 + 7 * INNER + 2));

        // Ample fuel: arrivals from the outer loop stay in translated code.
        let slot = &native.computation[&head].slot;
        let (runs, rust) = (slot.runs.get(), native.diagnostics.optimized_runs);
        begin(&mut cpu);
        let mut retired = 0;
        let mut owner_runs = 0;
        while cpu.eip != stop {
            retired += u64::from(alone(&mut native, &mut executor, &mut cpu, stop, 1 << 20));
            owner_runs += 1;
        }
        assert_eq!(retired, expected);
        check(&executor, &cpu);
        let entries = native.computation[&head].slot.runs.get() - runs;
        assert!(entries >= OUTER - owner_runs, "{entries} block entries");
        assert!(native.diagnostics.optimized_runs - rust < u64::from(owner_runs) + 4);

        // Fuel below the loop's length: refills stop at the fuel, cold
        // exits rebuild the state, and the runs still retire it exactly.
        let cold = native.diagnostics.optimized_runs;
        begin(&mut cpu);
        let mut retired = 0;
        while cpu.eip != stop {
            let fuel = 700 + retired % 389;
            retired += u64::from(alone(&mut native, &mut executor, &mut cpu, stop, fuel));
        }
        assert_eq!(retired, expected);
        check(&executor, &cpu);
        assert!(native.diagnostics.optimized_runs > cold + 10);
    }

    // spec: A64-OPT-1
    #[test]
    fn background_forms_install_at_a_later_entry_unless_stale() {
        let (mut executor, mut native, stop) = copy_loop();
        native.compiler = super::super::compile::Compiler::background();
        let head = CODE + 1;
        let mut cpu = CpuState::default();
        start(&mut cpu, CODE);
        while !native.computation.contains_key(&head) {
            assert_ne!(cpu.eip, stop, "the loop was never proved");
            alone(&mut native, &mut executor, &mut cpu, stop, 1 << 16);
        }
        native.install_optimized(true);
        assert!(native.computation[&head].code.optimized.is_some());

        // A form requested for a unit that is dropped before it installs
        // takes no code space.
        native.request_optimized(head, cpu.clone());
        native.cut(head, head + 1);
        let used = native.cache.used();
        native.install_optimized(true);
        assert!(!native.computation.contains_key(&head));
        assert_eq!(native.cache.used(), used);

        // The loop proves again, and runs optimized once its form installs.
        start(&mut cpu, CODE);
        let mut retired = 0;
        while cpu.eip != stop {
            retired += alone(&mut native, &mut executor, &mut cpu, stop, 1 << 16);
        }
        assert_eq!(retired, 1 + 5 * COUNT);
        native.install_optimized(true);
        start(&mut cpu, head);
        let before = native.diagnostics.optimized_runs;
        assert_eq!(
            alone(&mut native, &mut executor, &mut cpu, stop, 1 << 40),
            5 * COUNT
        );
        assert_eq!(native.diagnostics.optimized_runs, before + 1);
    }

    // spec: A64-OPT-1
    #[test]
    fn entry_states_the_form_keeps_missing_build_at_most_two_more() {
        let (mut executor, mut native, stop) = copy_loop();
        let head = CODE + 1;
        let mut cpu = CpuState::default();
        start(&mut cpu, CODE);
        while native.diagnostics.optimized_runs == 0 {
            alone(&mut native, &mut executor, &mut cpu, stop, 1 << 16);
        }
        // Every form assumes MXCSR's control bits: each new rounding mode
        // misses MISSES entries, then the next entry builds for it.
        for (mode, rebuilds) in [(1, true), (2, true), (3, false)] {
            cpu.mxcsr = 0x1f80 | mode << 13;
            for _ in 0..MISSES {
                let before = native.diagnostics.optimized_runs;
                start(&mut cpu, head);
                alone(&mut native, &mut executor, &mut cpu, stop, 1 << 16);
                assert_eq!(native.diagnostics.optimized_runs, before);
            }
            let before = native.diagnostics.optimized_runs;
            start(&mut cpu, head);
            alone(&mut native, &mut executor, &mut cpu, stop, 1 << 16);
            assert_eq!(native.diagnostics.optimized_runs > before, rebuilds);
        }
        assert_eq!(native.computation[&head].forms.get(), 1 + RESPECIALIZE);
    }

    // spec: A64-CTL-10 A64-TR-9
    #[test]
    fn changing_the_middle_code_page_revokes_the_prepared_loop() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        let nop = [0x66, 0x0f, 0x1f, 0x84, 0, 0, 0, 0, 0];
        for _ in 0..930 {
            a.db(&nop).unwrap();
        }
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        assert!((CODE + 8192..CODE + 12288).contains(&stop));
        let (mut executor, mut native) = fixture(&mut a);
        let context = state(&mut executor, CODE, OUTPUT, 2);
        let memory = executor.memory_mut();
        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        let mut cpu = context.as_ref().unwrap().cpu().clone();
        let result = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(result.exit, Exit::Computation);
        assert!(native.unit_address(CODE).is_some());
        assert!(native.computation.contains_key(&CODE));
        let middle = CODE + 4096u32.div_ceil(nop.len() as u32) * nop.len() as u32;
        memory.write(middle, &[0xcc]).unwrap();
        native.invalidate(memory);
        assert!(native.unit_address(CODE).is_none());
        assert!(!native.computation.contains_key(&CODE));
    }

    // spec: A64-CTL-4 A64-CTL-6 A64-CTL-9
    #[test]
    fn an_inlined_return_mismatch_exits_with_the_committed_transfer() {
        const TARGET: u32 = CODE + 0x80;
        const STACK: u32 = OUTPUT + 0x5000;
        let mut a = CodeAssembler::new(32).unwrap();
        let mut callee = a.create_label();
        let mut again = a.create_label();
        let mut end = a.create_label();
        a.call(callee).unwrap();
        a.set_label(&mut again).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.dec(ecx).unwrap();
        a.jne(again).unwrap();
        a.jmp(end).unwrap();
        a.set_label(&mut callee).unwrap();
        a.mov(dword_ptr(esp), TARGET).unwrap();
        a.mov(eax, 17).unwrap();
        a.ret().unwrap();
        a.set_label(&mut end).unwrap();
        a.int3().unwrap();
        let ret = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 2;

        let (mut executor, mut native) = fixture(&mut a);
        executor
            .memory_mut()
            .map(OUTPUT, 0x8000, READ | WRITE)
            .unwrap();
        let mut initial = state(&mut executor, CODE, OUTPUT, 2).unwrap();
        initial.cpu_mut().regs[4] = STACK;
        let mut serial = initial.cpu().clone();
        let memory = executor.memory_mut();
        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        assert_eq!(
            native
                .run(&mut serial, memory, TARGET, 64, std::ptr::null_mut())
                .unwrap()
                .unwrap()
                .exit,
            Exit::Computation
        );
        native.set_computation_offer(false, None);
        let owner = native
            .run(&mut serial, memory, TARGET, 64, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!((owner.exit, owner.retired), (Exit::Dispatch, 4));
        assert_eq!(
            (serial.eip, serial.regs[4], serial.regs[0]),
            (TARGET, STACK, 17)
        );
        assert_eq!(
            serial.last_indirect_branch,
            adv_cpu_core::IndirectBranch {
                source: ret,
                target: TARGET,
                kind: adv_cpu_core::IndirectBranchKind::Return,
            }
        );
        memory.write(STACK - 4, &[0; 4]).unwrap();

        let dispatches = native.diagnostics.dispatches;
        let mut retired = 0;
        let mut done = None;
        let context = Some(initial);
        native.with_computation(
            memory,
            &mut retired,
            (Arc::new(AtomicU32::new(0)), 0),
            None,
            &mut |jobs| done = jobs.run_local(1, &context),
        );
        let done = done.expect("admitted inline call");
        assert!(done.requires_owner);
        assert_eq!((done.retired, retired), (4, 4));
        assert_eq!(done.context.unwrap().cpu(), &serial);
        assert_eq!(native.diagnostics.dispatches, dispatches);
        assert_eq!(memory.read(STACK - 4, 4).unwrap(), TARGET.to_le_bytes());
        assert_eq!(memory.read(OUTPUT, 4).unwrap(), [0; 4]);
    }

    // spec: A64-MEM-5 A64-MEM-9
    #[test]
    fn private_fault_capture_preserves_the_exact_prefix() {
        let _pool = exclusive_pool();
        let mut a = CodeAssembler::new(32).unwrap();
        a.mov(eax, 42).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.mov(ecx, dword_ptr(esi)).unwrap();
        a.int3().unwrap();
        let (mut executor, mut native) = fixture(&mut a);
        executor
            .memory_mut()
            .map(OUTPUT, 0x4000, READ | WRITE)
            .unwrap();
        let mut work = Workspace::default();
        work.unit.build(
            executor.memory_mut(),
            CODE,
            &[],
            &|_, _| None,
            &|_| None,
            true,
        );
        let at = native.cache.next();
        let mut scratch = translate::Code::default();
        let emitted = translate::lower_computation(
            &mut work.unit,
            &mut scratch,
            CODE,
            native.features,
            at,
            native.stubs,
            (0x37f, false, None),
        )
        .unwrap();
        assert_eq!(native.cache.append(emitted.words), Some(at));
        native.cache.sync();
        let code = Code {
            entry: at,
            end: at + emitted.words.len() * 4,
            sites: emitted
                .sites
                .iter()
                .map(|&(offset, site)| (at + offset, site))
                .collect(),
            optimized: None,
        };
        let entry = Entry {
            enter: native.enter,
            opt_enter: native.opt_enter,
            window: native.runtime.window,
            // Point this fixture at the protected view to inject a fault.
            alias: native.runtime.window,
            fault: native.stubs.fault,
        };
        let first = state(&mut executor, CODE, OUTPUT, 31).unwrap();
        let mut second = state(&mut executor, CODE, OUTPUT + 4, 57).unwrap();
        second.cpu_mut().regs[6] += 0x4000;
        let (send, recv) = sync_channel(2);
        let signals = Signals {
            recall: Arc::new(AtomicU32::new(0)),
            pending: (Arc::new(AtomicU32::new(0)), 0),
        };
        let pool = Pool::get().expect("resident pool");
        // Inject an inaccessible read to exercise recovery. This does not
        // change any mapping during either run; the output bytes are disjoint.
        assert!(pool.submit(Job::Whole(Box::new(Task {
            id: 1,
            context: first,
            entry,
            code: code.clone(),
            once: false,
            signals: signals.clone(),
            done: send,
            wake: None,
        }))));
        let local = run(
            &mut Runtime::new(std::ptr::null_mut(), 0),
            2,
            second,
            entry,
            (&code, 0),
            (&signals, LOCAL),
            &mut ExecuteScratch::default(),
        )
        .completion;
        for result in [
            local,
            recv.recv_timeout(Duration::from_secs(5))
                .unwrap()
                .completion,
        ] {
            assert!(result.requires_owner);
            assert_eq!(result.retired, 2);
            let context = result.context.unwrap();
            assert!(context.has_resume());
            assert_eq!(context.cpu().eip, CODE + 7);
            assert_eq!(context.cpu().regs[0], 42);
            assert_eq!(context.cpu().regs[1], if result.id == 1 { 31 } else { 57 });
            executor.restore_context(&context).unwrap();
            assert_eq!(
                executor.run(0, 16).unwrap(),
                adv_cpu_core::RunOutcome::ReadFault {
                    pc: CODE + 7,
                    address: if result.id == 1 {
                        INPUT
                    } else {
                        INPUT + 0x4000
                    },
                }
            );
        }
        assert_eq!(
            executor.memory().read(OUTPUT, 8).unwrap(),
            [42, 0, 0, 0, 42, 0, 0, 0]
        );
    }

    // spec: A64-CTL-7 A64-CTL-8 A64-TR-9 A64F-FP-7
    #[test]
    fn outer_iteration_returns_canonical_state_and_can_be_readmitted() {
        const INNER: u32 = 20_000;
        const ROWS: u32 = 3;
        let mut a = CodeAssembler::new(32).unwrap();
        let mut outer = a.create_label();
        let mut inner = a.create_label();
        a.set_label(&mut outer).unwrap();
        a.mov(ecx, INNER).unwrap();
        a.set_label(&mut inner).unwrap();
        a.fadd_2(st0, st1).unwrap();
        a.fst(dword_ptr(edi)).unwrap();
        a.paddd(xmm0, xmm1).unwrap();
        a.movd(dword_ptr(edi + 4), xmm0).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jne(inner).unwrap();
        a.dec(ebx).unwrap();
        a.jne(outer).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        let len = (INNER * ROWS * 8) as usize;
        executor
            .memory_mut()
            .map(OUTPUT, 0x80000, READ | WRITE)
            .unwrap();
        let mut start = state(&mut executor, CODE, OUTPUT, 0).unwrap();
        let cpu = start.cpu_mut();
        cpu.regs[3] = ROWS;
        cpu.x87.control = 0x037f;
        cpu.x87.tag = 0xfff0;
        cpu.x87.regs[0] = adv_cpu_core::X87Register::from_parts(1 << 63, 0x3fff);
        cpu.x87.regs[1] = cpu.x87.regs[0];
        cpu.xmm[1] = [3; 16];
        cpu.eflags = 0x8d7;
        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        assert_eq!(
            native
                .run(
                    &mut start.cpu().clone(),
                    executor.memory_mut(),
                    stop,
                    1 << 16,
                    std::ptr::null_mut(),
                )
                .unwrap()
                .unwrap()
                .exit,
            Exit::Computation,
        );
        native.cache.sync();

        // A separate ordinary translator stops at the same original head.
        let mut owner = Native::new(native.runtime.window as usize).unwrap();
        owner.first_visits = false;
        owner.observe_stop(stop);
        let mut serial = start.clone();
        let mut first = None;
        let mut total = 0;
        loop {
            let result = owner
                .run(
                    serial.cpu_mut(),
                    executor.memory_mut(),
                    CODE,
                    1 << 16,
                    std::ptr::null_mut(),
                )
                .unwrap()
                .unwrap();
            assert_eq!(result.exit, Exit::Dispatch);
            total += u64::from(result.retired);
            if serial.cpu().eip == CODE {
                serial.cpu_mut().refresh_mmx();
                first.get_or_insert_with(|| (serial.clone(), total));
            } else if serial.cpu().eip == stop {
                break;
            }
        }
        serial.cpu_mut().refresh_mmx();
        let (expected_first, first_retired) = first.unwrap();
        assert_eq!(first_retired, u64::from(INNER) * 7 + 3);
        assert_eq!(total, first_retired * u64::from(ROWS));
        let expected = executor.memory().read(OUTPUT, len).unwrap().to_vec();
        executor.memory_mut().fill(OUTPUT, &[0], len).unwrap();

        // Only actual deadline recalls activate the alternate fixed form.
        let entry = Entry {
            enter: native.enter,
            opt_enter: native.opt_enter,
            window: native.runtime.window,
            alias: super::super::alias_base(executor.memory()),
            fault: native.stubs.fault,
        };
        let prepared = &native.computation[&CODE];
        let pending = Signals {
            recall: Arc::new(AtomicU32::new(0)),
            pending: (
                Arc::new(AtomicU32::new(adv_sync::pending::DEADLINE)),
                adv_sync::pending::DEADLINE,
            ),
        };
        for _ in 0..2 {
            let done = run(
                &mut Runtime::new(std::ptr::null_mut(), 0),
                1,
                start.clone(),
                entry,
                (&prepared.code, 0),
                (&pending, RECALL),
                &mut ExecuteScratch::default(),
            );
            assert!(done.deadline_recall);
            prepared.observe(CODE, false, &done);
        }
        assert!(prepared.mode.get() == Mode::Trial);

        // A recalled iteration still finishes at its head (A64-CTL-8).
        let recalled = Signals {
            recall: Arc::new(AtomicU32::new(RECALL)),
            pending: (Arc::new(AtomicU32::new(0)), 0),
        };
        let done = run(
            &mut Runtime::new(std::ptr::null_mut(), 0),
            1,
            start.clone(),
            entry,
            (&prepared.code, 1),
            (&recalled, RECALL),
            &mut ExecuteScratch::default(),
        )
        .completion;
        assert!(!done.requires_owner);
        assert_eq!(done.retired, first_retired);
        assert_eq!(done.context.unwrap().cpu(), expected_first.cpu());
        executor.memory_mut().fill(OUTPUT, &[0], len).unwrap();

        let mut context = Some(start);
        let mut retired = 0;
        native.with_computation(
            executor.memory_mut(),
            &mut retired,
            (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
            None,
            &mut |scope| {
                let done = scope.run_local(1, &context).expect("outer iteration");
                assert!(!done.requires_owner);
                assert_eq!(done.retired, first_retired);
                assert_eq!(done.context.as_ref().unwrap().cpu(), expected_first.cpu());
                context = done.context;
                while context.as_ref().unwrap().cpu().eip != stop {
                    let done = scope.run_local(1, &context).expect("fresh root admission");
                    assert!(!done.requires_owner);
                    context = done.context;
                }
            },
        );
        assert_eq!(retired, total);
        assert_eq!(context.as_ref().unwrap().cpu(), serial.cpu());
        assert_eq!(executor.memory().read(OUTPUT, len).unwrap(), expected);

        // A short row disables only that form, even if old whole-job
        // completions arrive after the trial has been selected.
        let prepared = &native.computation[&CODE];
        prepared.mode.set(Mode::Trial);
        let mut sample = Done {
            completion: Completion {
                id: 1,
                context: Some(expected_first),
                retired: first_retired,
                optimized: 0,
                elapsed: Duration::from_micros(10),
                helpers: Duration::ZERO,
                requires_owner: false,
            },
            deadline_recall: false,
            admitted: None,
        };
        prepared.observe(CODE, false, &sample);
        assert!(prepared.mode.get() == Mode::Trial);
        prepared.observe(CODE, true, &sample);
        assert!(prepared.mode.get() == Mode::WholeOnly);
        assert!(prepared.worth_offering());
        prepared.mode.set(Mode::Trial);
        sample.completion.elapsed = Duration::from_micros(100);
        prepared.observe(CODE, true, &sample);
        assert!(prepared.mode.get() == Mode::Iteration);
    }

    // spec: A64-CTL-7 A64-CTL-8 A64-TR-9 A64F-FP-7
    #[test]
    fn floating_and_integer_state_agree_after_refills_and_recall() {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.fadd_2(st0, st1).unwrap();
        a.fst(dword_ptr(edi)).unwrap();
        a.paddd(xmm0, xmm1).unwrap();
        a.movd(dword_ptr(edi + 4), xmm0).unwrap();
        a.add(edi, 8).unwrap();
        a.dec(ecx).unwrap();
        a.jne(head).unwrap();
        a.int3().unwrap();
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let (mut executor, mut native) = fixture(&mut a);
        executor
            .memory_mut()
            .map(OUTPUT, SPACE as usize * 2, READ | WRITE)
            .unwrap();
        let mut start = state(&mut executor, CODE, OUTPUT, COUNT).unwrap();
        let cpu = start.cpu_mut();
        cpu.x87.control = 0x037f;
        cpu.x87.tag = 0xfff0;
        cpu.x87.regs[0] = adv_cpu_core::X87Register::from_parts(1 << 63, 0x3fff);
        cpu.x87.regs[1] = cpu.x87.regs[0];
        cpu.xmm[1] = [3; 16];
        cpu.eflags = 0x8d7;

        native.candidates.push(CODE);
        native.set_computation_offer(true, None);
        let mut state = start.cpu().clone();
        assert_eq!(
            native
                .run(
                    &mut state,
                    executor.memory_mut(),
                    stop,
                    1 << 16,
                    std::ptr::null_mut()
                )
                .unwrap()
                .unwrap()
                .exit,
            Exit::Computation
        );
        let entry = Entry {
            enter: native.enter,
            opt_enter: native.opt_enter,
            window: native.runtime.window,
            alias: super::super::alias_base(executor.memory()),
            fault: native.stubs.fault,
        };
        let code = native
            .computation
            .get(&CODE)
            .expect("prepared x87 and SIMD")
            .code
            .clone();
        native.cache.sync();
        native.set_computation_offer(false, None);
        let mut serial = start.clone();
        let run_owner =
            |context: &mut ExecutionContext, native: &mut Native, executor: &mut Executor| {
                let mut retired = 0;
                while context.cpu().eip != stop {
                    let result = native
                        .run(
                            context.cpu_mut(),
                            executor.memory_mut(),
                            stop,
                            1 << 16,
                            std::ptr::null_mut(),
                        )
                        .unwrap()
                        .unwrap();
                    assert_eq!(result.exit, Exit::Dispatch);
                    assert_ne!(result.retired, 0);
                    retired += u64::from(result.retired);
                }
                context.cpu_mut().refresh_mmx();
                retired
            };
        let total = run_owner(&mut serial, &mut native, &mut executor);
        assert_eq!(total, u64::from(COUNT) * 7);
        let len = COUNT as usize * 8;
        let expected = executor.memory().read(OUTPUT, len).unwrap().to_vec();
        for pending in [0, adv_sync::pending::DEADLINE] {
            executor.memory_mut().fill(OUTPUT, &[0], len).unwrap();
            let signals = Signals {
                recall: Arc::new(AtomicU32::new(0)),
                pending: (
                    Arc::new(AtomicU32::new(pending)),
                    adv_sync::pending::DEADLINE,
                ),
            };
            let done = run(
                &mut Runtime::new(std::ptr::null_mut(), 0),
                3,
                start.clone(),
                entry,
                (&code, 0),
                (&signals, LOCAL),
                &mut ExecuteScratch::default(),
            )
            .completion;
            // A pending deadline finishes the iteration and returns at the head.
            assert!(!done.requires_owner);
            if pending != 0 {
                assert_eq!(done.retired, u64::from(COMPUTATION_SLICE / 7 * 7 + 7));
            }
            let mut context = done.context.unwrap();
            let suffix = run_owner(&mut context, &mut native, &mut executor);
            assert_eq!(done.retired + suffix, total);
            assert_eq!(context.cpu(), serial.cpu());
            assert_eq!(executor.memory().read(OUTPUT, len).unwrap(), expected);
        }
    }
}
