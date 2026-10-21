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

/// Live scheduler entries, separate from the append-only context identity table.
/// WakeQueue uses reusable execution slots, so its scans and stamps stay bounded
/// by the execution pool rather than the number of threads ever created.
#[derive(Default)]
pub(crate) struct ActiveContexts {
    entries: Vec<ActiveContext>,
    slots: Vec<Option<CtxId>>,
    queue: crate::wake_queue::WakeQueue,
}

pub(crate) struct ActiveContext {
    pub id: CtxId,
    pub slot: u32,
    pub ran: bool,
    pub polled: bool,
}

impl ActiveContexts {
    pub fn entries(&self) -> &[ActiveContext] {
        &self.entries
    }

    pub fn insert(&mut self, id: CtxId, slot: u32) {
        assert!(slot < crate::layout::MAX_CONTEXTS);
        self.slots
            .resize(self.slots.len().max(slot as usize + 1), None);
        assert!(self.slots[slot as usize].replace(id).is_none());
        self.entries.push(ActiveContext {
            id,
            slot,
            ran: false,
            polled: false,
        });
    }

    pub fn retire(&mut self, id: CtxId) -> bool {
        let Ok(index) = self.entries.binary_search_by_key(&id, |entry| entry.id) else {
            return false;
        };
        let slot = self.entries[index].slot;
        self.queue.retire(slot);
        self.slots[slot as usize] = None;
        self.entries.retain(|entry| entry.id != id);
        true
    }

    pub fn wake(&mut self, id: CtxId, observed_us: u64, deadline_us: Option<u64>) {
        if let Ok(index) = self.entries.binary_search_by_key(&id, |entry| entry.id) {
            self.queue
                .wake(self.entries[index].slot, observed_us, deadline_us);
        }
    }

    pub fn wake_as(
        &mut self,
        id: CtxId,
        observed_us: u64,
        deadline_us: Option<u64>,
        priority: crate::WakePriority,
    ) {
        if let Ok(index) = self.entries.binary_search_by_key(&id, |entry| entry.id) {
            self.queue
                .wake_as(self.entries[index].slot, observed_us, deadline_us, priority);
        }
    }

    pub fn pick(
        &mut self,
        after: &mut Option<u32>,
        count: usize,
        ready: impl Fn(CtxId) -> bool,
        prefer_wakes: bool,
    ) -> Option<crate::wake_queue::Selection> {
        let slots = &self.slots;
        let mut selection = self.queue.pick(
            after,
            count.min(slots.len()),
            |slot| slots[slot as usize].is_some_and(&ready),
            prefer_wakes,
        )?;
        selection.id = slots[selection.id as usize].expect("selected slot owns a live context");
        Some(selection)
    }

    pub fn begin_slice(&mut self) {
        for entry in &mut self.entries {
            entry.ran = false;
            entry.polled = false;
        }
    }

    pub fn mark_turn(&mut self, id: CtxId, yielded: bool) {
        let index = self
            .entries
            .binary_search_by_key(&id, |entry| entry.id)
            .expect("selected context is active");
        self.entries[index].ran = true;
        self.entries[index].polled = yielded;
    }

    pub fn clear_polled(&mut self) {
        for entry in &mut self.entries {
            entry.polled = false;
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct SchedulerWork {
    pub polls: std::cell::Cell<usize>,
    pub selections: std::cell::Cell<usize>,
    pub scratch: std::cell::Cell<usize>,
}

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

/// The Windows timer period a game gets from timeBeginPeriod(1).
pub(crate) const TICK_US: u64 = 1000;

/// The first timer tick at or after `us`.
pub(crate) fn next_tick(us: u64) -> u64 {
    us.div_ceil(TICK_US).saturating_mul(TICK_US)
}

impl Wait {
    pub(crate) fn deadline(&self) -> Option<u64> {
        match self {
            Self::Until(at) => Some(*at),
            Self::Object { deadline, .. } | Self::Objects { deadline, .. } => *deadline,
            Self::Yield => None,
        }
    }

    /// Expire on the next timer tick at or after the deadline (PROC-CLOCK-6).
    pub(crate) fn round_to_tick(&mut self) {
        if let Self::Until(at)
        | Self::Object {
            deadline: Some(at), ..
        }
        | Self::Objects {
            deadline: Some(at), ..
        } = self
        {
            *at = next_tick(*at);
        }
    }

    pub(crate) fn objects(&self) -> (&[Handle], Option<Handle>) {
        match self {
            Self::Object { handle, .. } => (std::slice::from_ref(handle), None),
            Self::Objects {
                handles,
                notification,
                ..
            } => (handles, *notification),
            _ => (&[], None),
        }
    }
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
    /// Abandoned, not failed, when the guest unwinds past `stack`.
    pub unwindable: bool,
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
        allow_time_skip: bool,
    },
    Done {
        exit_code: u32,
    },
}

pub struct Context {
    /// Last audio trap, measured by the same guest clock as wait deadlines.
    pub audio_call_us: Option<u64>,
    /// Guest audio reference calls conservatively protect this thread for its lifetime.
    pub audio_guest: Option<crate::GuestWord>,
    pub suspend_count: u32,
    pub completion: std::sync::Arc<crate::process::Completion>,
    pub id: CtxId,
    pub name: String,
    pub tid: u32,
    pub teb: u32,
    pub stack_lo: u32,
    pub stack_hi: u32,
    /// Initial ESP/EIP, consumed when the register image is first materialized.
    pub(crate) start: Option<(u32, u32)>,
    pub(crate) initial_registers: Vec<(adv_cpu::backend::Reg, u32)>,
    /// Execution storage is reclaimable independently of the thread identity.
    pub(crate) storage: Option<ContextStorage>,
    /// One-shot process bootstrap, consumed only by an actual scheduler selection.
    /// The binding identifies its continuation origin, independently of mutable EIP.
    pub(crate) bootstrap_pending: Option<u32>,
    pub(crate) frames: Vec<Frame>,
    pub(crate) state: State,
    pub(crate) wait_objects: Vec<u64>,
    /// Its last block was a yield (Sleep(0)): it is polling, not waiting on anything.
    pub(crate) yielded: bool,
    /// Its next timed wait keeps its exact deadline instead of the next tick.
    pub(crate) exact_deadline: bool,
    pub(crate) spin: crate::frame_spin::FrameSpin,
    pub(crate) progress: crate::poll_progress::PollProgress,
    /// Kernel object signaled when this context ends.
    pub thread_handle: Handle,
}

pub(crate) struct ContextStorage {
    pub slot: u32,
    pub stack: u32,
    pub tls_directory: u32,
    pub tls_blocks: Vec<(u32, u32)>,
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
        self.suspend_count == 0 && matches!(self.state, State::Ready { .. })
    }

    pub fn exit_code(&self) -> Option<u32> {
        match self.state {
            State::Done { exit_code } => Some(exit_code),
            _ => None,
        }
    }
}

/// A kernel object. The scheduler waits on synchronization variants; everything
/// else a higher layer keeps behind a handle (files, enumerations, sections, ...)
/// is a [`Object::Host`] payload, dropped when its last handle closes.
pub enum Object {
    Process {
        identity: std::sync::Arc<crate::process::ProcessIdentity>,
        access: u32,
    },
    RemoteThread {
        identity: crate::process::ThreadIdentity,
        access: u32,
    },
    Event {
        manual_reset: bool,
        signaled: bool,
    },
    Timer {
        manual_reset: bool,
        signaled: bool,
        deadline: Option<u64>,
        period_us: u64,
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

impl Object {
    pub(crate) fn completion(&self) -> Option<&crate::process::Completion> {
        match self {
            Self::Process { identity, .. } => Some(&identity.completion),
            Self::RemoteThread { identity, .. } => Some(&identity.completion),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Object {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Process { identity, access } => f
                .debug_tuple("Process")
                .field(&identity.pid)
                .field(access)
                .finish(),
            Self::RemoteThread { identity, access } => f
                .debug_tuple("RemoteThread")
                .field(&identity.tid)
                .field(access)
                .finish(),
            Self::Event {
                manual_reset,
                signaled,
            } => f
                .debug_struct("Event")
                .field("manual_reset", manual_reset)
                .field("signaled", signaled)
                .finish(),
            Self::Thread { ctx } => f.debug_struct("Thread").field("ctx", ctx).finish(),
            Self::Timer {
                manual_reset,
                signaled,
                deadline,
                period_us,
            } => f
                .debug_struct("Timer")
                .field("manual_reset", manual_reset)
                .field("signaled", signaled)
                .field("deadline", deadline)
                .field("period_us", period_us)
                .finish(),
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
        Some(Object::Process { identity, .. }) => identity.completion.exit_code().is_some(),
        Some(Object::RemoteThread { identity, .. }) => identity.completion.exit_code().is_some(),
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
        Some(
            Object::Event {
                manual_reset,
                signaled,
            }
            | Object::Timer {
                manual_reset,
                signaled,
                ..
            },
        ) => {
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

#[cfg(test)]
mod active_tests {
    use crate::*;
    use adv_cpu::JitCpu;

    struct Empty;
    impl Resolver for Empty {
        fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
            None
        }
        fn provides(&self, _: &str) -> bool {
            false
        }
    }

    fn machine() -> Machine<JitCpu> {
        Machine::new(
            JitCpu::new(layout::PARK_ADDR, false).unwrap(),
            Empty,
            Clock::real(),
        )
        .unwrap()
    }

    fn wait_sys() -> Sys {
        Sys {
            clock: Clock::virtual_time(),
            invocation_arguments: None,
            trace: Trace::default(),
            thunks: ThunkTable::new(Box::new(Empty)),
            objects: Objects::default(),
            vm: Vm::new(),
            contexts: Vec::new(),
            guest_words: GuestWordPool::default(),
            current: 0,
            frame: 0,
            completed_frames: 0,
            frame_pacer: None,
            pid: 0x1000,
            process_identity: process::ProcessIdentity::new(0x1000),
            next_tid: 0x1004,
            images: Images::default(),
            tls_images: Vec::new(),
            process_init: VecDeque::new(),
            process_ready: Vec::new(),
            process_entry: None,
            process_start_entry: None,
            guest_return_entry: thunk_addr(thunk::RETURN_PAD),
            thread_exception_entry: None,
            process_return: None,
            process_prepare: None,
            context_wait: None,
            exception_handler: None,
            first_cpu_fault: None,
            latest_cpu_fault: None,
            guest_failure_code: None,
            checkpoint_identity: std::sync::Arc::new(()),
            next_frame_id: 0,
            recent: RecentCalls::new(256),
            host_calls: 0,
            call_limit: None,
            stats: Stats::new(),
            schedule_trace: schedule_trace::ScheduleTrace::default(),
            scheduling: scheduling::SchedulingStats::default(),
            wakes: sched::ActiveContexts::default(),
            wait_deadlines: Default::default(),
            finished_contexts: Vec::new(),
            rr_last: None,
            scheduler_work: sched::SchedulerWork::default(),
        }
    }

    fn block(sys: &mut Sys, id: CtxId, wait: Wait) {
        sys.contexts[id as usize].state = super::State::Blocked {
            wait,
            then: Box::new(|_, _| Outcome::Return(0)),
            origin: super::Origin {
                esp: 0,
                ret_eip: 0,
                thunk: 0,
                trace_args: None,
                interrupted: false,
            },
            allow_time_skip: true,
        };
        sys.register_waiter(id as usize);
    }

    fn event(sys: &mut Sys) -> Handle {
        sys.objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        })
    }

    fn spawn(sys: &mut Sys) -> CtxId {
        let id = sys.contexts.len() as CtxId;
        let thread_handle = sys.objects.insert(Object::Thread { ctx: id });
        sys.contexts.push(super::Context {
            audio_call_us: None,
            audio_guest: None,
            suspend_count: 0,
            completion: std::sync::Arc::new(crate::process::Completion::default()),
            id,
            name: "waiter".into(),
            tid: id,
            teb: 0,
            stack_lo: 0,
            stack_hi: 0,
            start: None,
            initial_registers: Vec::new(),
            storage: None,
            bootstrap_pending: None,
            frames: Vec::new(),
            state: super::State::Ready { resume: None },
            yielded: false,
            exact_deadline: false,
            spin: Default::default(),
            progress: Default::default(),
            thread_handle,
            wait_objects: Vec::new(),
        });
        sys.wakes.insert(id, id);
        id
    }

    // spec: PROC-MACH-10
    #[test]
    fn auto_reset_releases_three_waiters_in_wait_order_across_aliases() {
        let mut sys = wait_sys();
        let h = event(&mut sys);
        let alias = sys.objects.duplicate(h).unwrap();
        for _ in 0..3 {
            spawn(&mut sys);
        }
        for id in [2, 0, 1] {
            block(
                &mut sys,
                id,
                Wait::Object {
                    handle: h,
                    deadline: None,
                },
            );
        }
        for (turn, id) in [2, 0, 1].into_iter().enumerate() {
            assert!(sys.set_event(alias));
            assert!(sys.context(id).is_ready());
            assert_eq!(
                sys.contexts.iter().filter(|c| c.is_ready()).count(),
                turn + 1
            );
            assert!(!sys.try_acquire(h));
        }
    }

    // spec: PROC-MACH-10
    #[test]
    fn wait_all_acquires_atomically_when_its_last_member_changes() {
        let mut sys = wait_sys();
        let id = spawn(&mut sys);
        let handles = [event(&mut sys), event(&mut sys)];
        block(
            &mut sys,
            id,
            Wait::Objects {
                handles: handles.to_vec(),
                all: true,
                deadline: None,
                notification: None,
            },
        );
        sys.set_event(handles[0]);
        assert_eq!(sys.context(id).state_name(), "Blocked");
        assert!(matches!(
            sys.objects.get(handles[0]),
            Some(Object::Event { signaled: true, .. })
        ));
        sys.set_event(handles[1]);
        assert!(sys.context(id).is_ready());
        for h in handles {
            assert!(!sys.try_acquire(h));
        }
        assert!(sys.contexts[id as usize].wait_objects.is_empty());
    }

    // spec: PROC-MACH-10
    #[test]
    fn nonblocking_acquires_expire_timers_without_evaluating_other_waits() {
        let mut sys = wait_sys();
        let now = sys.clock.now_us();
        let id = spawn(&mut sys);
        let other = event(&mut sys);
        block(
            &mut sys,
            id,
            Wait::Object {
                handle: other,
                deadline: Some(now + 10),
            },
        );
        let timer = sys.objects.insert(Object::Timer {
            manual_reset: false,
            signaled: false,
            deadline: None,
            period_us: 0,
        });
        assert!(sys.arm_timer(timer, now + 10, 10));
        sys.clock.advance_to(now + 10);
        sys.scheduler_work.polls.set(0);
        assert!(sys.try_acquire(timer));
        assert_eq!(sys.scheduler_work.polls.get(), 0);
        assert_eq!(sys.context(id).state_name(), "Blocked");
        sys.clock.advance_to(now + 20);
        sys.scheduler_work.polls.set(0);
        assert!(sys.try_acquire_objects(&[timer], true).is_some());
        assert_eq!(sys.scheduler_work.polls.get(), 0);
        assert_eq!(sys.context(id).state_name(), "Blocked");
        sys.wake();
        assert!(sys.context(id).is_ready());
    }

    // spec: PROC-MACH-10
    #[test]
    fn unchanged_pass_evaluates_no_waits() {
        let mut sys = wait_sys();
        let id = spawn(&mut sys);
        let h = event(&mut sys);
        block(
            &mut sys,
            id,
            Wait::Object {
                handle: h,
                deadline: None,
            },
        );
        sys.scheduler_work.polls.set(0);
        sys.wake();
        assert_eq!(sys.scheduler_work.polls.get(), 0);
        assert_eq!(sys.context(id).state_name(), "Blocked");
    }

    // spec: PROC-MACH-10
    #[test]
    fn finished_owner_abandons_mutex_to_its_waiter() {
        let mut sys = wait_sys();
        let owner = spawn(&mut sys);
        let waiter = spawn(&mut sys);
        let h = sys.objects.insert(Object::Mutex {
            owner: Some(owner),
            recursion: 1,
        });
        block(
            &mut sys,
            waiter,
            Wait::Objects {
                handles: vec![h],
                all: true,
                deadline: None,
                notification: None,
            },
        );
        assert!(sys.terminate_context_stopped(owner, 0));
        sys.wake();
        assert!(matches!(
            sys.contexts[waiter as usize].state,
            super::State::Ready {
                resume: Some(super::ReadyResume::Woke {
                    reason: WakeReason::Objects {
                        index: 0,
                        abandoned: true
                    },
                    ..
                })
            }
        ));
        assert!(
            matches!(sys.objects.get(h), Some(Object::Mutex { owner: Some(id), recursion: 1 }) if *id == waiter)
        );
    }

    // spec: PROC-MACH-10, PROC-MACH-2
    #[test]
    fn deadlines_and_registrations_retire_on_notification_timeout_and_termination() {
        let mut sys = wait_sys();
        let now = sys.clock.now_us();
        let id = spawn(&mut sys);
        let h = event(&mut sys);
        let alias = sys.objects.duplicate(h).unwrap();
        let notification = event(&mut sys);
        block(
            &mut sys,
            id,
            Wait::Objects {
                handles: vec![h],
                all: true,
                deadline: Some(now + 10),
                notification: Some(notification),
            },
        );
        sys.objects.close(h).unwrap();
        assert_eq!(sys.next_deadline(), Some(now + 10));
        sys.set_event(notification);
        assert!(matches!(
            sys.contexts[id as usize].state,
            super::State::Ready {
                resume: Some(super::ReadyResume::Woke {
                    reason: WakeReason::Notified,
                    ..
                })
            }
        ));
        assert_eq!(sys.next_deadline(), None);
        sys.scheduler_work.polls.set(0);
        sys.set_event(alias);
        assert_eq!(sys.scheduler_work.polls.get(), 0);
        sys.reset_event(alias);
        block(
            &mut sys,
            id,
            Wait::Object {
                handle: alias,
                deadline: Some(now + 20),
            },
        );
        sys.clock.advance_to(now + 20);
        sys.wake();
        assert!(sys.context(id).is_ready());
        assert_eq!(sys.next_deadline(), None);
        block(
            &mut sys,
            id,
            Wait::Object {
                handle: alias,
                deadline: Some(now + 30),
            },
        );
        assert!(sys.terminate_context_stopped(id, 0));
        assert_eq!(sys.next_deadline(), None);
        sys.scheduler_work.polls.set(0);
        sys.set_event(alias);
        sys.wake();
        assert_eq!(sys.scheduler_work.polls.get(), 0);
        assert!(sys.contexts[id as usize].wait_objects.is_empty());
    }

    // spec: PROC-MACH-10, PROC-HANDLE-1
    #[test]
    fn shared_process_and_thread_completions_mark_their_waiters_changed() {
        let mut sys = wait_sys();
        let process = crate::process::ProcessIdentity::new(42);
        let thread = std::sync::Arc::new(crate::process::Completion::default());
        let handles = [
            sys.objects.insert(Object::Process {
                identity: process.clone(),
                access: 0x100000,
            }),
            sys.objects.insert(Object::RemoteThread {
                identity: crate::process::ThreadIdentity {
                    process: process.clone(),
                    context: 0,
                    tid: 43,
                    completion: thread.clone(),
                },
                access: 0x100000,
            }),
        ];
        for h in handles {
            let id = spawn(&mut sys);
            block(
                &mut sys,
                id,
                Wait::Object {
                    handle: h,
                    deadline: None,
                },
            );
        }
        process.completion.complete(0);
        sys.wake();
        assert!(sys.context(0).is_ready());
        assert_eq!(sys.context(1).state_name(), "Blocked");
        thread.complete(0);
        sys.wake();
        assert!(sys.context(1).is_ready());
    }

    fn churn(machine: &mut Machine<JitCpu>, count: u32) -> Vec<(CtxId, Handle, u32, u32)> {
        let mut retained = Vec::new();
        for code in 1..=count {
            let id = machine
                .spawn("short", layout::PARK_ADDR, &[], 65536)
                .unwrap();
            let ctx = machine.sys.context(id);
            let (handle, tid) = (ctx.thread_handle, ctx.tid);
            assert!(machine.sys.terminate_context_stopped(id, code));
            if code == 1 || code == count / 2 || code == count {
                retained.push((id, handle, tid, code));
            } else {
                assert!(machine.sys.objects.close(handle).is_some());
            }
        }
        machine
            .sys
            .reclaim_finished_storage(&mut machine.cpu)
            .unwrap();
        retained
    }

    #[test]
    fn scheduler_work_is_bounded_after_sequential_thread_exits() {
        let mut machine = machine();
        let keeper = machine
            .spawn("keeper", layout::PARK_ADDR, &[], 65536)
            .unwrap();
        assert_eq!(machine.sys.suspend_context(keeper), Some(0));
        let retained = churn(&mut machine, 4096);
        machine.sys.scheduler_work = Default::default();
        assert!(matches!(
            machine.run_slice(Duration::from_secs(1)),
            SliceResult::Complete(RunResult::Idle {
                next_deadline: None
            })
        ));
        for (id, handle, tid, code) in retained {
            let context = machine.sys.context(id);
            assert_eq!(context.id, id);
            assert_eq!(context.tid, tid);
            assert_eq!(context.exit_code(), Some(code));
            assert!(
                matches!(machine.sys.objects.get(handle), Some(Object::Thread { ctx }) if *ctx == id)
            );
            assert_eq!(
                machine
                    .sys
                    .thread_identity(id)
                    .unwrap()
                    .completion
                    .exit_code(),
                Some(code)
            );
            assert!(machine.sys.try_acquire(handle));
            assert_eq!(
                machine.sys.try_acquire_objects(&[handle], true),
                Some(WakeReason::Objects {
                    index: 0,
                    abandoned: false
                })
            );
        }
        // One slice and six handle waits should inspect only the one live context.
        assert!(
            machine.sys.scheduler_work.polls.get() <= 7,
            "poll visits: {}",
            machine.sys.scheduler_work.polls.get()
        );
        assert!(
            machine.sys.scheduler_work.selections.get() <= 2,
            "selection visits: {}",
            machine.sys.scheduler_work.selections.get()
        );
        assert!(
            machine.sys.scheduler_work.scratch.get() <= 2,
            "slice scratch entries: {}",
            machine.sys.scheduler_work.scratch.get()
        );
        assert_eq!(machine.sys.contexts.len(), 4097);
        assert_eq!(machine.sys.wakes.entries().len(), 1);
        assert!(machine.sys.finished_contexts.is_empty());
        assert!(machine.sys.wakes.slots.len() <= layout::MAX_CONTEXTS as usize);
    }

    // spec: PROC-MACH-10
    #[test]
    fn sparse_execution_slots_do_not_visit_retired_contexts() {
        let mut machine = machine();
        for _ in 0..layout::MAX_CONTEXTS {
            machine
                .spawn("live", layout::PARK_ADDR, &[], 65536)
                .unwrap();
        }
        let keeper = layout::MAX_CONTEXTS - 1;
        machine.sys.suspend_context(keeper).unwrap();
        for id in 0..keeper {
            assert!(machine.sys.terminate_context_stopped(id, id));
        }
        machine
            .sys
            .reclaim_finished_storage(&mut machine.cpu)
            .unwrap();
        machine.sys.scheduler_work = Default::default();
        assert!(matches!(
            machine.run_slice(Duration::from_secs(1)),
            SliceResult::Complete(RunResult::Idle { .. })
        ));
        assert_eq!(machine.sys.scheduler_work.polls.get(), 0);
        assert_eq!(machine.sys.scheduler_work.selections.get(), 2);
        assert_eq!(machine.sys.scheduler_work.scratch.get(), 0);
        assert_eq!(machine.sys.wakes.entries().len(), 1);
        let replacement = machine
            .spawn("replacement", layout::PARK_ADDR, &[], 65536)
            .unwrap();
        assert_eq!(
            machine
                .sys
                .context(replacement)
                .storage
                .as_ref()
                .unwrap()
                .slot,
            0
        );
        assert_eq!(machine.sys.wakes.entries().len(), 2);
    }

    // spec: PROC-MACH-2
    #[test]
    fn reused_slots_keep_wake_identity_and_normal_progress() {
        let mut active = super::ActiveContexts::default();
        for slot in 0..3 {
            active.insert(10_000 + slot, slot);
        }
        active.wake_as(10_001, 0, Some(0), WakePriority::Audio);
        active.mark_turn(10_001, true);
        assert!(active.retire(10_001));
        active.insert(20_000, 1);
        let replacement = active.entries().last().unwrap();
        assert!(!replacement.ran && !replacement.polled);
        active.wake(10_002, 5, Some(3));
        let mut cursor = None;
        let pick = active
            .pick(&mut cursor, layout::MAX_CONTEXTS as usize, |_| true, true)
            .unwrap();
        assert_eq!(pick.id, 10_002);
        assert!(pick.promoted);
        assert_eq!(pick.wake.unwrap().deadline_us, Some(3));
        assert_eq!(cursor, None);
        assert_eq!(
            active
                .pick(&mut cursor, layout::MAX_CONTEXTS as usize, |_| true, true)
                .unwrap()
                .id,
            10_000
        );
        let pick = active
            .pick(&mut cursor, layout::MAX_CONTEXTS as usize, |_| true, true)
            .unwrap();
        assert_eq!(pick.id, 20_000);
        assert!(pick.wake.is_none() && !pick.promoted);
        let mut counts = [0; 3];
        let mut streak = 0;
        for time in 0..900 {
            active.wake_as(10_000, time, None, WakePriority::Audio);
            let pick = active
                .pick(&mut cursor, layout::MAX_CONTEXTS as usize, |_| true, true)
                .unwrap();
            let slot = active
                .entries()
                .iter()
                .find(|entry| entry.id == pick.id)
                .unwrap()
                .slot;
            counts[slot as usize] += 1;
            streak = if pick.promoted { streak + 1 } else { 0 };
            assert!(streak <= 8);
        }
        assert!(counts[1] >= 50 && counts[2] >= 50, "{counts:?}");
    }

    #[test]
    fn parked_thread_and_process_exits_retire_the_active_index() {
        let mut machine = machine();
        for _ in 0..3 {
            machine
                .spawn("live", layout::PARK_ADDR, &[], 65536)
                .unwrap();
        }
        machine.sys.current = 1;
        assert!(machine.park(crate::Parked::ExitThread(7)).is_none());
        assert_eq!(machine.sys.wakes.entries().len(), 2);
        machine.sys.current = 2;
        assert!(matches!(
            machine.park(crate::Parked::ExitProcess(21)),
            Some(RunResult::Exited(21))
        ));
        assert!(machine.sys.wakes.entries().is_empty());
        assert_eq!(machine.sys.finished_contexts.len(), 3);
        machine
            .sys
            .reclaim_finished_storage(&mut machine.cpu)
            .unwrap();
        assert!(machine.sys.finished_contexts.is_empty());
        assert_eq!(machine.sys.context(1).exit_code(), Some(7));
        for id in [0, 2] {
            assert_eq!(machine.sys.context(id).exit_code(), Some(21));
        }
        assert!(
            machine
                .sys
                .contexts
                .iter()
                .all(|context| context.storage.is_none())
        );
    }

    #[test]
    fn failed_reclaim_keeps_pending_storage_for_retry() {
        let mut machine = machine();
        machine
            .spawn("keeper", layout::PARK_ADDR, &[], 65536)
            .unwrap();
        let retired = machine
            .spawn("short", layout::PARK_ADDR, &[], 65536)
            .unwrap();
        assert!(machine.sys.terminate_context_stopped(retired, 9));
        let storage = machine.sys.contexts[retired as usize]
            .storage
            .as_mut()
            .unwrap();
        let directory = storage.tls_directory;
        storage.tls_directory = layout::PARK_ADDR;
        assert!(
            machine
                .spawn("failed", layout::PARK_ADDR, &[], 65536)
                .is_err()
        );
        assert_eq!(machine.sys.contexts.len(), 2);
        assert_eq!(machine.sys.wakes.entries().len(), 1);
        assert_eq!(machine.sys.finished_contexts, [retired]);
        machine.sys.contexts[retired as usize]
            .storage
            .as_mut()
            .unwrap()
            .tls_directory = directory;
        let replacement = machine
            .spawn("retry", layout::PARK_ADDR, &[], 65536)
            .unwrap();
        assert_eq!(replacement, 2);
        assert!(machine.sys.finished_contexts.is_empty());
        assert!(machine.sys.context(retired).storage.is_none());
        assert_eq!(machine.sys.context(retired).exit_code(), Some(9));
    }

    #[test]
    #[ignore = "local scheduler microbenchmark; no game or timing assertion"]
    fn scheduler_slice_after_ten_thousand_exits() {
        let mut machine = machine();
        let keeper = machine
            .spawn("keeper", layout::PARK_ADDR, &[], 65536)
            .unwrap();
        machine.sys.suspend_context(keeper).unwrap();
        churn(&mut machine, 10_000);
        let slices = 10_000;
        let started = Instant::now();
        for _ in 0..slices {
            assert!(matches!(
                machine.run_slice(Duration::from_secs(1)),
                SliceResult::Complete(RunResult::Idle { .. })
            ));
        }
        eprintln!(
            "scheduler benchmark: history={} active=1 slices={} ns_per_slice={:.1}",
            machine.sys.contexts.len(),
            slices,
            started.elapsed().as_nanos() as f64 / f64::from(slices)
        );
    }
}
