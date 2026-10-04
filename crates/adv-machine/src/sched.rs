//! Guest contexts ("threads") and the cooperative scheduler.
//!
//! Guest threads (up to the resource limit) run under exclusive context leases
//! on a bounded execution pool. Blocking releases execution; real-time slices preempt
//! guest computation or return a completed frame to the host. The normal scheduling
//! lane remains round-robin, with a bounded preference for genuinely satisfied waits.

pub use crate::objects::Objects;
use std::any::Any;

use crate::outcome::{Continuation, DeferredContinuation, WakeReason};

pub type CtxId = u32;
pub type Handle = u32;

/// What a blocked context waits for. Times are guest microseconds ([`crate::Clock`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Wait {
    /// Let every other ready context run once, then continue (Sleep(0), SwitchToThread).
    Yield,
    /// Sleep until the given time.
    Until(u64),
    /// Wait for an object to become signaled, with an optional deadline.
    Object {
        handle: Handle,
        deadline: Option<u64>,
    },
    /// Atomically acquire any (lowest index) or all of a set of objects.
    /// A notification invalidates the caller's wait criteria without acquiring
    /// any member; USER32 uses it to refresh generated-input deadlines.
    Objects {
        handles: Vec<Handle>,
        all: bool,
        deadline: Option<u64>,
        notification: Option<Handle>,
    },
}

/// The host call a parked context is inside: where to return and what to finish.
pub(crate) struct Origin {
    /// ESP at the host call's entry ([esp] = guest return address).
    pub esp: u32,
    /// Address of the thunk's `ret[ n]`.
    pub ret_eip: u32,
    pub thunk: u32,
    /// Raw args, kept when the call is traced.
    pub trace_args: Option<Vec<u32>>,
    /// CPU interruptions finish through an architectural resume, never ret[n].
    pub interrupted: bool,
}

/// A guest call in progress on behalf of a host call.
pub(crate) struct Frame {
    pub id: u64,
    /// Actual caller-owned boundary, possibly below Origin's reserved scratch.
    pub stack: u32,
    pub return_floor: u32,
    pub then: Continuation,
    pub origin: Origin,
}

pub(crate) enum ReadyResume {
    Woke {
        then: Continuation,
        origin: Origin,
        reason: WakeReason,
    },
    Deferred {
        then: DeferredContinuation,
        origin: Origin,
    },
}

impl ReadyResume {
    fn origin(&self) -> &Origin {
        match self {
            Self::Woke { origin, .. } | Self::Deferred { origin, .. } => origin,
        }
    }
}

pub(crate) enum State {
    /// Exclusive native lease. No scheduler path may select this context until
    /// the backend returns the matching token and complete register state.
    Running {
        token: u64,
    },
    /// Runnable. `resume` completes a satisfied guest wait or deferred host work before
    /// the guest continues. Only satisfied waits enter the wake-priority queue.
    Ready {
        resume: Option<ReadyResume>,
    },
    Blocked {
        wait: Wait,
        then: Continuation,
        origin: Origin,
        allow_time_skip: bool,
    },
    Done {
        exit_code: u32,
    },
}

pub struct Context {
    pub id: CtxId,
    pub name: String,
    pub tid: u32,
    pub teb: u32,
    pub stack_lo: u32,
    pub stack_hi: u32,
    /// Initial ESP/EIP, consumed the first time the context is scheduled.
    pub(crate) start: Option<(u32, u32)>,
    pub(crate) frames: Vec<Frame>,
    pub(crate) state: State,
    /// Its last block was a yield (Sleep(0)): it is polling, not waiting on anything.
    pub(crate) yielded: bool,
    /// Kernel object signaled when this context ends.
    pub thread_handle: Handle,
}

impl Context {
    /// Read-only scheduler diagnostics for frontends and failure reports.
    pub fn state_name(&self) -> &'static str {
        match self.state {
            State::Running { .. } => "Running",
            State::Ready { .. } => "Ready",
            State::Blocked { .. } => "Blocked",
            State::Done { .. } => "Done",
        }
    }

    pub fn waiting_on(&self) -> Option<&Wait> {
        match &self.state {
            State::Blocked { wait, .. } => Some(wait),
            _ => None,
        }
    }

    /// Host call a blocked context is inside (or has just been woken from).
    pub fn blocked_thunk(&self) -> Option<u32> {
        match &self.state {
            State::Blocked { origin, .. } => Some(origin.thunk),
            State::Ready {
                resume: Some(resume),
            } => Some(resume.origin().thunk),
            _ => None,
        }
    }

    pub fn blocked_args(&self) -> Option<&[u32]> {
        match &self.state {
            State::Blocked { origin, .. } => origin.trace_args.as_deref(),
            State::Ready {
                resume: Some(resume),
            } => resume.origin().trace_args.as_deref(),
            _ => None,
        }
    }

    /// Stack address of the parked host call's guest return address.
    pub fn blocked_stack(&self) -> Option<u32> {
        match &self.state {
            State::Blocked { origin, .. } => Some(origin.esp),
            State::Ready {
                resume: Some(resume),
            } => Some(resume.origin().esp),
            _ => None,
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.state, State::Done { .. })
    }
    /// Observe readiness without changing scheduler state or selecting a context.
    pub fn is_ready(&self) -> bool {
        matches!(self.state, State::Ready { .. })
    }

    pub fn exit_code(&self) -> Option<u32> {
        match self.state {
            State::Done { exit_code } => Some(exit_code),
            _ => None,
        }
    }
}

/// A kernel object. The scheduler waits on the first four; everything else a higher layer
/// keeps behind a handle (files, enumerations, sections, ...) is a [`Object::Host`] payload,
/// dropped when its last handle closes.
pub enum Object {
    Event {
        manual_reset: bool,
        signaled: bool,
    },
    Thread {
        ctx: CtxId,
    },
    Semaphore {
        count: u32,
        max: u32,
    },
    Mutex {
        owner: Option<CtxId>,
        recursion: u32,
    },
    /// Owned by a higher layer; never waitable here.
    Host(Box<dyn Any>),
}

impl std::fmt::Debug for Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Event {
                manual_reset,
                signaled,
            } => f
                .debug_struct("Event")
                .field("manual_reset", manual_reset)
                .field("signaled", signaled)
                .finish(),
            Self::Thread { ctx } => f.debug_struct("Thread").field("ctx", ctx).finish(),
            Self::Semaphore { count, max } => f
                .debug_struct("Semaphore")
                .field("count", count)
                .field("max", max)
                .finish(),
            Self::Mutex { owner, recursion } => f
                .debug_struct("Mutex")
                .field("owner", owner)
                .field("recursion", recursion)
                .finish(),
            Self::Host(_) => f.write_str("Host(..)"),
        }
    }
}

/// Whether `handle` is signaled for `waiter`. With `consume`, acquiring it has its side
/// effect (auto-reset event cleared, semaphore decremented, mutex owned).
pub(crate) fn poll_object(
    objects: &mut Objects,
    contexts: &[Context],
    handle: Handle,
    waiter: CtxId,
    consume: bool,
) -> bool {
    match objects.get_mut(handle) {
        Some(Object::Semaphore { count, .. }) => {
            let s = *count > 0;
            if s && consume {
                *count -= 1;
            }
            s
        }
        Some(Object::Mutex { owner, recursion }) => {
            let s = owner.is_none_or(|o| o == waiter || contexts[o as usize].is_done());
            if s && consume {
                if *owner != Some(waiter) {
                    *recursion = 0;
                }
                *owner = Some(waiter);
                *recursion += 1;
            }
            s
        }
        Some(Object::Event {
            manual_reset,
            signaled,
        }) => {
            let s = *signaled;
            if s && consume && !*manual_reset {
                *signaled = false;
            }
            s
        }
        Some(Object::Thread { ctx }) => contexts[*ctx as usize].is_done(),
        _ => false,
    }
}

/// WaitAll must check the entire set before modifying any object. In particular,
/// an auto-reset signal remains available to another waiter until the set is ready.
pub(crate) fn poll_objects(
    objects: &mut Objects,
    contexts: &[Context],
    handles: &[Handle],
    waiter: CtxId,
    all: bool,
) -> Option<WakeReason> {
    let abandoned = |objects: &Objects, handle| {
        matches!(objects.get(handle), Some(Object::Mutex { owner: Some(owner), .. })
            if *owner != waiter && contexts[*owner as usize].is_done())
    };
    if all {
        let mut identities = std::collections::HashSet::new();
        if handles.iter().any(|&handle| {
            objects
                .identity(handle)
                .is_none_or(|id| !identities.insert(id))
        }) {
            return None;
        }
        if !handles
            .iter()
            .all(|&h| poll_object(objects, contexts, h, waiter, false))
        {
            return None;
        }
        let first_abandoned = handles.iter().position(|&h| abandoned(objects, h));
        for &h in handles {
            poll_object(objects, contexts, h, waiter, true);
        }
        Some(WakeReason::Objects {
            index: first_abandoned.unwrap_or(0) as u32,
            abandoned: first_abandoned.is_some(),
        })
    } else {
        for (index, &h) in handles.iter().enumerate() {
            let abandoned = abandoned(objects, h);
            if poll_object(objects, contexts, h, waiter, true) {
                return Some(WakeReason::Objects {
                    index: index as u32,
                    abandoned,
                });
            }
        }
        None
    }
}
