//! Guest contexts ("threads") and the cooperative scheduler.
//!
//! PAL runs three OS threads: main/UI, game, and work. Their only interaction is
//! flag polling with `Sleep(1)`, events, and critical sections. We run them as guest contexts
//! on one host thread. Blocking switches contexts; real-time slices can also preempt
//! guest computation or return a completed frame to the host. The normal scheduling
//! lane remains round-robin, with a bounded preference for genuinely satisfied waits.

use std::collections::HashMap;

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
}

/// A guest call in progress on behalf of a host call.
pub(crate) struct Frame {
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
    /// Runnable. `resume` completes a satisfied guest wait or deferred host work before
    /// the guest continues. Only satisfied waits enter the wake-priority queue.
    Ready {
        resume: Option<ReadyResume>,
    },
    Blocked {
        wait: Wait,
        then: Continuation,
        origin: Origin,
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

#[derive(Clone, Debug)]
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
    /// Owned by the win32/pal layers (files, heaps, ...); never waitable here.
    Opaque(u32),
}

#[derive(Default)]
pub struct Objects {
    map: HashMap<Handle, Object>,
    targets: HashMap<Handle, Handle>,
    references: HashMap<Handle, u32>,
    next: Handle,
}

impl Objects {
    pub fn insert(&mut self, o: Object) -> Handle {
        // Handles are multiples of 4, like NT's, starting well above small integers.
        self.next = self.next.max(0x100) + 4;
        self.map.insert(self.next, o);
        self.targets.insert(self.next, self.next);
        self.references.insert(self.next, 1);
        self.next
    }
    pub fn get(&self, h: Handle) -> Option<&Object> {
        self.map.get(self.targets.get(&h)?)
    }
    pub fn get_mut(&mut self, h: Handle) -> Option<&mut Object> {
        self.map.get_mut(self.targets.get(&h)?)
    }
    pub fn remove(&mut self, h: Handle) -> Option<Object> {
        let object = self.targets.remove(&h)?;
        let references = self.references.get_mut(&object).unwrap();
        *references -= 1;
        if *references == 0 {
            self.references.remove(&object);
            self.map.remove(&object)
        } else {
            self.map.get(&object).cloned()
        }
    }
    pub fn identity(&self, h: Handle) -> Option<Handle> {
        self.targets.get(&h).copied()
    }
    pub fn has_identity(&self, object: Handle) -> bool {
        self.references.contains_key(&object)
    }
    pub fn same_object(&self, first: Handle, second: Handle) -> bool {
        self.identity(first)
            .is_some_and(|object| self.identity(second) == Some(object))
    }
    /// Another live handle to the same retained object, including shared state.
    pub fn alias_identity(&mut self, object: Handle) -> Option<Handle> {
        let references = self.references.get_mut(&object)?;
        let count = references.checked_add(1)?;
        let next = self.next.checked_add(4)?;
        *references = count;
        self.next = next;
        self.targets.insert(next, object);
        Some(next)
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
