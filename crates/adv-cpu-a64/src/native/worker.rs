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
use crate::translate::computation::{Evaluation, Proof, Range};

pub(super) struct Prepared {
    pub code: Code,
    pub proof: Proof,
    skip: std::cell::Cell<u32>,
    mode: std::cell::Cell<Mode>,
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
    (code, once): (&Code, bool),
    signals: &Signals,
    local: bool,
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
    runtime.recall_mask = if local { RECALL | OWNER_NEEDED } else { RECALL };
    runtime.computation_pending = signals.pending.0.as_ptr() as u64;
    runtime.computation_pending_mask = signals.pending.1;
    runtime.completed = 0;
    runtime.head_exit = u32::from(once);
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
            requires_owner: exit != Exit::Dispatch || runtime.branch != 0,
        },
    }
}

struct Pool {
    send: SyncSender<Task>,
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
        let (send, recv) = sync_channel::<Task>(limit);
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
                        let task = recv.lock().unwrap().recv();
                        let Ok(task) = task else { break };
                        let result = run(
                            &mut runtime,
                            task.id,
                            task.context,
                            task.entry,
                            (&task.code, task.once),
                            &task.signals,
                            false,
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

    fn submit(&self, task: Task) -> bool {
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
        let ranges = proof.evaluate(cpu, &mut self.eval, |addr| {
            let range = Range {
                start: addr,
                end: u64::from(addr) + 4,
                write: false,
            };
            if conflicts(&range, active) || !grant(memory, &mut self.grants, &range) {
                return None;
            }
            let mut bytes = [0; 4];
            memory.read_into(addr, &mut bytes).ok()?;
            Some(u32::from_le_bytes(bytes))
        })?;
        ranges
            .iter()
            .all(|range| !conflicts(range, active) && grant(memory, &mut self.grants, range))
            .then_some(ranges)
    }
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
}

impl<'a> Session<'a> {
    pub(super) fn new(
        entry: Entry,
        prepared: &'a FxHashMap<u32, Prepared>,
        memory: &'a Memory,
        admission: &'a mut Admission,
        retired: &'a mut u64,
        forms: &'a mut Option<[u64; 5]>,
        notifications: Notifications,
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
        if self.pool.submit(Task {
            id,
            context: context.clone(),
            code,
            once: iteration,
            entry: self.entry,
            signals: self.signals.clone(),
            done: self.send.clone(),
            wake: self.wake.clone(),
        }) {
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
        let done = run(
            &mut self.runtime,
            id,
            context.clone(),
            self.entry,
            (&code, iteration),
            &self.signals,
            true,
        );
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
            (&native.computation.get(&head).unwrap().code, false),
            &Signals {
                recall: Arc::new(AtomicU32::new(0)),
                pending: (
                    Arc::new(AtomicU32::new(adv_sync::pending::DEADLINE)),
                    adv_sync::pending::DEADLINE,
                ),
            },
            true,
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
            (&native.computation[&outer].code, false),
            &Signals {
                recall: Arc::new(AtomicU32::new(RECALL)),
                pending: (Arc::new(AtomicU32::new(0)), 0),
            },
            false,
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
        assert!(pool.submit(Task {
            id: 1,
            context: first,
            entry,
            code: code.clone(),
            once: false,
            signals: signals.clone(),
            done: send,
            wake: None,
        }));
        let local = run(
            &mut Runtime::new(std::ptr::null_mut(), 0),
            2,
            second,
            entry,
            (&code, false),
            &signals,
            true,
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
                (&prepared.code, false),
                &pending,
                false,
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
            (&prepared.code, true),
            &recalled,
            false,
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
                (&code, false),
                &signals,
                true,
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
