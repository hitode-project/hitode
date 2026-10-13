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
    /// Its last block was a yield (Sleep(0)): it is polling, not waiting on anything.
    pub(crate) yielded: bool,
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
        assert_eq!(machine.sys.scheduler_work.polls.get(), 1);
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
