//! Background preparation of resident and worker forms.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Condvar, Mutex};

use adv_cpu_core::CpuState;
use rustc_hash::FxHashSet;

use crate::translate::front::{
    Unit,
    decode::Op,
    trace::{Mode, Trace},
};
use crate::translate::jit::Code;
use crate::translate::lower::resident::RowTarget;
use crate::translate::opt::computation::Proof;
use crate::translate::opt::{back, front};

use super::Workspace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::native) enum Task {
    Discover,
    Append,
    Replace(usize),
    Job,
}

pub(in crate::native) struct Request {
    pub eip: u32,
    pub generation: u64,
    pub code: usize,
    pub task: Task,
    pub unit: Arc<Unit>,
    pub proof: Option<Proof>,
    pub at: CpuState,
    pub checked: FxHashSet<u32>,
    pub all_checked: bool,
    pub refused: FxHashSet<u32>,
    pub cuts: FxHashSet<u32>,
    pub count_entries: bool,
}

pub(in crate::native) struct Compiled {
    pub eip: u32,
    pub generation: u64,
    pub code: usize,
    pub task: Task,
    pub at: CpuState,
    pub resident: Option<Result<ResidentForm, front::Reject>>,
    pub unit: Arc<Unit>,
    pub pass_through: Option<RowTarget>,
    pub job: Option<Job>,
}

pub(in crate::native) struct ResidentForm {
    pub built: front::Built,
    pub lowered: back::Lowered,
}

impl ResidentForm {
    pub fn placement(&mut self) -> crate::translate::lower::resident::Layout<'_> {
        let lowered = &mut self.lowered;
        crate::translate::lower::resident::Layout {
            words: &mut lowered.words,
            body: lowered.body,
            miss: lowered.miss,
            cold: lowered.cold,
            env_miss: lowered.env_miss,
            environment: &lowered.environment,
            exit: lowered.exit,
            spill_cells: lowered.spill_cells,
            probation_slot: lowered.probation_slot,
            probation_sensor: lowered.probation_sensor.as_ref().unwrap(),
            accounting: &lowered.probation,
            links: &lowered.links,
            transitions: &lowered.transitions,
            sites: &lowered.sites,
            marks: &lowered.marks,
            split: lowered.split,
            row_price: lowered.row_price,
            row_failures: &lowered.row_failures,
            counters: &lowered.counters,
        }
    }
}

pub(in crate::native) struct ResidentRecovery {
    pub records:
        Vec<crate::translate::opt::recovery::Record<crate::translate::opt::recovery::Host>>,
    pub flags: Vec<Option<crate::translate::opt::recovery::FlagRecipe>>,
    pub mmx: bool,
}

pub(in crate::native) struct Job {
    pub proof: Proof,
    pub form: Option<JobForm>,
    pub scoped: Option<Arc<Unit>>,
}

pub(in crate::native) struct JobForm {
    pub built: front::Built,
    pub lowered: back::Lowered,
}

fn computation_proof(trace: &Trace) -> Option<Proof> {
    if trace.insts.iter().any(|inst| {
        inst.mode == Mode::Real
            || inst.locked.is_some()
            || inst.spin
            || inst.op.reads_clock()
            || matches!(
                inst.op,
                Op::Body(_)
                    | Op::Syscall
                    | Op::Timestamp
                    | Op::Fence
                    | Op::Strings(_)
                    | Op::CallIndirect(_)
                    | Op::JmpIndirect(_)
            )
    }) {
        return None;
    }
    let mut proof = Proof::build(trace).ok()?;
    specialize_split(&mut proof, trace);
    Some(proof)
}

fn specialize_split(proof: &mut Proof, trace: &Trace) {
    let index = std::cell::Cell::new(0);
    proof.prove_split(trace, |_, _| {
        let i = index.get();
        index.set(i + 1);
        trace.x86[i].clone()
    });
}

fn scoped_form(unit: &Unit, proof: &Proof, at: &CpuState) -> Option<JobForm> {
    let Some(frame) = proof.scope_frame() else {
        let built = front::build(unit, proof, at).ok()?;
        let lowered = back::lower(&built.func);
        return Some(JobForm { built, lowered });
    };
    // The optimizer retains guest PCs on memory operations. Only a PC
    // whose occurrences all name the same private cell can use this form.
    let mut accesses = std::collections::BTreeMap::new();
    for (inst, &cell) in unit.trace.insts.iter().zip(&frame.access) {
        if accesses
            .insert(inst.pc, cell)
            .is_some_and(|prior| prior != cell)
        {
            return None;
        }
    }
    let accesses = accesses
        .into_iter()
        .filter_map(|(pc, cell)| Some((pc, cell?)))
        .collect();
    let built = front::build_scoped(unit, proof, at, &frame.access).ok()?;
    let lowered = back::lower_scoped(&built.func, &accesses);
    Some(JobForm { built, lowered })
}

fn compile(request: Request) -> Compiled {
    let Request {
        eip,
        generation,
        code,
        task,
        unit,
        proof,
        at,
        checked,
        all_checked,
        refused,
        cuts,
        count_entries,
    } = request;
    let proof = if task == Task::Discover {
        proof.or_else(|| computation_proof(&unit.trace))
    } else {
        proof
    };
    let started = std::time::Instant::now();
    let mut scoped_job = None;
    let mut pass_through = None;
    let resident = if task == Task::Job {
        None
    } else {
        let cut_unit;
        let unit = if cuts.is_empty() {
            &*unit
        } else {
            let cut: Vec<_> = unit
                .trace
                .insts
                .iter()
                .map(|i| cuts.contains(&i.pc))
                .collect();
            cut_unit = unit.cut(&cut);
            &cut_unit
        };
        let guarded = unit
            .trace
            .insts
            .iter()
            .position(|inst| inst.guarded_slot.is_some());
        let latches = unit.trace.row_latches();
        let row_unit;
        let rows = if latches.is_empty() {
            unit
        } else {
            row_unit = {
                let mut shape = unit.clone();
                shape.trace.separate_row_latches(&latches);
                shape.trace.specialize_row_alignment(&at.regs);
                shape
            };
            &row_unit
        };
        let build = |unit: &Unit, proof: Option<&Proof>| {
            front::build_with(
                unit,
                proof,
                &at,
                &|pc| all_checked || checked.contains(&pc),
                &|pc| refused.contains(&pc),
            )
        };
        let scoped = crate::translate::opt::computation::rows::Domain::build(&rows.trace, &at.regs)
            .and_then(|(domain, mut proof)| {
                let mut shape = rows.clone();
                domain.use_proved_alignment(&mut shape.trace);
                let rows = &shape;
                let built = build(rows, None)
                    .inspect_err(|rejection| {
                        crate::jit_map::optimized(eip, format_args!("row lowering {rejection:?}"));
                    })
                    .ok()?;
                if built.cuts != 0 {
                    return None;
                }
                proof.prove_scoped_split(&rows.trace);
                crate::jit_map::optimized(
                    eip,
                    format_args!(
                        "row scope {}",
                        if proof.scoped_splits() {
                            "eligible"
                        } else {
                            "carried state or frame"
                        }
                    ),
                );
                let offer = proof.scoped_splits();
                let lowered =
                    back::lower_rows(&built.func, &built.assumed, &domain, count_entries, offer)?;
                if offer {
                    let mut worker = rows.clone();
                    if let Some((latch, taken)) = proof.scoped_latch() {
                        worker.trace.end_at_latch(latch, taken);
                    }
                    let form = scoped_form(&worker, &proof, &at);
                    scoped_job = Some(Job {
                        proof,
                        form,
                        scoped: Some(Arc::new(worker)),
                    });
                }
                Some(ResidentForm { built, lowered })
            });
        let result = match (scoped, guarded) {
            (Some(scoped), _) => Ok(scoped),
            (None, Some(index)) => {
                let inst = &unit.trace.insts[index];
                if let (Some(slot), Op::Call(value)) = (inst.guarded_slot, inst.op) {
                    pass_through = Some(RowTarget {
                        base: slot.base.unwrap(),
                        offset: slot.disp,
                        value,
                    });
                }
                Err(front::Reject::Unsupported(index))
            }
            (None, None) => build(unit, proof.as_ref()).map(|built| {
                let lowered = back::lower_resident(
                    &built.func,
                    &built.assumed,
                    &built.func.entry,
                    count_entries,
                );
                ResidentForm { built, lowered }
            }),
        };
        match &result {
            Ok(form) => {
                let (cuts, us) = (form.built.cuts + cuts.len(), started.elapsed().as_micros());
                let shape = if form.lowered.has_rows() {
                    "rows"
                } else {
                    "built"
                };
                crate::jit_map::optimized(eip, format_args!("{shape}\t{cuts}\t{us}us"));
            }
            Err(reject) => {
                let inst = reject.index().and_then(|i| unit.trace.insts.get(i));
                let (pc, op) = inst.map_or((0, None), |inst| (inst.pc, Some(&inst.op)));
                crate::jit_map::optimized(eip, format_args!("{reject:?}\t{pc}\t{op:?}"));
            }
        }
        Some(result)
    };
    let job = if scoped_job.is_none() && matches!(task, Task::Discover | Task::Job) {
        proof.map(|proof| {
            let form = front::build(&unit, &proof, &at).ok().map(|built| JobForm {
                lowered: back::lower(&built.func),
                built,
            });
            Job {
                proof,
                form,
                scoped: None,
            }
        })
    } else {
        None
    };
    Compiled {
        eip,
        generation,
        code,
        task,
        at,
        resident,
        unit,
        pass_through,
        job: scoped_job.or(job),
    }
}

impl Compiled {
    fn superseded(request: Request) -> Self {
        Self {
            eip: request.eip,
            generation: request.generation,
            code: 0,
            task: request.task,
            at: request.at,
            resident: None,
            unit: request.unit,
            pass_through: None,
            job: None,
        }
    }
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<Request>,
    active: FxHashSet<u32>,
    closed: bool,
}

struct Pool {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    done: Sender<Compiled>,
    results: Receiver<Compiled>,
}

impl Pool {
    fn new() -> Self {
        let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
        let (done, results) = channel();
        let workers =
            std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(2).max(1));
        for _ in 0..workers {
            let (queue, done) = (queue.clone(), done.clone());
            std::thread::Builder::new()
                .name("guest compile".into())
                .spawn(move || {
                    set_priority();
                    let (lock, ready) = &*queue;
                    loop {
                        let request = {
                            let mut queue = lock.lock().unwrap();
                            loop {
                                if queue.closed {
                                    return;
                                }
                                if let Some(i) = queue
                                    .pending
                                    .iter()
                                    .position(|r| !queue.active.contains(&r.eip))
                                {
                                    let request = queue.pending.remove(i).unwrap();
                                    queue.active.insert(request.eip);
                                    break request;
                                }
                                queue = ready.wait(queue).unwrap();
                            }
                        };
                        let mut compiled = compile(request);
                        let mut queue = lock.lock().unwrap();
                        queue.active.remove(&compiled.eip);
                        if queue
                            .pending
                            .iter()
                            .any(|request| request.eip == compiled.eip)
                        {
                            compiled.code = 0;
                        }
                        if done.send(compiled).is_err() {
                            return;
                        }
                        ready.notify_all();
                    }
                })
                .expect("compile thread");
        }
        Self {
            queue,
            done,
            results,
        }
    }

    fn request(&self, request: Request) {
        let (lock, ready) = &*self.queue;
        let mut queue = lock.lock().unwrap();
        if let Some(pending) = queue.pending.iter_mut().find(|r| r.eip == request.eip) {
            let old = std::mem::replace(pending, request);
            self.done.send(Compiled::superseded(old)).unwrap();
        } else {
            queue.pending.push_back(request);
            ready.notify_one();
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        let (lock, ready) = &*self.queue;
        lock.lock().unwrap().closed = true;
        ready.notify_all();
    }
}

/// In-place compilation keeps the execution tests deterministic.
pub(in crate::native) struct Compiler {
    pub tier1: Workspace,
    pub computation: Code,
    inline: Option<VecDeque<Compiled>>,
    pool: Option<Pool>,
    pending: usize,
}

impl Compiler {
    pub fn new() -> Self {
        Self {
            inline: cfg!(test).then(VecDeque::new),
            ..Self::background()
        }
    }

    pub fn background() -> Self {
        Self {
            tier1: Workspace::default(),
            computation: Code::default(),
            inline: None,
            pool: None,
            pending: 0,
        }
    }

    /// Compile in place, or on the background pool. Set before the first run.
    pub fn set_inline(&mut self, inline: bool) {
        self.inline = inline.then(VecDeque::new);
    }

    pub fn request(&mut self, request: Request) {
        if let Some(ready) = &mut self.inline {
            ready.push_back(compile(request));
        } else {
            self.pool.get_or_insert_with(Pool::new).request(request);
            self.pending += 1;
        }
    }

    /// A finished result, if any; `wait` blocks for one still pending.
    pub fn next(&mut self, wait: bool) -> Option<Compiled> {
        if let Some(ready) = &mut self.inline {
            return ready.pop_front();
        }
        if self.pending == 0 {
            return None;
        }
        let results = &self.pool.as_ref().unwrap().results;
        let compiled = if wait {
            results.recv().ok()
        } else {
            match results.try_recv() {
                Ok(compiled) => Some(compiled),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => panic!("compile threads"),
            }
        };
        self.pending -= usize::from(compiled.is_some());
        compiled
    }
}

/// Below the interactive owner and computation workers, above background work.
fn set_priority() {
    #[cfg(target_vendor = "apple")]
    // SAFETY: sets this thread's own QoS class.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
    }
}
