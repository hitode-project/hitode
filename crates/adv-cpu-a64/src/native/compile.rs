//! Builds optimized forms (A64-OPT-1) away from the owner, which installs
//! each at its next entry if the unit it was built for is still current.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use adv_cpu_core::CpuState;

use crate::translate::computation::Proof;
use crate::translate::opt::{back, front};
use crate::translate::{Trace, Unit};

pub(super) struct Request {
    pub eip: u32,
    /// The cache generation and the ordinary computation code the form is
    /// for: a result for any other is stale.
    pub generation: u64,
    pub code: usize,
    pub trace: Trace,
    pub proof: Proof,
    /// The entry state the form takes its facts from.
    pub at: CpuState,
}

/// The code of a result no computation matches: it only settles the count
/// of results the owner awaits.
const SUPERSEDED: usize = 0;

pub(super) struct Compiled {
    pub eip: u32,
    pub generation: u64,
    pub code: usize,
    pub form: Option<Form>,
}

pub(super) struct Form {
    pub built: front::Built,
    pub lowered: back::Lowered,
    /// Instructions the entry region prepays, for the JIT map.
    pub prepaid: u32,
}

fn compile(request: Request) -> Compiled {
    let Request {
        eip,
        generation,
        code,
        trace,
        proof,
        at,
    } = request;
    let started = std::time::Instant::now();
    let mut unit = Unit::default();
    unit.trace = trace;
    let form = match front::build(&unit, &proof, &at) {
        Ok(built) => {
            let lowered = back::lower(&built.func);
            let (cuts, us) = (built.cuts, started.elapsed().as_micros());
            crate::jit_map::optimized(eip, format_args!("built\t{cuts}\t{us}us"));
            Some(Form {
                lowered,
                built,
                prepaid: unit.trace.prepaid(0),
            })
        }
        Err(reject) => {
            let inst = reject.index().and_then(|i| unit.trace.insts.get(i));
            let (pc, op) = inst.map_or((0, None), |inst| (inst.pc, Some(&inst.op)));
            crate::jit_map::optimized(eip, format_args!("{reject:?}\t{pc}\t{op:?}"));
            None
        }
    };
    Compiled {
        eip,
        generation,
        code,
        form,
    }
}

/// Where requests compile: on a thread of their own, or in place, which
/// keeps tests deterministic.
pub(super) enum Compiler {
    Inline(Vec<Compiled>),
    Thread {
        send: Option<Sender<Request>>,
        recv: Option<Receiver<Compiled>>,
        pending: usize,
    },
}

impl Compiler {
    pub fn new() -> Self {
        if cfg!(test) {
            Self::Inline(Vec::new())
        } else {
            Self::background()
        }
    }

    /// The thread starts with the first request.
    pub fn background() -> Self {
        Self::Thread {
            send: None,
            recv: None,
            pending: 0,
        }
    }

    pub fn request(&mut self, request: Request) {
        match self {
            Self::Inline(ready) => ready.push(compile(request)),
            Self::Thread {
                send,
                recv,
                pending,
            } => {
                let send = send.get_or_insert_with(|| {
                    let (send, requests) = channel();
                    let (done, results) = channel();
                    *recv = Some(results);
                    std::thread::Builder::new()
                        .name("guest compile".into())
                        .spawn(move || {
                            set_priority();
                            let mut queue = std::collections::VecDeque::new();
                            loop {
                                if queue.is_empty() {
                                    let Ok(request) = requests.recv() else {
                                        return;
                                    };
                                    queue.push_back(request);
                                }
                                queue.extend(requests.try_iter());
                                let request: Request = queue.pop_front().unwrap();
                                // A later request for the same entry, from a
                                // retranslation, supersedes this one.
                                let compiled = if queue.iter().any(|r| r.eip == request.eip) {
                                    Compiled {
                                        eip: request.eip,
                                        generation: request.generation,
                                        code: SUPERSEDED,
                                        form: None,
                                    }
                                } else {
                                    compile(request)
                                };
                                if done.send(compiled).is_err() {
                                    return;
                                }
                            }
                        })
                        .expect("compile thread");
                    send
                });
                send.send(request).expect("compile thread");
                *pending += 1;
            }
        }
    }

    /// A finished result, if any; `wait` blocks for one still pending.
    pub fn next(&mut self, wait: bool) -> Option<Compiled> {
        match self {
            Self::Inline(ready) => ready.pop(),
            Self::Thread { pending: 0, .. } => None,
            Self::Thread { recv, pending, .. } => {
                let recv = recv.as_ref()?;
                let compiled = if wait {
                    recv.recv().ok()
                } else {
                    match recv.try_recv() {
                        Ok(compiled) => Some(compiled),
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => panic!("compile thread"),
                    }
                };
                *pending -= usize::from(compiled.is_some());
                compiled
            }
        }
    }
}

/// The form sets how fast the owner's frame computes, so the thread runs
/// at user-initiated QoS: below the interactive owner and computation
/// workers, above background work.
fn set_priority() {
    #[cfg(target_vendor = "apple")]
    // SAFETY: sets this thread's own QoS class.
    unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
    }
}
