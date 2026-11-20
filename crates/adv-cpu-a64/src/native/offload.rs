//! Runs worker jobs under an owner-held code and memory lease.
//!
//! The service owns job artifacts, pacing and split execution. The arbiter's
//! offer plan caches positive and negative proof results by the registers,
//! parameter words and mapping generation the proof reads. The service reuses
//! evaluation scratch and checks active leases again before each submission.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use adv_cpu_core::backend::{Completion, Computation, Pace};
use adv_cpu_core::{CpuState, Memory};
use adv_sync::WakeEvent;
use rustc_hash::FxHashMap;

use super::plan::{Arbiter, JobPlan, REOFFER};
use super::store::{self, ACTIVE, Active, FaultCapture, WorkerCode, WorkerFaults, site_at};
use crate::runtime::SCOPE_CELLS;
use crate::runtime::{
    COMPUTATION_SLICE, Enter, ExecutionContext, Exit, Runtime, alias_base, record_branch,
};
use crate::translate::opt::Assumed;
use crate::translate::opt::computation::{Evaluation, Plan, Proof, Range, ScopeCell};
use crate::translate::opt::recovery;
use crate::translate::opt::recovery::{FlagRecipe, Host, Record};
use crate::translate::opt::{back::Lowered, front::Built, ir::Source};

#[derive(Default)]
pub(super) struct Offload {
    pub(super) admission: Admission,
    pub(super) enabled: bool,
}

#[derive(Default)]
struct AdmissionCache {
    pub generation: Option<(u64, u64)>,
    pub regs: [u32; 8],
    pub words: Vec<(u32, Option<u32>)>,
    pub evaluation: Evaluation,
    pub admitted: bool,
    pub split: Option<Option<Plan>>,
    #[cfg(test)]
    pub evaluations: u32,
}

pub(super) struct WorkerJob<U> {
    pub(super) code: Code,
    pub(super) proof: Proof,
    pub(super) unit: U,
    pub(super) retired: u32,
    pub(super) entry_registers: u8,
}

#[derive(Clone)]
pub(super) struct Code {
    pub placed: WorkerCode,
    pub frame: Arc<[ScopeCell]>,
    pub scoped_mmx: u16,
    pub scoped_gpr: u32,
    /// The optimized form, run instead when a job meets its assumptions.
    pub optimized: Option<Arc<Optimized>>,
}

/// An optimized form of the same unit (A64-OPT-1).
pub(super) struct Optimized {
    pub code: WorkerCode,
    pub entry: Vec<Source>,
    pub assumed: Assumed,
    pub records: Vec<Record<Host>>,
    pub flags: Vec<Option<FlagRecipe>>,
    pub cells: usize,
}

impl<U> WorkerJob<U> {
    pub(super) fn install<R>(
        &mut self,
        store: &mut store::CodeStore<R>,
        eip: u32,
        built: Built,
        mut lowered: Lowered,
    ) {
        let Some(placed) =
            store.place_worker_form(&mut lowered.words, lowered.exit, &lowered.sites)
        else {
            return;
        };
        crate::jit_map::record(
            placed.entry,
            &lowered.words,
            eip,
            self.retired,
            &lowered.marks,
            "a64-opt",
        );
        self.code.optimized = Some(Arc::new(Optimized {
            code: placed,
            entry: built.func.entry,
            assumed: built.assumed,
            records: lowered.records,
            flags: lowered.flags,
            cells: lowered.spill_cells,
        }));
    }
}

/// A helper's wake latency until helpers have shown theirs: it starts
/// claiming this long after the owner.
const WAKE_NS: f64 = 50_000.0;
/// Blocks per thread, so the last block to finish leaves the others idle
/// for about a quarter of a thread's part. A block is at most one slice.
const BLOCKS_PER_THREAD: f64 = 4.0;
/// The owner's fixed work in a split besides its blocks: admission and merge.
const ROUND_NS: f64 = 10_000.0;
/// MXCSR exception flags, sticky across a loop's blocks.
const MXCSR_FLAGS: u32 = 0x3f;

#[derive(Default)]
struct ExecuteScratch {
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
    if code.scoped_mmx != 0 {
        runtime.scope_written = 0;
    }
    if code.scoped_gpr != 0 {
        runtime.scope_gpr_written = 0;
    }
    let started = Instant::now();
    let cpu = context.cpu_mut();
    cpu.settle_flags();
    if code.optimized.as_deref().is_none_or(|o| o.assumed.mmx) || cpu.mmx_mode != 0 {
        cpu.refresh_mmx();
    }
    let optimized = admit(code, cpu, entry.alias, &mut scratch.params);
    let (exit, retired, deferred) = execute(runtime, cpu, entry, code, optimized, scratch);
    let elapsed = started.elapsed();
    context.finish_computation(exit);
    Done {
        admitted: code.optimized.as_ref().map(|_| optimized.is_some()),
        deadline_recall: (exit == Exit::Recall || deferred)
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
            requires_owner: exit != Exit::Dispatch,
        },
    }
}

/// The optimized form of `code` if it has one whose entry facts hold at
/// `cpu`, filling `params`. `alias` reaches the admitted frame.
fn admit<'a>(
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
fn execute(
    runtime: &mut Runtime,
    cpu: &mut CpuState,
    entry: Entry,
    code: &Code,
    optimized: Option<&Optimized>,
    scratch: &mut ExecuteScratch,
) -> (Exit, u64, bool) {
    let host = |addr: u32| entry.alias.wrapping_add(u64::from(addr)) as *mut u8;
    let entry_esp = cpu.regs[ESP];
    let scoped_mmx = code.scoped_mmx;
    let code = optimized.map_or(&code.placed, |o| &o.code);
    if let Some(o) = optimized {
        if scratch.saved.len() < o.cells {
            scratch.saved.resize(o.cells, 0);
        }
        runtime.opt_params = scratch.params.as_ptr() as u64;
        runtime.recovery.saved = scratch.saved.as_mut_ptr() as u64;
        runtime.recovery.cells = o.cells as u64;
        runtime.recovery.recalled = 0;
    }
    scratch.capture.pc.set(0);
    let faults = WorkerFaults {
        code,
        stub: entry.fault,
        capture: &scratch.capture,
    };
    let countdown = runtime.countdown;
    runtime.cpu = cpu;
    runtime.window = entry.window;
    runtime.shadow = entry.alias;
    runtime.completed = 0;
    let previous = ACTIVE.replace(Active::Worker(std::ptr::from_ref(&faults).cast()));
    // SAFETY: the caller retains this code and mapping through the call.
    // The proved form touches only its admitted ranges and this thread's state.
    let returned = unsafe {
        // The publisher's ISB does not synchronize the worker's instruction stream.
        std::arch::asm!("isb", options(nostack, preserves_flags));
        let target_enter = if optimized.is_some() {
            entry.opt_enter
        } else {
            entry.enter
        };
        target_enter.run(runtime, code.entry)
    };
    ACTIVE.set(previous);
    runtime.cpu = std::ptr::null_mut();
    let mut exit = Exit::from_raw(returned.reason as u32).expect("computation exit");
    if let Exit::Plan(event @ crate::runtime::PlanEvent::WorkerBranch(kind)) = exit {
        record_branch(
            cpu,
            (returned.reason >> 32) | (u64::from(kind) << 32),
            cpu.eip,
        );
        crate::jit_map::optimized(cpu.eip, format_args!("plan-event\t{}", event.name()));
    }
    // An optimized form may leave with a negative countdown.
    let mut retired = countdown.wrapping_sub(returned.remaining);
    if exit == Exit::Optimized {
        let o = optimized.expect("optimized entry");
        let recovery = &runtime.recovery;
        let record = &o.records[recovery.record as usize];
        // SAFETY: as above, the frame lies in the leased ranges.
        let (refund, mmx_meta) = recovery::materialize(
            record,
            &recovery.dump,
            &scratch.saved,
            &o.flags,
            cpu,
            |offset, bytes| unsafe {
                let at = host(entry_esp.wrapping_add_signed(offset));
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), at, bytes.len());
            },
        );
        retired = retired.wrapping_sub(u64::from(refund));
        if scoped_mmx != 0 {
            runtime.scope_written = (mmx_meta & 0xff) | (u32::from(mmx_meta & 0x700 != 0) << 8);
        }
        // Every exit but a recall, failures included, continues where the
        // owner dispatches.
        exit = if recovery.recalled != 0 {
            Exit::Recall
        } else {
            Exit::Dispatch
        };
    }
    if exit == Exit::Event(crate::runtime::Event::Replay) {
        let site = site_at(&code.sites, faults.capture.pc.get()).expect("faulting site");
        faults.capture.recover(cpu, site);
        retired -= u64::from(site.refund);
    }
    returned.commit_repeat(cpu);
    if optimized.is_none_or(|o| o.assumed.mmx) || cpu.mmx_mode != 0 {
        cpu.refresh_mmx();
    }
    (exit, retired, returned.deferred != 0)
}

/// How split loops ran, for reports.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SplitCounts {
    pub splits: u64,
    pub scoped: u64,
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
    scoped: bool,
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
    last: Option<LastBlock>,
    mmx: MmxJoin,
    gpr: Option<Box<GprJoin>>,
}

impl Drained {
    fn merge(&mut self, other: Self) {
        self.blocks += other.blocks;
        self.retired += other.retired;
        self.optimized += other.optimized;
        self.sticky |= other.sticky;
        self.mmx.merge(other.mmx);
        if let Some(gpr) = other.gpr {
            if let Some(total) = &mut self.gpr {
                total.merge(&gpr);
            } else {
                self.gpr = Some(gpr);
            }
        }
        if let Some(last) = other.last
            && self
                .last
                .as_ref()
                .is_none_or(|prior| prior.index < last.index)
        {
            self.last = Some(last);
        }
    }
}

/// The latest completed writer of each byte, ordered by block index.
#[derive(Default)]
struct GprJoin {
    bytes: [Option<(u32, u8)>; 32],
}

impl GprJoin {
    fn record(&mut self, index: u32, mask: u32, cpu: &CpuState) {
        for (i, byte) in self.bytes.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                *byte = Some((index, cpu.regs[i / 4].to_le_bytes()[i % 4]));
            }
        }
    }

    fn merge(&mut self, other: &Self) {
        for (at, next) in self.bytes.iter_mut().zip(other.bytes) {
            if let Some((index, _)) = next
                && at.is_none_or(|(prior, _)| prior < index)
            {
                *at = next;
            }
        }
    }

    fn publish(&self, mask: u32, start: &CpuState, cpu: &mut CpuState) {
        for (i, value) in self.bytes.iter().enumerate() {
            if mask & (1 << i) != 0 {
                let shift = (i % 4) * 8;
                let byte = value.map_or(start.regs[i / 4] >> shift, |(_, byte)| u32::from(byte));
                let reg = &mut cpu.regs[i / 4];
                *reg = (*reg & !(0xff << shift)) | ((byte & 0xff) << shift);
            }
        }
    }
}

/// The last block that actually wrote each omitted MMX component.
#[derive(Default)]
struct MmxJoin {
    slots: [Option<(u32, adv_cpu_core::X87Register)>; 8],
    env: Option<(u32, (u8, u16))>,
}

impl MmxJoin {
    fn record(&mut self, index: u32, mask: u16, cpu: &CpuState) {
        for (i, value) in self.slots.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                *value = Some((index, cpu.x87.regs[i]));
            }
        }
        if mask & (1 << 8) != 0 {
            self.env = Some((index, (cpu.x87.top() as u8, cpu.x87.tag)));
        }
    }

    fn merge(&mut self, other: Self) {
        fn latest<T>(at: &mut Option<(u32, T)>, next: Option<(u32, T)>) {
            if let Some((index, _)) = next
                && at.as_ref().is_none_or(|&(first, _)| first < index)
            {
                *at = next;
            }
        }
        for (at, next) in self.slots.iter_mut().zip(other.slots) {
            latest(at, next);
        }
        latest(&mut self.env, other.env);
    }

    fn publish(self, mask: u16, start: &CpuState, cpu: &mut CpuState) {
        for (i, value) in self.slots.into_iter().enumerate() {
            if mask & (1 << i) != 0 {
                cpu.x87.regs[i] = value.map_or(start.x87.regs[i], |(_, value)| value);
            }
        }
        if mask & (1 << 8) != 0 {
            let (top, tag) = self
                .env
                .map_or((start.x87.top() as u8, start.x87.tag), |(_, env)| env);
            cpu.x87.set_top(usize::from(top));
            cpu.x87.tag = tag;
        }
        cpu.refresh_mmx();
    }
}

struct LastBlock {
    index: u32,
    done: Completion<Option<ExecutionContext>>,
    thread: u32,
    frame: [u32; SCOPE_CELLS],
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
            self.run_block(index, thread, runtime, scratch, &quiet, &mut out);
        }
        out.elapsed = started.elapsed();
        out
    }
    fn run_block(
        &self,
        index: u32,
        thread: u32,
        runtime: &mut Runtime,
        scratch: &mut ExecuteScratch,
        quiet: &Signals,
        out: &mut Drained,
    ) {
        let last = index + 1 == self.count;
        let iterations = if last { 0 } else { self.size };
        let context = self.enter(index * self.size, thread);
        let done = run(
            runtime,
            0,
            context,
            self.entry,
            (&self.code, iterations),
            (quiet, 0),
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
        if self.code.scoped_mmx != 0 {
            out.mmx.record(
                index,
                self.code.scoped_mmx & runtime.scope_written as u16,
                cpu,
            );
        }
        if self.code.scoped_gpr != 0 {
            out.gpr
                .get_or_insert_with(Box::default)
                .record(index, runtime.scope_gpr_written, cpu);
        }
        if last || self.scoped {
            out.last = Some(LastBlock {
                index,
                done,
                thread,
                frame: runtime.scope_frame,
            });
        }
    }

    /// Once all claims have drained, publish either the final row or the
    /// prefix that completed before recall, before the owner runs guest code.
    fn finish(&self, id: u32, total: Drained) -> (Completion<Option<ExecutionContext>>, u32) {
        let claimed = self.next.load(Ordering::Acquire).min(self.count);
        let iterations = if claimed == self.count {
            self.plan.iterations
        } else {
            claimed * self.size
        };
        let (mut completion, last) = match total.last {
            Some(last) => {
                publish_frame(
                    &self.code.frame,
                    &last.frame,
                    self.start.cpu(),
                    self.entry.alias,
                );
                (last.done, Some(last.thread))
            }
            None => (
                Completion {
                    id,
                    context: Some(self.at(iterations)),
                    retired: 0,
                    optimized: 0,
                    elapsed: Duration::ZERO,
                    helpers: Duration::ZERO,
                    requires_owner: false,
                },
                None,
            ),
        };
        let cpu = completion.context.as_mut().unwrap().cpu_mut();
        self.settle(cpu, last, iterations);
        if self.code.scoped_mmx != 0 {
            total
                .mmx
                .publish(self.code.scoped_mmx, self.start.cpu(), cpu);
        }
        if let Some(gpr) = total.gpr {
            gpr.publish(self.code.scoped_gpr, self.start.cpu(), cpu);
        }
        cpu.mxcsr |= total.sticky;
        completion.id = id;
        completion.retired = total.retired;
        completion.optimized = total.optimized;
        (completion, iterations)
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
                    store::prepare_execution_thread();
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
    sites: FxHashMap<u32, AdmissionCache>,
    grants: PageGrants,
    generation: (u64, u64),
    spare: Vec<Vec<Range>>,
}

impl Admission {
    pub(super) fn invalidate(&mut self, eip: u32) {
        self.sites.remove(&eip);
    }

    pub(super) fn clear(&mut self) {
        self.sites.clear();
    }

    fn recheck(&mut self, eip: u32, memory: &Memory, active: &[ActiveTask]) -> bool {
        self.sync_generation(memory);
        self.sites[&eip].evaluation.admitted().iter().all(|range| {
            !conflicts(range, active)
                && self
                    .grants
                    .allows(memory, range.start, range.end, range.write)
        })
    }

    fn sync_generation(&mut self, memory: &Memory) {
        let generation = (memory.identity(), memory.map_generation());
        if self.generation != generation {
            self.generation = generation;
            self.grants.clear();
        }
    }

    fn ranges<'a>(
        &'a mut self,
        job: &WorkerJob<impl Sized>,
        cpu: &CpuState,
        memory: &Memory,
        active: &[ActiveTask],
    ) -> Option<&'a [Range]> {
        self.sync_generation(memory);
        let cache = self.sites.entry(cpu.eip).or_default();
        let regs = std::array::from_fn(|i| {
            if job.entry_registers & (1 << i) != 0 {
                cpu.regs[i]
            } else {
                0
            }
        });
        let alias = alias_base(memory);
        let grants = &mut self.grants;
        if cache.generation != Some(self.generation)
            || cache.regs != regs
            || !cache
                .words
                .iter()
                .all(|&(addr, value)| parameter(memory, grants, alias, active, addr) == value)
        {
            #[cfg(test)]
            {
                cache.evaluations += 1;
            }
            cache.generation = Some(self.generation);
            cache.regs = regs;
            cache.words.clear();
            cache.split = None;
            cache.admitted = job
                .proof
                .evaluate(cpu, &mut cache.evaluation, |addr| {
                    let value = parameter(memory, grants, alias, active, addr);
                    cache.words.push((addr, value));
                    value
                })
                .is_some_and(|ranges| {
                    ranges
                        .iter()
                        .all(|range| grants.allows(memory, range.start, range.end, range.write))
                });
        }
        let ranges = cache.evaluation.admitted();
        (cache.admitted && ranges.iter().all(|range| !conflicts(range, active))).then_some(ranges)
    }
}

impl Admission {
    /// After `ranges`, the blocks of its head loop, if it splits.
    fn plan(
        &mut self,
        proof: &Proof,
        cpu: &CpuState,
        memory: &Memory,
        active: &[ActiveTask],
    ) -> Option<Plan> {
        let alias = alias_base(memory);
        let grants = &mut self.grants;
        let cache = self.sites.get_mut(&cpu.eip).unwrap();
        *cache.split.get_or_insert_with(|| {
            proof.plan(cpu, &mut cache.evaluation, |addr| {
                let value = parameter(memory, grants, alias, active, addr);
                cache.words.push((addr, value));
                value
            })
        })
    }
}

impl Admission {
    /// Whether the evaluated computation may also write `region`, which it
    /// does not otherwise touch.
    fn private(
        &mut self,
        memory: &Memory,
        active: &[ActiveTask],
        region: &Range,
        eip: u32,
    ) -> bool {
        let admitted = self.sites[&eip].evaluation.admitted();
        let overlaps = |other: &Range| {
            u64::from(region.start) < other.end && u64::from(other.start) < region.end
        };
        !admitted.iter().any(overlaps)
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

pub(super) struct Session<'a, U> {
    entry: Entry,
    jobs: &'a FxHashMap<u32, WorkerJob<U>>,
    plans: &'a mut Arbiter,
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
    scope_updates: Vec<u32>,
}

impl<'a, U> Session<'a, U> {
    pub(super) fn new(
        entry: Entry,
        (jobs, plans): (&'a FxHashMap<u32, WorkerJob<U>>, &'a mut Arbiter),
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
            jobs,
            plans,
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
            scope_updates: Vec::new(),
        })
    }

    pub(super) fn take_scope_updates(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.scope_updates)
    }

    fn prepare(&mut self, id: u32, context: &ExecutionContext) -> Option<(&Code, &[Range], bool)> {
        let cpu = context.cpu();
        if self.signals.requested() || self.active.iter().any(|job| job.id == id) {
            return None;
        }
        if cpu.eflags & ((1 << 8) | (1 << 16)) != 0 || cpu.rep.is_some() || context.has_resume() {
            return None;
        }
        let job = self.jobs.get(&cpu.eip)?;
        let policy = self.plans.job_mut(cpu.eip).unwrap();
        let iteration = policy.iteration();
        let Some(ranges) = self.admission.ranges(job, cpu, self.memory, &self.active) else {
            policy.reoffer.set(REOFFER);
            return None;
        };
        Some((&job.code, ranges, iteration))
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
        let wake = wake_cost(counts);
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
            if !self
                .admission
                .private(self.memory, &self.active, &region, context.cpu().eip)
            {
                return None;
            }
        }
        let started = Instant::now();
        let eip = context.cpu().eip;
        let scoped = self.plans.job(eip).unwrap().scoped;
        let mut start = context.clone();
        start.cpu_mut().settle_flags();
        if code.scoped_mmx != 0 {
            start.cpu_mut().refresh_mmx();
        }
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
            scoped,
        });
        let (send, recv) = sync_channel(helpers);
        let submitted = (0..helpers)
            .take_while(|_| {
                (!scoped || self.admission.recheck(eip, self.memory, &self.active))
                    && self.pool.submit(Job::Blocks(blocks.clone(), send.clone()))
            })
            .count();
        if scoped && submitted == 0 {
            return None;
        }
        let mut total = blocks.drain(&mut self.runtime, &mut ExecuteScratch::default());
        let mut helped = Duration::ZERO;
        self.counts.splits += 1;
        self.counts.scoped += u64::from(scoped);
        if scoped {
            let job = self.plans.job(eip).unwrap();
            job.scope_splits.set(job.scope_splits.get() + 1);
        }
        self.counts.wakes += submitted as u64;
        for _ in 0..submitted {
            let drained = recv.recv().expect("block helper");
            if drained.blocks == 0 {
                self.counts.idle_wakes += 1;
            } else {
                self.counts.claim_ns += drained.claimed_after.as_nanos() as u64;
            }
            helped += drained.elapsed;
            total.merge(drained);
        }
        self.counts.blocks += u64::from(total.blocks);
        let (mut completion, iterations) = blocks.finish(id, total);
        completion.elapsed = started.elapsed();
        completion.helpers = helped;
        learn(
            self.plans.job(context.cpu().eip).unwrap(),
            &completion,
            iterations,
            wake_cost(self.counts),
        );
        Some(completion)
    }

    fn account(
        &mut self,
        entry: u32,
        iteration: bool,
        done: Done,
    ) -> Completion<Option<ExecutionContext>> {
        let policy = self.plans.job(entry).unwrap();
        policy.observe(
            entry,
            iteration,
            done.deadline_recall,
            &done.completion,
            COMPUTATION_SLICE,
        );
        if let Some(admitted) = done.admitted {
            policy.admitted(admitted);
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

fn publish_frame(cells: &[ScopeCell], values: &[u32; SCOPE_CELLS], start: &CpuState, alias: u64) {
    for (&ScopeCell { base, offset }, &value) in cells.iter().zip(values) {
        let address = start.regs[usize::from(base)].wrapping_add_signed(offset);
        // SAFETY: the owner still holds the write lease on this dword.
        unsafe { (alias.wrapping_add(u64::from(address)) as *mut u32).write_unaligned(value) };
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

fn wake_cost(counts: &SplitCounts) -> f64 {
    match counts.wakes - counts.idle_wakes {
        0 => WAKE_NS,
        claimed => (counts.claim_ns as f64 / claimed as f64).max(1.0),
    }
}

/// Records the instructions and time per outer iteration of a job that ran
/// `iterations` of them.
fn learn(
    prepared: &JobPlan,
    done: &Completion<Option<ExecutionContext>>,
    iterations: u32,
    wake: f64,
) {
    if iterations == 0 || done.requires_owner {
        return;
    }
    let n = u64::from(iterations);
    let ns = (done.elapsed + done.helpers).as_nanos() as f64;
    prepared.set_cost(ns as u64);
    if prepared.scoped && ns > 0.0 {
        prepared
            .split_minimum
            .set((9.0 * wake * n as f64 / ns).ceil().max(2.0) as u64);
    }
    prepared
        .per_iteration
        .set(Some(((done.retired / n).max(1), ns / n as f64)));
}

impl<U> Computation<Option<ExecutionContext>> for Session<'_, U> {
    fn submit(&mut self, id: u32, context: &Option<ExecutionContext>) -> bool {
        let Some(context) = context.as_ref() else {
            return false;
        };
        let guest_entry = context.cpu().eip;
        if self.plans.job(guest_entry).is_some_and(|job| job.scoped)
            || self.active.len() == self.pool.limit
        {
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
        let prepared = &self.jobs[&guest_entry];
        let plan = if prepared.proof.splits() {
            self.admission
                .plan(&prepared.proof, context.cpu(), self.memory, &self.active)
        } else {
            None
        };
        let scoped = self.plans.job(guest_entry).unwrap().scoped;
        if scoped {
            self.scope_updates.push(guest_entry);
        }
        if let Some(plan) = plan
            && let Some(pace) = self.pace
            && let Some(per_iteration) = self.plans.job(guest_entry).unwrap().per_iteration.get()
            && let Some(completion) = self.split(id, context, &code, plan, per_iteration, pace)
        {
            let done = Done {
                completion,
                deadline_recall: false,
                admitted: None,
            };
            return Some(self.account(guest_entry, false, done));
        }
        let job = self.plans.job(guest_entry).unwrap();
        if scoped && (plan.is_none() || job.per_iteration.get().is_some()) {
            return None;
        }
        let mut scratch = ExecuteScratch::default();
        let quiet = Signals {
            recall: self.signals.recall.clone(),
            pending: (self.signals.pending.0.clone(), 0),
        };
        let signals = if code.frame.is_empty() {
            (&self.signals, LOCAL)
        } else {
            (&quiet, 0)
        };
        let done = run(
            &mut self.runtime,
            id,
            context.clone(),
            self.entry,
            (&code, u32::from(iteration || scoped)),
            signals,
            &mut scratch,
        );
        if !code.frame.is_empty() {
            publish_frame(
                &code.frame,
                &self.runtime.scope_frame,
                context.cpu(),
                self.entry.alias,
            );
        }
        if let Some(plan) = plan
            && !done.completion.requires_owner
        {
            let eip = done.completion.context.as_ref().unwrap().cpu().eip;
            let iterations = match (eip == guest_entry, iteration || scoped) {
                (false, _) => plan.iterations,
                (true, true) => 1,
                (true, false) => 0,
            };
            learn(
                self.plans.job(guest_entry).unwrap(),
                &done.completion,
                iterations,
                wake_cost(self.counts),
            );
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

impl<U> Drop for Session<'_, U> {
    fn drop(&mut self) {
        self.recall();
        while self.wait(None).is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use adv_cpu_core::memory::{EXEC, READ, WRITE};
    use iced_x86::code_asm::*;

    use super::super::compile::{self, Workspace};
    use super::super::plan::Mode;
    use super::super::store::WorkerCode;
    use super::*;
    use crate::Executor;
    use crate::native::Native;
    use crate::translate;
    use crate::translate::opt::computation::Evaluation;

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

    // spec: A64A-PLACE-3 A64-CTL-7 A64-CTL-9 A64-CTL-10 A64-TR-9
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
        native.observe_stop(stop);
        assert!(native.force_resident(memory, head));
        cpu.eip = head;
        native.set_computation_offer(true, None);
        let entry = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(entry.exit, Exit::Computation);
        assert_eq!(entry.retired, 0);
        // The next linked arrival is the next scheduled offer sample.
        native.plans.job(head).unwrap().reoffer.set(1);
        cpu.eip = CODE;
        let incoming = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(incoming.exit, Exit::Computation);
        assert_eq!((cpu.eip, incoming.retired), (head, 1));

        let version = &native.code.resident(head).unwrap().versions[0];
        let (probe, entry) = (version.probe, version.entry);
        let bypass = store::branch(probe, entry);
        native.code.cache.patch(probe, bypass);
        assert_eq!(native.code.cache.word(probe), crate::encode::b(20));
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
                enter: native.code.enter,
                opt_enter: native.code.opt_enter,
                window: native.runtime.window,
                alias: alias_base(executor.memory()),
                fault: native.code.stubs.fault,
            },
            (&native.worker_jobs.get(&head).unwrap().code, 0),
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
        assert!(!native.worker_jobs.contains_key(&head));
    }

    // spec: A64-OPT-9, A64-TR-9
    #[test]
    fn scoped_rows_split_under_the_lease_and_match_serial() {
        scoped_split(false, false, false);
    }

    // spec: A64-OPT-9
    #[test]
    fn scoped_rows_use_host_frame_cells_and_preserve_guest_bytes() {
        scoped_split(true, false, false);
    }

    // spec: A64-OPT-9
    #[test]
    fn scoped_rows_join_the_latest_actual_mmx_writers_before_the_tail() {
        scoped_split(true, true, false);
        scoped_split(true, true, true);
    }

    fn scoped_split(frame: bool, conditional_mmx: bool, raw: bool) {
        let _pool = exclusive_pool();
        const ROWS: u32 = 128;
        const WIDTH: u32 = 8;
        const FRAME_PAGE: u32 = 0x8_0000;
        let original_frame = [0xa6; 0x4000];
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        let mut row = a.create_label();
        a.set_label(&mut row).unwrap();
        let mut callee = a.create_label();
        if frame {
            a.mov(dword_ptr(esp + 8), esi).unwrap();
            a.mov(esi, dword_ptr(esp + 8)).unwrap();
            a.mov(dword_ptr(ebx + 4), esi).unwrap();
            a.call(callee).unwrap();
        }
        if conditional_mmx {
            let mut skip = a.create_label();
            a.cmp(edx, ROWS / 2).unwrap();
            a.jbe(skip).unwrap();
            a.movq(mm1, qword_ptr(esi)).unwrap();
            a.set_label(&mut skip).unwrap();
        }
        let step = if conditional_mmx { 16 } else { 8 };
        a.mov(ecx, WIDTH).unwrap();
        let mut pixel = a.create_label();
        a.set_label(&mut pixel).unwrap();
        if conditional_mmx {
            a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
            a.por(xmm0, xmm7).unwrap();
            a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
        } else {
            a.movq(mm0, qword_ptr(esi)).unwrap();
            a.por(mm0, mm7).unwrap();
            a.movntq(qword_ptr(edi), mm0).unwrap();
        }
        a.add(esi, step).unwrap();
        a.add(edi, step).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(pixel).unwrap();
        a.dec(edx).unwrap();
        a.jnz(row).unwrap();
        if conditional_mmx {
            a.movd(eax, mm1).unwrap();
        }
        if frame {
            let mut done = a.create_label();
            a.jmp(done).unwrap();
            a.set_label(&mut callee).unwrap();
            a.mov(esi, dword_ptr(ebx + 4)).unwrap();
            a.ret().unwrap();
            a.set_label(&mut done).unwrap();
        }
        a.int3().unwrap();
        let (mut executor, mut native) = fixture(&mut a);
        let stop = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32 - 1;
        let head = CODE + 1;
        let len = (ROWS * WIDTH * step as u32) as usize;
        let input: Vec<u8> = (0..len as u32).map(|n| n.wrapping_mul(37) as u8).collect();
        {
            let memory = executor.memory_mut();
            memory
                .map(INPUT, len.next_multiple_of(0x4000), READ | WRITE)
                .unwrap();
            memory
                .map(OUTPUT, len.next_multiple_of(0x4000), READ | WRITE)
                .unwrap();
            memory.write(INPUT, &input).unwrap();
            if frame {
                memory
                    .map(FRAME_PAGE, original_frame.len(), READ | WRITE)
                    .unwrap();
                memory.write(FRAME_PAGE, &original_frame).unwrap();
            }
        }
        let mut cpu = CpuState {
            eip: head,
            regs: [
                0,
                0,
                ROWS,
                FRAME_PAGE + 0x200,
                FRAME_PAGE + 0x100,
                0,
                INPUT,
                OUTPUT,
            ],
            ..CpuState::default()
        };
        cpu.mmx[7] = 0xff00_0000_ff00_0000;
        if conditional_mmx {
            cpu.x87.set_top(3);
        }
        executor.memory_mut().attach().unwrap();
        native
            .run(
                &mut cpu,
                executor.memory_mut(),
                stop,
                0,
                std::ptr::null_mut(),
            )
            .unwrap();
        assert!(native.force_resident_at(executor.memory_mut(), cpu.clone()));
        let code = &mut native.worker_jobs.get_mut(&head).unwrap().code;
        if conditional_mmx {
            assert_eq!(code.scoped_mmx, (1 << 1) | (1 << 8));
            if raw {
                code.optimized = None;
            } else {
                assert!(code.optimized.is_some());
            }
        }
        let job = native.plans.job(head).expect("scoped worker artifact");
        assert!(job.scoped);
        assert_eq!(
            native.worker_jobs[&head].code.frame.len(),
            usize::from(frame) * 3
        );
        job.per_iteration
            .set(Some((u64::from(WIDTH) * 7 + 3, 20_000.0)));
        native.pace.set(Some(Pace::default()));
        native.set_computation_offer(true, None);
        let start = cpu.clone();
        let first = native
            .run(
                &mut cpu,
                executor.memory_mut(),
                stop,
                1 << 16,
                std::ptr::null_mut(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(first.exit, Exit::Computation);
        let context = Some(ExecutionContext {
            cpu,
            offered: None,
            resume: None,
            fuel_debt: 0,
        });
        let map_bytes = len.next_multiple_of(0x4000);
        executor
            .memory_mut()
            .protect(OUTPUT, map_bytes, READ)
            .unwrap();
        let mut denied_retired = 0;
        native.with_computation(
            executor.memory_mut(),
            &mut denied_retired,
            (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
            None,
            &mut |session| assert!(session.run_local(1, &context).is_none()),
        );
        assert_eq!(denied_retired, 0);
        executor
            .memory_mut()
            .protect(OUTPUT, map_bytes, READ | WRITE)
            .unwrap();
        let mut split = None;
        let mut retired = 0;
        native.with_computation(
            executor.memory_mut(),
            &mut retired,
            (Arc::new(AtomicU32::new(0)), adv_sync::pending::DEADLINE),
            None,
            &mut |session| {
                assert!(!session.submit(1, &context));
                split = session.run_local(1, &context);
            },
        );
        let split = split.expect("leased split");
        assert_eq!(native.diagnostics.split.scoped, 1);
        assert_eq!(split.retired, retired);
        let mut parallel = split.context.unwrap().cpu;
        if conditional_mmx {
            assert_eq!(parallel.x87.top(), 0);
            assert_eq!(parallel.x87.tag, 0);
            assert_ne!(parallel.mmx[1], 0);
        }
        native.set_computation_offer(false, None);
        for _ in 0..64 {
            if parallel.eip == stop {
                break;
            }
            retired += u64::from(
                native
                    .run(
                        &mut parallel,
                        executor.memory_mut(),
                        stop,
                        1 << 16,
                        std::ptr::null_mut(),
                    )
                    .unwrap()
                    .unwrap()
                    .retired,
            );
        }
        let split_memory = executor.memory().read(OUTPUT, len).unwrap().to_vec();
        executor.memory_mut().write(OUTPUT, &vec![0; len]).unwrap();
        let frame_after_split = frame.then(|| {
            executor
                .memory()
                .read(FRAME_PAGE, original_frame.len())
                .unwrap()
                .to_vec()
        });
        if frame {
            executor
                .memory_mut()
                .write(FRAME_PAGE, &original_frame)
                .unwrap();
        }
        let mut serial = start;
        let mut serial_retired = 0;
        for _ in 0..64 {
            if serial.eip == stop {
                break;
            }
            let run = native
                .run(
                    &mut serial,
                    executor.memory_mut(),
                    stop,
                    1 << 16,
                    std::ptr::null_mut(),
                )
                .unwrap()
                .unwrap();
            serial_retired += u64::from(run.retired);
        }
        for state in [&mut serial, &mut parallel] {
            state.settle_flags();
            state.refresh_mmx();
        }
        assert_eq!(serial.eip, stop);
        assert_eq!(parallel.eip, serial.eip);
        assert_eq!(serial.regs, parallel.regs);
        assert_eq!(serial.eflags, parallel.eflags);
        assert_eq!(serial.mmx, parallel.mmx);
        assert_eq!(serial.x87, parallel.x87);
        assert_eq!(serial_retired, u64::from(first.retired) + retired);
        assert_eq!(executor.memory().read(OUTPUT, len).unwrap(), split_memory);
        if let Some(split) = frame_after_split {
            assert_eq!(
                executor
                    .memory()
                    .read(FRAME_PAGE, original_frame.len())
                    .unwrap(),
                split
            );
            for untouched in [0..0xfc, 0x100..0x108, 0x10c..0x204, 0x208..split.len()] {
                assert_eq!(split[untouched.clone()], original_frame[untouched]);
            }
        }
    }

    // spec: A64-TR-9, A64-OPT-9
    #[test]
    fn scoped_rows_publish_byte_writers_and_private_cells_at_completion_and_recall() {
        let _pool = exclusive_pool();
        const ROWS: u32 = 128;
        const WIDTH: u32 = 8;
        const PITCH: u32 = (WIDTH / 2 + 1) * 16;
        const FRAME: u32 = 0x8_0000;
        let mut a = CodeAssembler::new(32).unwrap();
        a.nop().unwrap();
        let mut row = a.create_label();
        let mut pixel = a.create_label();
        let mut test = a.create_label();
        let mut callee = a.create_label();
        let mut tail = a.create_label();
        let mut stop = a.create_label();
        a.set_label(&mut row).unwrap();
        a.mov(dword_ptr(ebx + 4), edx).unwrap();
        a.mov(dword_ptr(ebx + 4), esi).unwrap();
        a.mov(dword_ptr(ebx + 8), edx).unwrap();
        a.mov(ecx, WIDTH).unwrap();
        a.sub(ecx, 2).unwrap();
        a.jmp(test).unwrap();
        a.set_label(&mut pixel).unwrap();
        a.movaps(xmm0, xmmword_ptr(esi)).unwrap();
        a.movaps(xmmword_ptr(edi), xmm0).unwrap();
        a.mov(ebp, dword_ptr(esi)).unwrap();
        a.add(esi, 16).unwrap();
        a.add(edi, 16).unwrap();
        a.sub(ecx, 2).unwrap();
        a.set_label(&mut test).unwrap();
        a.jns(pixel).unwrap();
        a.call(callee).unwrap();
        a.add(ebx, 4).unwrap();
        a.call(callee).unwrap();
        a.sub(ebx, 4).unwrap();
        a.imul_2(ebp, dword_ptr(ebx + 8)).unwrap();
        a.mov(dword_ptr(edi + 4), ebp).unwrap();

        let mut skip_full = a.create_label();
        a.cmp(edx, 110).unwrap();
        a.jbe(skip_full).unwrap();
        a.mov(eax, dword_ptr(esi + 8)).unwrap();
        a.set_label(&mut skip_full).unwrap();
        let mut skip_word = a.create_label();
        a.cmp(edx, 102).unwrap();
        a.jne(skip_word).unwrap();
        a.mov(ax, 0x5678).unwrap();
        a.set_label(&mut skip_word).unwrap();
        let mut skip_pair = a.create_label();
        a.cmp(edx, 95).unwrap();
        a.jne(skip_pair).unwrap();
        a.mov(ah, 0x42).unwrap();
        a.mov(al, 0x64).unwrap();
        a.set_label(&mut skip_pair).unwrap();
        let mut skip_byte = a.create_label();
        a.cmp(edx, 80).unwrap();
        a.jne(skip_byte).unwrap();
        a.mov(al, byte_ptr(esi)).unwrap();
        a.mov(byte_ptr(edi), al).unwrap();
        a.set_label(&mut skip_byte).unwrap();
        a.add(esi, 16).unwrap();
        a.add(edi, 16).unwrap();
        a.dec(edx).unwrap();
        a.jne(row).unwrap();
        a.set_label(&mut tail).unwrap();
        a.mov(ecx, eax).unwrap();
        a.add(ecx, dword_ptr(ebx + 4)).unwrap();
        a.set_label(&mut stop).unwrap();
        a.int3().unwrap();
        a.set_label(&mut callee).unwrap();
        a.add(ebp, dword_ptr(ebx + 4)).unwrap();
        a.ret().unwrap();
        let labels = a
            .assemble_options(
                u64::from(CODE),
                iced_x86::BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
            )
            .unwrap();
        let head = labels.label_ip(&row).unwrap() as u32;
        let tail = labels.label_ip(&tail).unwrap() as u32;
        let stop = labels.label_ip(&stop).unwrap() as u32;

        let len = (ROWS * PITCH) as usize;
        let mut input: Vec<_> = (0..len).map(|i| (i ^ (i >> 5)) as u8).collect();
        let last_dword = ((ROWS - 111) * PITCH + PITCH - 8) as usize;
        input[last_dword..last_dword + 4].copy_from_slice(&0x2456_8899u32.to_le_bytes());
        input[((ROWS - 80) * PITCH + PITCH - 16) as usize] = 0x33;
        for (raw, recall) in [(false, true), (false, false), (true, true), (true, false)] {
            let (mut serial, mut native) = fixture(&mut a);
            let memory = serial.memory_mut();
            for at in [INPUT, OUTPUT, FRAME] {
                memory.map(at, 0x4000, READ | WRITE).unwrap();
            }
            memory.write(INPUT, &input).unwrap();
            let reset = |memory: &mut Memory| {
                memory.write(OUTPUT, &vec![0; len]).unwrap();
                memory.write(FRAME, &[0xa6; 0x400]).unwrap();
            };
            reset(memory);
            let start = CpuState {
                eip: head,
                regs: [
                    0x1337_0033,
                    0,
                    ROWS,
                    FRAME + 0x200,
                    FRAME + 0x100,
                    0,
                    INPUT,
                    OUTPUT,
                ],
                ..CpuState::default()
            };
            memory.attach().unwrap();
            assert!(native.force_resident_at(memory, start.clone()));
            let job = &native.worker_jobs[&head];
            let frame = job.proof.scope_frame().unwrap();
            assert_eq!(frame.cells.len(), 3);
            let accesses: Vec<_> = job.unit.trace.insts.iter().zip(&frame.access).collect();
            assert!(accesses.iter().any(|(a, x)| {
                x.is_some()
                    && accesses
                        .iter()
                        .any(|(b, y)| a.pc == b.pc && y.is_some() && x != y)
            }));
            assert_eq!(job.code.scoped_gpr, 0xf);
            assert!(job.code.optimized.is_some());
            let mut evaluation = Evaluation::default();
            let memory = serial.memory();
            let read = |addr| {
                let bytes = memory.read(addr, 4).ok()?;
                Some(u32::from_le_bytes(bytes.try_into().ok()?))
            };
            job.proof
                .evaluate(&start, &mut evaluation, read)
                .expect("admitted test ranges");
            let plan = job
                .proof
                .plan(&start, &mut evaluation, read)
                .expect("disjoint test rows");
            let context = Some(ExecutionContext {
                cpu: start.clone(),
                offered: None,
                resume: None,
                fuel_debt: 0,
            });
            native.pace.set(Some(Pace::default()));
            let snapshot = |memory: &Memory| {
                [
                    memory.read(OUTPUT, len).unwrap().to_vec(),
                    memory.read(FRAME, 0x400).unwrap().to_vec(),
                ]
            };
            if raw {
                native.worker_jobs.get_mut(&head).unwrap().code.optimized = None;
            }
            reset(serial.memory_mut());
            let mut retired = 0;
            let mut completion = None;
            native
                .plans
                .job(head)
                .unwrap()
                .per_iteration
                .set(Some((64, 20_000.0)));
            if recall {
                let job = &native.worker_jobs[&head];
                let memory = serial.memory();
                // Eight claims are in flight when recall arrives.
                let blocks = Blocks {
                    next: AtomicU32::new(8),
                    stop: std::sync::atomic::AtomicBool::new(false),
                    threads: AtomicU32::new(0),
                    count: ROWS / 8,
                    size: 8,
                    plan,
                    start: context.clone().unwrap(),
                    entry: Entry {
                        enter: native.code.enter,
                        opt_enter: native.code.opt_enter,
                        window: native.runtime.window,
                        alias: alias_base(memory),
                        fault: native.code.stubs.fault,
                    },
                    code: job.code.clone(),
                    signals: Signals {
                        recall: Arc::new(AtomicU32::new(RECALL)),
                        pending: (Arc::new(AtomicU32::new(0)), 0),
                    },
                    submitted: Instant::now(),
                    scoped: true,
                };
                let mut runtime = Runtime::new(std::ptr::null_mut(), 0);
                let mut scratch = ExecuteScratch::default();
                let mut total = blocks.drain(&mut runtime, &mut scratch);
                assert_eq!(total.blocks, 0);
                let mut even = Drained::default();
                let mut odd = Drained::default();
                for index in 0..8 {
                    let out = if index % 2 == 0 { &mut even } else { &mut odd };
                    blocks.run_block(
                        index,
                        index % 2,
                        &mut runtime,
                        &mut scratch,
                        &blocks.signals,
                        out,
                    );
                }
                total.merge(odd);
                total.merge(even);
                let (prefix, rows) = blocks.finish(1, total);
                assert_eq!(rows, 64);
                completion = Some(prefix);
            } else {
                native.with_computation(
                    serial.memory_mut(),
                    &mut retired,
                    (Arc::new(AtomicU32::new(0)), 0),
                    None,
                    &mut |session| completion = session.run_local(1, &context),
                );
            }
            let completed = completion.unwrap();
            assert_eq!(completed.optimized, if raw { 0 } else { completed.retired });
            let mut parallel = completed.context.unwrap().cpu;
            assert_eq!(parallel.regs[0], 0x2456_4233);
            assert_eq!(parallel.eip, if recall { head } else { tail });
            if recall {
                let prefix = snapshot(serial.memory());
                reset(serial.memory_mut());
                serial.state = start.clone();
                assert_eq!(
                    serial.run(stop, completed.retired).unwrap(),
                    adv_cpu_core::RunOutcome::Budget,
                );
                assert_eq!(serial.state.eip, head);
                assert_eq!(serial.state.regs[0], parallel.regs[0]);
                assert_eq!(prefix, snapshot(serial.memory()));
            } else {
                assert_ne!(native.diagnostics.split.scoped, 0);
            }
            let mut retired = completed.retired;
            for _ in 0..64 {
                if parallel.eip == stop {
                    break;
                }
                let run = native
                    .run(
                        &mut parallel,
                        serial.memory_mut(),
                        stop,
                        1 << 16,
                        std::ptr::null_mut(),
                    )
                    .unwrap()
                    .unwrap();
                retired += u64::from(run.retired);
            }
            let image = snapshot(serial.memory());
            reset(serial.memory_mut());
            serial.state = start.clone();
            let budget = 1 << 16;
            assert_eq!(
                serial.run(stop, budget).unwrap(),
                adv_cpu_core::RunOutcome::ReachedStop,
            );
            assert_eq!(retired, budget - serial.state.fuel);
            for cpu in [&mut parallel, &mut serial.state] {
                cpu.fuel = 0;
                // The interpreter records inlined returns for dispatch.
                cpu.last_indirect_branch = Default::default();
                cpu.settle_flags();
            }
            assert_eq!(parallel, serial.state, "raw={raw}, recall={recall}");
            assert_eq!(image, snapshot(serial.memory()));
        }
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
        let mut cpu = start.as_ref().unwrap().cpu().clone();
        native.prepare_job(executor.memory_mut(), stop, &cpu);
        native.set_computation_offer(true, None);
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
        assert!(native.worker_jobs[&CODE].proof.splits());
        native.pace.set(Some(Pace::default()));
        let mut results = Vec::new();
        let mut memories = Vec::new();
        // Keep the first job's retirement sample and give the second a
        // serial estimate that warrants a split on every host.
        for run in 0..2 {
            if run == 1 {
                let sample = &native.plans.job(CODE).unwrap().per_iteration;
                let (retired, _) = sample.get().unwrap();
                sample.set(Some((retired, 1_000_000.0 / f64::from(N))));
            }
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
            &native.worker_jobs[&CODE].proof,
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
                enter: native.code.enter,
                opt_enter: native.code.opt_enter,
                window: native.runtime.window,
                alias: alias_base(executor.memory()),
                fault: native.code.stubs.fault,
            },
            code: native.worker_jobs[&CODE].code.clone(),
            signals: Signals {
                recall: Arc::new(AtomicU32::new(0)),
                pending: (Arc::new(AtomicU32::new(0)), 0),
            },
            submitted: Instant::now(),
            scoped: false,
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
        let mut cpu = start.as_ref().unwrap().cpu().clone();
        native.prepare_job(executor.memory_mut(), stop, &cpu);
        native.set_computation_offer(true, None);
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
        let mut cpu = start.as_ref().unwrap().cpu().clone();
        native.prepare_job(executor.memory_mut(), stop, &cpu);
        native.set_computation_offer(true, None);
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
        assert!(!native.worker_jobs[&CODE].proof.splits());
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
        let mut cpu = clean.as_ref().unwrap().cpu().clone();
        native.prepare_job(executor.memory_mut(), stop, &cpu);
        native.set_computation_offer(true, None);
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
        assert!(native.worker_jobs[&CODE].code.optimized.is_some());
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
            (Exit::Event(crate::runtime::Event::PendingX87), 0, CODE)
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
                let mut cpu = start.as_ref().unwrap().cpu().clone();
                native.prepare_job(executor.memory_mut(), stop, &cpu);
                native.set_computation_offer(true, None);
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
                let prepared = &native.worker_jobs[&CODE];
                assert!(prepared.code.optimized.is_some());
                let entry = Entry {
                    enter: native.code.enter,
                    opt_enter: native.code.opt_enter,
                    window: native.runtime.window,
                    alias: alias_base(executor.memory()),
                    fault: native.code.stubs.fault,
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
                        (&native.worker_jobs[&CODE].code, 0),
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

    // spec: A64-OPT-1 A64-OPT-14
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
        cpu.eip = head;
        native.prepare_job(executor.memory_mut(), stop, &cpu);
        let prepared = &native.worker_jobs[&head];
        let memory = executor.memory();

        // Initial check and sliding pointers inside the first page and across
        // the second page must all be admitted via PageGrants.
        cpu.eip = head;
        cpu.regs[1] = 4;
        cpu.regs[6] = INPUT;
        cpu.regs[7] = OUTPUT;
        assert!(
            native
                .offload
                .admission
                .ranges(prepared, &cpu, memory, &[])
                .is_some()
        );
        cpu.regs[6] = INPUT + 64;
        cpu.regs[7] = OUTPUT + 64;
        assert!(
            native
                .offload
                .admission
                .ranges(prepared, &cpu, memory, &[])
                .is_some()
        );
        cpu.regs[6] = INPUT + 0x1000;
        cpu.regs[7] = OUTPUT + 0x1000;
        assert!(
            native
                .offload
                .admission
                .ranges(prepared, &cpu, memory, &[])
                .is_some()
        );

        let evaluations = native.offload.admission.sites[&cpu.eip].evaluations;
        assert!(
            native
                .offload
                .admission
                .ranges(prepared, &cpu, memory, &[])
                .is_some()
        );
        assert_eq!(
            native.offload.admission.sites[&cpu.eip].evaluations,
            evaluations
        );
        // Removing read permission on the cached first input page invalidates it.
        executor.memory_mut().protect(INPUT, 4096, 0).unwrap();
        cpu.regs[6] = INPUT + 64;
        cpu.regs[7] = OUTPUT + 64;
        assert!(
            !native
                .offload
                .admission
                .ranges(prepared, &cpu, executor.memory(), &[])
                .is_some()
        );

        let evaluations = native.offload.admission.sites[&cpu.eip].evaluations;
        assert!(
            native
                .offload
                .admission
                .ranges(prepared, &cpu, executor.memory(), &[])
                .is_none()
        );
        assert_eq!(
            native.offload.admission.sites[&cpu.eip].evaluations,
            evaluations
        );

        // Restoring permissions re-admits; watching code inside the output page
        // rejects writes into the watched line while respecting exact ranges.
        executor
            .memory_mut()
            .protect(INPUT, 4096, READ | WRITE)
            .unwrap();
        assert!(
            native
                .offload
                .admission
                .ranges(prepared, &cpu, executor.memory(), &[])
                .is_some()
        );

        executor.memory_mut().watch_code(OUTPUT + 128, 64);
        cpu.regs[6] = INPUT + 128;
        cpu.regs[7] = OUTPUT + 128;
        assert!(
            !native
                .offload
                .admission
                .ranges(prepared, &cpu, executor.memory(), &[])
                .is_some()
        );
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
        let mut cpu = context.as_ref().unwrap().cpu().clone();
        native.prepare_job(memory, stop, &cpu);
        native.set_computation_offer(true, None);
        let result = native
            .run(&mut cpu, memory, stop, 1 << 16, std::ptr::null_mut())
            .unwrap()
            .unwrap();
        assert_eq!(result.exit, Exit::Computation);
        assert!(native.unit_address(CODE).is_some());
        assert!(native.worker_jobs.contains_key(&CODE));
        let middle = CODE + 4096u32.div_ceil(nop.len() as u32) * nop.len() as u32;
        memory.write(middle, &[0xcc]).unwrap();
        native.invalidate(memory);
        assert!(native.unit_address(CODE).is_none());
        assert!(!native.worker_jobs.contains_key(&CODE));
    }

    // spec: A64A-ROLE-8 A64-CTL-4 A64-CTL-6 A64-CTL-9 A64-OPT-11
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
        native.prepare_job(memory, TARGET, &serial);
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
        assert_eq!(native.diagnostics.optimized_runs, 0);
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
        let at = native.code.cache.next();
        let mut scratch = translate::Code::default();
        let emitted = compile::lower_computation(
            &work.unit,
            &mut scratch,
            CODE,
            native.features,
            at,
            native.code.stubs,
            (0x37f, false, None),
        )
        .unwrap();
        assert_eq!(native.code.cache.append(emitted.words), Some(at));
        native.code.cache.sync();
        let code = Code {
            frame: Arc::new([]),
            scoped_mmx: 0,
            scoped_gpr: 0,
            placed: WorkerCode {
                entry: at,
                end: at + emitted.words.len() * 4,
                sites: emitted
                    .sites
                    .iter()
                    .map(|&(offset, site)| (at + offset, site))
                    .collect(),
            },
            optimized: None,
        };
        let entry = Entry {
            enter: native.code.enter,
            opt_enter: native.code.opt_enter,
            window: native.runtime.window,
            // Point this fixture at the protected view to inject a fault.
            alias: native.runtime.window,
            fault: native.code.stubs.fault,
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
        native.prepare_job(executor.memory_mut(), stop, start.cpu());
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
        native.code.cache.sync();

        // A separate ordinary translator stops at the same original head.
        let mut owner = Native::new(native.runtime.window as usize).unwrap();
        owner.first_visits = false;
        owner.keep_ordinary(CODE + 5); // The inner head follows mov ecx,imm32.
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
            enter: native.code.enter,
            opt_enter: native.code.opt_enter,
            window: native.runtime.window,
            alias: alias_base(executor.memory()),
            fault: native.code.stubs.fault,
        };
        let prepared = native.plans.job(CODE).unwrap();
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
                (&native.worker_jobs[&CODE].code, 0),
                (&pending, RECALL),
                &mut ExecuteScratch::default(),
            );
            assert!(done.deadline_recall);
            prepared.observe(
                CODE,
                false,
                done.deadline_recall,
                &done.completion,
                COMPUTATION_SLICE,
            );
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
            (&native.worker_jobs[&CODE].code, 1),
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
        let prepared = native.plans.job(CODE).unwrap();
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
        prepared.observe(
            CODE,
            false,
            sample.deadline_recall,
            &sample.completion,
            COMPUTATION_SLICE,
        );
        assert!(prepared.mode.get() == Mode::Trial);
        prepared.observe(
            CODE,
            true,
            sample.deadline_recall,
            &sample.completion,
            COMPUTATION_SLICE,
        );
        assert!(prepared.mode.get() == Mode::WholeOnly);
        prepared.mode.set(Mode::Trial);
        sample.completion.elapsed = Duration::from_micros(100);
        prepared.observe(
            CODE,
            true,
            sample.deadline_recall,
            &sample.completion,
            COMPUTATION_SLICE,
        );
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

        native.prepare_job(executor.memory_mut(), stop, start.cpu());
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
            enter: native.code.enter,
            opt_enter: native.code.opt_enter,
            window: native.runtime.window,
            alias: alias_base(executor.memory()),
            fault: native.code.stubs.fault,
        };
        let code = native
            .worker_jobs
            .get(&CODE)
            .expect("prepared x87 and SIMD")
            .code
            .clone();
        native.code.cache.sync();
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
