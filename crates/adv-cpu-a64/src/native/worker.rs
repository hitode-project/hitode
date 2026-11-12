//! Execution under an owner-held code and memory lease. Only the owner
//! evaluates ranges and accepts completions.

use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use adv_cpu_core::backend::{Completion, Computation};
use adv_sync::WakeEvent;
use rustc_hash::FxHashMap;

use super::{ACTIVE, Active, Enter, Exit, FaultCapture, Memory, Runtime, Site, record_branch};
use crate::executor::ExecutionContext;
use crate::runtime::COMPUTATION_SLICE;
use crate::translate::computation::{Evaluation, Plan, Proof, Range};

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

pub(super) struct Prepared {
    pub code: Code,
    pub proof: Proof,
    skip: std::cell::Cell<u32>,
    mode: std::cell::Cell<Mode>,
    /// Instructions and time per outer iteration, once a job showed them.
    per_iteration: std::cell::Cell<Option<(u64, f64)>>,
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
    pub fn new(code: Code, iterates: bool, proof: Proof) -> Self {
        let mode = if iterates {
            Mode::Whole(0)
        } else {
            Mode::WholeOnly
        };
        Self {
            code,
            proof,
            skip: std::cell::Cell::new(0),
            mode: std::cell::Cell::new(mode),
            per_iteration: std::cell::Cell::new(None),
        }
    }

    /// The code and whether a job stops after one outer iteration.
    fn selected(&self) -> (&Code, bool) {
        (
            &self.code,
            matches!(self.mode.get(), Mode::Trial | Mode::Iteration),
        )
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
}

pub(super) struct Faults<'a> {
    pub code: &'a Code,
    pub stub: usize,
    pub capture: FaultCapture,
}

#[derive(Clone, Copy)]
pub(super) struct Entry {
    pub enter: Enter,
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
    signals: &Signals,
    recall_mask: u32,
) -> Done {
    let cpu = context.cpu_mut();
    cpu.settle_flags();
    cpu.refresh_mmx();
    let faults = Faults {
        code,
        stub: entry.fault,
        capture: FaultCapture::default(),
    };
    runtime.cpu = cpu;
    runtime.window = entry.window;
    runtime.shadow = entry.alias;
    runtime.countdown = u64::from(COMPUTATION_SLICE);
    runtime.recall = signals.recall.as_ptr() as u64;
    runtime.recall_mask = recall_mask;
    runtime.computation_pending = signals.pending.0.as_ptr() as u64;
    runtime.computation_pending_mask = signals.pending.1;
    runtime.completed = 0;
    runtime.head_exit = head_exit;
    runtime.deferred = 0;
    runtime.branch = 0;
    runtime.fault_simd = faults.capture.simd.as_ptr() as u64;
    let previous = ACTIVE.replace(Active::Worker(std::ptr::from_ref(&faults).cast()));
    let started = Instant::now();
    // SAFETY: the session retains this code and mapping through completion.
    // The proved form touches only its admitted ranges and this thread's state.
    unsafe {
        // Instruction-cache maintenance is done by the publishing owner.
        // Its ISB does not synchronize this worker's instruction stream.
        std::arch::asm!("isb", options(nostack, preserves_flags));
        (entry.enter)(runtime, code.entry);
    }
    let elapsed = started.elapsed();
    ACTIVE.set(previous);
    runtime.cpu = std::ptr::null_mut();
    let exit = Exit::from_raw(runtime.exit).expect("computation exit");
    let mut retired = runtime.completed + u64::from(COMPUTATION_SLICE) - runtime.countdown;
    if exit == Exit::Fault {
        let site = super::site_at(&code.sites, faults.capture.pc.get()).expect("faulting site");
        faults.capture.recover(cpu, site, runtime.temps);
        retired -= u64::from(site.refund);
    }
    record_branch(cpu, runtime.branch, cpu.eip);
    cpu.refresh_mmx();
    context.finish_computation(exit);
    Done {
        deadline_recall: (exit == Exit::Recall || runtime.deferred != 0)
            && signals.pending.0.load(Ordering::Acquire)
                & signals.pending.1
                & adv_sync::pending::DEADLINE
                != 0,
        completion: Completion {
            id,
            context: Some(context),
            retired,
            elapsed,
            helpers: Duration::ZERO,
            requires_owner: exit != Exit::Dispatch || runtime.branch != 0,
        },
    }
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
    elapsed: Duration,
    sticky: u32,
    blocks: u32,
    /// From submission to its first claim.
    claimed_after: Duration,
    /// The last block's completion and its thread.
    last: Option<(Completion<Option<ExecutionContext>>, u32)>,
}

const ESP: usize = 4;

/// The distance between private frames: the span in cache lines.
fn frame_stride((lo, hi): (i32, i32)) -> u32 {
    ((hi - lo) as u32).next_multiple_of(64)
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

    fn drain(&self, runtime: &mut Runtime) -> Drained {
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
                &quiet,
                0,
            )
            .completion;
            let cpu = done.context.as_ref().expect("block context").cpu();
            assert!(
                last || (!done.requires_owner && cpu.eip == self.start.cpu().eip),
                "a proved block left its iterations"
            );
            out.retired += done.retired;
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
                    loop {
                        let job = recv.lock().unwrap().recv();
                        let task = match job {
                            Ok(Job::Whole(task)) => *task,
                            Ok(Job::Blocks(blocks, done)) => {
                                let drained = blocks.drain(&mut runtime);
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
                            &task.signals,
                            RECALL,
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

#[derive(Default)]
pub(super) struct Admission {
    eval: Evaluation,
    grants: Vec<Range>,
    generation: (u64, u64),
    spare: Vec<Vec<Range>>,
}

impl Admission {
    fn ranges<'a>(
        &'a mut self,
        proof: &Proof,
        cpu: &adv_cpu_core::CpuState,
        memory: &Memory,
        active: &[ActiveTask],
    ) -> Option<&'a [Range]> {
        let generation = (memory.identity(), memory.map_generation());
        if self.generation != generation {
            self.generation = generation;
            self.grants.clear();
        }
        let grants = &mut self.grants;
        let ranges = proof.evaluate(cpu, &mut self.eval, |addr| {
            parameter(memory, grants, active, addr)
        })?;
        ranges
            .iter()
            .all(|range| !conflicts(range, active) && grant(memory, &mut self.grants, range))
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
        let grants = &mut self.grants;
        proof.plan(cpu, &mut self.eval, |addr| {
            parameter(memory, grants, active, addr)
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
            && grant(memory, &mut self.grants, region)
    }
}

/// One ordinary four-byte parameter cell, after checking active write leases.
fn parameter(
    memory: &Memory,
    grants: &mut Vec<Range>,
    active: &[ActiveTask],
    addr: u32,
) -> Option<u32> {
    let range = Range {
        start: addr,
        end: u64::from(addr) + 4,
        write: false,
    };
    if conflicts(&range, active) || !grant(memory, grants, &range) {
        return None;
    }
    let mut bytes = [0; 4];
    memory.read_into(addr, &mut bytes).ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn grant(memory: &Memory, cache: &mut Vec<Range>, range: &Range) -> bool {
    if cache.iter().any(|grant| {
        grant.start == range.start && grant.end == range.end && grant.write == range.write
    }) {
        return true;
    }
    let Ok(len) = usize::try_from(range.end - u64::from(range.start)) else {
        return false;
    };
    if !memory.computation_range(range.start, len, range.write) {
        return false;
    }
    if cache.len() == 64 {
        cache.clear();
    }
    cache.push(*range);
    true
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
    share: f32,
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
        (share, counts): (f32, &'a mut SplitCounts),
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
            share,
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
    /// the owner and the fewest idle workers that save the session's share
    /// of its serial time.
    fn split(
        &mut self,
        id: u32,
        context: &ExecutionContext,
        code: &Code,
        plan: Plan,
        (retired, ns): (u64, f64),
    ) -> Option<Completion<Option<ExecutionContext>>> {
        let slice = (u64::from(COMPUTATION_SLICE) / retired).clamp(1, u64::from(u32::MAX)) as u32;
        let serial = f64::from(plan.iterations) * ns;
        let most = self.pool.idle().min(plan.iterations as usize - 1);
        let counts = &*self.counts;
        let wake = match counts.wakes - counts.idle_wakes {
            0 => WAKE_NS,
            claimed => counts.claim_ns as f64 / claimed as f64,
        };
        let helpers = helpers(serial, (wake, f64::from(slice) * ns), self.share, most);
        if helpers == 0 {
            return None;
        }
        let threads = (helpers + 1) as f64 * BLOCKS_PER_THREAD;
        let size = slice.min((f64::from(plan.iterations) / threads).ceil() as u32);
        let count = plan.iterations.div_ceil(size);
        // Private frames lie below ESP, one stride per thread.
        if let Some(frame) = plan.frame {
            let below = (helpers as u32 + 1) * frame_stride(frame);
            let start = context.cpu().regs[ESP]
                .wrapping_sub(below)
                .wrapping_add(frame.0 as u32);
            let region = Range {
                start,
                end: u64::from(start) + u64::from(below),
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
        let mut total = blocks.drain(&mut self.runtime);
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
        self.prepared[&entry].observe(entry, iteration, &done);
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

/// The fewest helpers whose blocks cut `serial` ns by `share`, or all that
/// are worth waking, given the `wake` latency and the most a `slice` block
/// takes. One is worth waking only if its part of the loop exceeds four
/// wake latencies.
fn helpers(serial: f64, (wake, slice): (f64, f64), share: f32, most: usize) -> usize {
    let worth = ((serial - wake) / (4.0 * wake) - 1.0).max(0.0) as usize;
    let most = most.min(worth);
    let target = serial * f64::from(1.0 - share);
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
            && self.share > 0.0
            && let Some(per_iteration) = prepared.per_iteration.get()
            && let Some(completion) = self.split(id, context, &code, plan, per_iteration)
        {
            let done = Done {
                completion,
                deadline_recall: false,
            };
            return Some(self.account(guest_entry, false, done));
        }
        let done = run(
            &mut self.runtime,
            id,
            context.clone(),
            self.entry,
            (&code, u32::from(iteration)),
            &self.signals,
            LOCAL,
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
                window: native.runtime.window,
                alias: super::super::alias_base(executor.memory()),
                fault: native.stubs.fault,
            },
            (&native.computation.get(&head).unwrap().code, 0),
            &Signals {
                recall: Arc::new(AtomicU32::new(0)),
                pending: (
                    Arc::new(AtomicU32::new(adv_sync::pending::DEADLINE)),
                    adv_sync::pending::DEADLINE,
                ),
            },
            LOCAL,
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
        assert_eq!(helpers(400_000.0, (12_000.0, 1e9), 0.1, 7), 1);
        assert_eq!(helpers(400_000.0, (50_000.0, 1e9), 0.1, 7), 0);
        // Half of it needs two; a held key takes every helper worth one.
        assert_eq!(helpers(400_000.0, (12_000.0, 1e9), 0.5, 7), 2);
        assert_eq!(helpers(400_000.0, (12_000.0, 1e9), 1.0, 7), 7);
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
        native.split_share = 1.0;
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

    // spec: A64-TR-9
    #[test]
    fn blocks_run_frame_writing_iterations_on_private_frames() {
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
                window: native.runtime.window,
                alias: super::super::alias_base(executor.memory()),
                fault: native.stubs.fault,
            },
            (&native.computation[&outer].code, 0),
            &Signals {
                recall: Arc::new(AtomicU32::new(RECALL)),
                pending: (Arc::new(AtomicU32::new(0)), 0),
            },
            RECALL,
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
        };
        let entry = Entry {
            enter: native.enter,
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
            &signals,
            LOCAL,
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
                &pending,
                RECALL,
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
            &recalled,
            RECALL,
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
                elapsed: Duration::from_micros(10),
                helpers: Duration::ZERO,
                requires_owner: false,
            },
            deadline_recall: false,
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
                &signals,
                LOCAL,
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
