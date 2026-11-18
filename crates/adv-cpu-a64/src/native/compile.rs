//! Builds resident and detached forms away from the owner.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Condvar, Mutex};

use adv_cpu_core::CpuState;
use rustc_hash::FxHashSet;

use crate::translate::computation::Proof;
use crate::translate::opt::{back, front};
use crate::translate::{Mode, Op, Trace, Unit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Task {
    Discover,
    Append,
    Replace(usize),
    Job,
}

pub(super) struct Request {
    pub eip: u32,
    pub generation: u64,
    pub code: usize,
    pub task: Task,
    pub trace: Trace,
    pub proof: Option<Proof>,
    pub at: CpuState,
    pub checked: FxHashSet<u32>,
    pub all_checked: bool,
    pub refused: FxHashSet<u32>,
    pub count_entries: bool,
}

pub(super) struct Compiled {
    pub eip: u32,
    pub generation: u64,
    pub code: usize,
    pub task: Task,
    pub at: CpuState,
    pub resident: Option<Form>,
    pub job: Option<(Proof, Option<Form>)>,
}

pub(super) struct Form {
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
    let index = std::cell::Cell::new(0);
    proof.prove_split(trace, |_, _| {
        let i = index.get();
        index.set(i + 1);
        trace.x86[i]
    });
    Some(proof)
}

fn compile(request: Request) -> Compiled {
    let Request {
        eip,
        generation,
        code,
        task,
        trace,
        proof,
        at,
        checked,
        all_checked,
        refused,
        count_entries,
    } = request;
    let mut unit = Unit::default();
    unit.trace = trace;
    let proof = if task == Task::Discover {
        proof.or_else(|| computation_proof(&unit.trace))
    } else {
        proof
    };
    let started = std::time::Instant::now();
    let resident = if task == Task::Job {
        None
    } else {
        match front::build_with(
            &unit,
            proof.as_ref(),
            &at,
            &|pc| all_checked || checked.contains(&pc),
            &|pc| refused.contains(&pc),
        ) {
            Ok(built) => {
                let lowered = back::lower_resident(
                    &built.func,
                    &built.assumed,
                    &built.func.entry,
                    count_entries,
                );
                let (cuts, us) = (built.cuts, started.elapsed().as_micros());
                crate::jit_map::optimized(eip, format_args!("built\t{cuts}\t{us}us"));
                Some(Form { built, lowered })
            }
            Err(reject) => {
                let inst = reject.index().and_then(|i| unit.trace.insts.get(i));
                let (pc, op) = inst.map_or((0, None), |inst| (inst.pc, Some(&inst.op)));
                crate::jit_map::optimized(eip, format_args!("{reject:?}\t{pc}\t{op:?}"));
                None
            }
        }
    };
    let job = if matches!(task, Task::Discover | Task::Job) {
        proof.map(|proof| {
            let form = front::build(&unit, &proof, &at).ok().map(|built| Form {
                lowered: back::lower(&built.func),
                built,
            });
            (proof, form)
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
        job,
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
pub(super) struct Compiler {
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
            inline: None,
            pool: None,
            pending: 0,
        }
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
