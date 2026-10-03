//! One background thread, two priorities, and startup-registered SPSC owners.
//! Submission never waits. A running job cannot be preempted; Urgent work wins
//! at the next job boundary, with round-robin arbitration between its owners.
use std::cell::{Cell, RefCell};
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::TrySendError;
use std::thread::{self, JoinHandle};
use std::time::Instant;

use crate::{WakeEvent, spsc};

const MAX_PORTS: usize = 8;
const MAX_CAPACITY: usize = 64;
static NEXT_SERVICE: AtomicU64 = AtomicU64::new(1);
const CLOSED: usize = 1usize << (usize::BITS - 1);

#[derive(Clone, Copy)]
pub struct Affinity(u64);

/// Client-defined estimates, not time or a preemption guarantee. Immutable once
/// the service starts; neutral clients interpret one work unit themselves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorkPolicy {
    background_limit: Option<usize>,
}
impl WorkPolicy {
    pub fn background_limit(self) -> Option<usize> {
        self.background_limit
    }
    pub fn responsive(background_limit: usize) -> Self {
        assert!(background_limit > 0);
        Self {
            background_limit: Some(background_limit),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    Urgent,
    Background,
}
impl Priority {
    fn index(self) -> usize {
        match self {
            Self::Urgent => 0,
            Self::Background => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Status {
    Queued,
    Running,
    Completed,
    Cancelled,
    Panicked,
}

struct TaskState {
    status: AtomicU8,
    cancelable: bool,
}
#[derive(Clone)]
pub struct Ticket(Arc<TaskState>);
impl Ticket {
    pub fn status(&self) -> Status {
        match self.0.status.load(Ordering::Acquire) {
            0 => Status::Queued,
            1 => Status::Running,
            2 => Status::Completed,
            3 => Status::Cancelled,
            _ => Status::Panicked,
        }
    }
    /// Cancel a queued, discardable job only. Running and required cleanup jobs
    /// must finish. Dropping a ticket does not cancel its task.
    pub fn cancel(&self) -> bool {
        self.0.cancelable
            && self
                .0
                .status
                .compare_exchange(
                    Status::Queued as u8,
                    Status::Cancelled as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
    }
}

enum Notify {
    Execution,
    Custom(WakeEvent),
    None,
}
pub struct Task {
    run: Option<Box<dyn FnOnce() + Send + 'static>>,
    state: Arc<TaskState>,
    notify: Notify,
    affinity: Option<u64>,
}
impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task")
            .field("status", &Ticket(self.state.clone()).status())
            .finish_non_exhaustive()
    }
}
impl Task {
    pub fn new(run: impl FnOnce() + Send + 'static) -> Self {
        Self {
            run: Some(Box::new(run)),
            state: Arc::new(TaskState {
                status: AtomicU8::new(0),
                cancelable: true,
            }),
            notify: Notify::Execution,
            affinity: None,
        }
    }
    /// Once accepted, finish this task even during service shutdown. Use for
    /// GPU map cleanup or an owned window presenter that must be returned.
    pub fn finish_on_shutdown(mut self) -> Self {
        Arc::get_mut(&mut self.state)
            .expect("unsubmitted task owns its state")
            .cancelable = false;
        self
    }
    /// UI presentation can wake its own owner without interrupting guest work.
    pub fn with_wake(mut self, wake: WakeEvent) -> Self {
        self.notify = Notify::Custom(wake);
        self
    }
    /// A client that publishes and notifies its own typed completion uses this.
    pub fn without_wake(mut self) -> Self {
        self.notify = Notify::None;
        self
    }
    /// Restrict a task containing worker-local state to this exact service.
    /// Rebinding an already restricted task cannot change its owner.
    pub fn affine_to(mut self, affinity: Affinity) -> Self {
        self.affinity.get_or_insert(affinity.0);
        self
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        if self.run.is_some() {
            self.state
                .status
                .store(Status::Cancelled as u8, Ordering::Release);
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LaneStats {
    pub submitted: u64,
    pub completed: u64,
    pub cancelled: u64,
    pub panicked: u64,
    pub queued: usize,
    pub peak_queued: usize,
    pub max_queue_ns: u64,
    pub max_run_ns: u64,
}
#[derive(Default)]
struct Counts {
    submitted: AtomicU64,
    completed: AtomicU64,
    cancelled: AtomicU64,
    panicked: AtomicU64,
    queued: AtomicUsize,
    peak: AtomicUsize,
    queue_ns: AtomicU64,
    run_ns: AtomicU64,
}
impl Counts {
    fn snapshot(&self) -> LaneStats {
        LaneStats {
            submitted: self.submitted.load(Ordering::Relaxed),
            completed: self.completed.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            panicked: self.panicked.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
            peak_queued: self.peak.load(Ordering::Relaxed),
            max_queue_ns: self.queue_ns.load(Ordering::Relaxed),
            max_run_ns: self.run_ns.load(Ordering::Relaxed),
        }
    }
}
struct Shared {
    id: u64,
    started: AtomicBool,
    // Closing and in-flight publishers share one atomic gate. Once CLOSED is
    // observed without publishers, a final queue scan establishes drain.
    gate: AtomicUsize,
    stopped: AtomicBool,
    request: WakeEvent,
    completion: WakeEvent,
    counts: [Counts; 2],
    work_limit: AtomicUsize,
}
struct Packet {
    task: Task,
    submitted: Instant,
}
struct Port {
    queues: [spsc::Consumer<Packet>; 2],
    space: Arc<Space>,
}
struct Space {
    blocked: [AtomicBool; 2],
    wake: Option<WakeEvent>,
}

/// Unique producer endpoint. Move it once to its owning thread; it is not Clone
/// or Sync. Components on one execution owner may share the local Rc wrapper.
pub struct Sender {
    shared: Arc<Shared>,
    queues: [spsc::Producer<Packet>; 2],
    space: Arc<Space>,
    _owner: PhantomData<Cell<()>>,
    #[cfg(test)]
    before_publish: Option<Box<dyn FnOnce() + Send>>,
}
pub type LocalSender = Rc<RefCell<Sender>>;
impl Sender {
    pub fn into_local(self) -> LocalSender {
        Rc::new(RefCell::new(self))
    }
    pub fn execution_wake(&self) -> WakeEvent {
        self.shared.completion.clone()
    }
    pub fn affinity(&self) -> Affinity {
        Affinity(self.shared.id)
    }
    pub fn work_policy(&self) -> WorkPolicy {
        WorkPolicy {
            background_limit: std::num::NonZeroUsize::new(
                self.shared.work_limit.load(Ordering::Relaxed),
            )
            .map(std::num::NonZeroUsize::get),
        }
    }
    pub fn is_closed(&self) -> bool {
        self.shared.gate.load(Ordering::Acquire) & CLOSED != 0
            || !self.shared.started.load(Ordering::Acquire)
    }
    pub fn is_stopped(&self) -> bool {
        self.shared.stopped.load(Ordering::Acquire)
    }
    pub fn stats(&self, priority: Priority) -> LaneStats {
        self.shared.counts[priority.index()].snapshot()
    }
    pub fn try_submit(
        &mut self,
        priority: Priority,
        task: Task,
    ) -> Result<Ticket, TrySendError<Task>> {
        if self.is_closed() || task.affinity.is_some_and(|id| id != self.shared.id) {
            return Err(TrySendError::Disconnected(task));
        }
        if self
            .shared
            .gate
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |gate| {
                (gate & CLOSED == 0).then_some(gate + 1)
            })
            .is_err()
        {
            return Err(TrySendError::Disconnected(task));
        }
        #[cfg(test)]
        if let Some(hook) = self.before_publish.take() {
            hook();
        }
        let index = priority.index();
        // A single producer reserves space conservatively: once not full, only
        // the consumer can change availability before this producer's push.
        // Publish the blocked marker BEFORE the second full check, so a free
        // slot between rejection and owner sleep cannot lose its notification.
        if self.queues[index].is_full() {
            self.space.blocked[index].store(true, Ordering::Release);
            if self.queues[index].is_full() {
                self.shared.gate.fetch_sub(1, Ordering::Release);
                self.shared.request.notify();
                return Err(TrySendError::Full(task));
            }
            self.space.blocked[index].store(false, Ordering::Release);
        }
        let counts = &self.shared.counts[index];
        let state = task.state.clone();
        counts.queued.fetch_add(1, Ordering::Relaxed);
        if let Err(packet) = self.queues[index].push(Packet {
            task,
            submitted: Instant::now(),
        }) {
            counts.queued.fetch_sub(1, Ordering::Relaxed);
            self.shared.gate.fetch_sub(1, Ordering::Release);
            self.shared.request.notify();
            return Err(TrySendError::Full(packet.task));
        }
        counts.submitted.fetch_add(1, Ordering::Relaxed);
        counts
            .peak
            .fetch_max(counts.queued.load(Ordering::Relaxed), Ordering::Relaxed);
        self.shared.gate.fetch_sub(1, Ordering::Release);
        self.shared.request.notify();
        Ok(Ticket(state))
    }
}

/// Register every producer before spawning. There is no dynamic MPSC control
/// queue and no mutex shared between execution, UI and the background worker.
pub struct Builder {
    shared: Arc<Shared>,
    ports: Vec<Port>,
    started: bool,
    #[cfg(test)]
    after_empty: Option<Box<dyn FnOnce() + Send>>,
}
impl Builder {
    pub fn new(execution_wake: WakeEvent) -> std::io::Result<Self> {
        Ok(Self {
            shared: Arc::new(Shared {
                id: NEXT_SERVICE
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |id| id.checked_add(1))
                    .map_err(|_| std::io::Error::other("background affinity IDs exhausted"))?,
                started: AtomicBool::new(false),
                gate: AtomicUsize::new(0),
                stopped: AtomicBool::new(false),
                request: WakeEvent::new()?,
                completion: execution_wake,
                counts: [Counts::default(), Counts::default()],
                work_limit: AtomicUsize::new(0),
            }),
            ports: Vec::new(),
            started: false,
            #[cfg(test)]
            after_empty: None,
        })
    }
    /// Configure before start, including endpoints already registered with this
    /// builder. CPU/background clients enforce their own estimated work costs.
    pub fn with_work_policy(self, policy: WorkPolicy) -> Self {
        self.shared
            .work_limit
            .store(policy.background_limit.unwrap_or(0), Ordering::Relaxed);
        self
    }
    pub fn sender(&mut self, urgent: usize, background: usize) -> std::io::Result<Sender> {
        self.register_sender(urgent, background, None)
    }
    /// Notify an owner rejected by a full queue when its next slot becomes
    /// available. This retained event permits retry without a polling timer.
    pub fn sender_with_capacity_wake(
        &mut self,
        urgent: usize,
        background: usize,
        wake: WakeEvent,
    ) -> std::io::Result<Sender> {
        self.register_sender(urgent, background, Some(wake))
    }
    fn register_sender(
        &mut self,
        urgent: usize,
        background: usize,
        wake: Option<WakeEvent>,
    ) -> std::io::Result<Sender> {
        if self.ports.len() == MAX_PORTS
            || !(1..=MAX_CAPACITY).contains(&urgent)
            || !(1..=MAX_CAPACITY).contains(&background)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "background ports/capacity exceed their bounds",
            ));
        }
        let (urgent_tx, urgent_rx) = spsc::channel(urgent);
        let (background_tx, background_rx) = spsc::channel(background);
        let space = Arc::new(Space {
            blocked: [AtomicBool::new(false), AtomicBool::new(false)],
            wake,
        });
        self.ports.push(Port {
            queues: [urgent_rx, background_rx],
            space: space.clone(),
        });
        Ok(Sender {
            shared: self.shared.clone(),
            queues: [urgent_tx, background_tx],
            space,
            _owner: PhantomData,
            #[cfg(test)]
            before_publish: None,
        })
    }
    pub fn start(mut self) -> std::io::Result<Service> {
        if self.ports.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "background service needs an owner port",
            ));
        }
        let shared = self.shared.clone();
        let mut ports = std::mem::take(&mut self.ports);
        #[cfg(test)]
        let mut after_empty = self.after_empty.take();
        let thread = thread::Builder::new()
            .name("adv-background".into())
            .spawn(move || {
                let mut cursors = [0; 2];
                loop {
                    let closing = shared.gate.load(Ordering::Acquire) & CLOSED != 0;
                    let mut next = None;
                    for priority in [Priority::Urgent, Priority::Background] {
                        let lane = priority.index();
                        for offset in 0..ports.len() {
                            let owner = (cursors[lane] + offset) % ports.len();
                            if let Some(packet) = ports[owner].queues[lane].pop() {
                                if ports[owner].space.blocked[lane].swap(false, Ordering::AcqRel)
                                    && let Some(wake) = &ports[owner].space.wake
                                {
                                    wake.signal();
                                }
                                cursors[lane] = (owner + 1) % ports.len();
                                next = Some((priority, packet));
                                break;
                            }
                        }
                        if next.is_some() {
                            break;
                        }
                    }
                    if let Some((priority, packet)) = next {
                        execute(&shared, priority, packet, closing);
                        continue;
                    }
                    #[cfg(test)]
                    if let Some(hook) = after_empty.take() {
                        hook();
                    }
                    if shared.gate.load(Ordering::Acquire) == CLOSED {
                        // A publisher can have filled a queue after the empty
                        // arbitration scan, then released the final gate count.
                        // Acquire observes its publication, and CLOSED excludes
                        // all future publishers. Only this second scan can prove
                        // that accepted work has been drained.
                        if ports
                            .iter()
                            .all(|port| port.queues.iter().all(spsc::Consumer::is_empty))
                        {
                            break;
                        }
                        continue;
                    }
                    // All producers publish before signaling this retained event.
                    if shared.request.wait(None).is_err() {
                        shared.gate.fetch_or(CLOSED, Ordering::AcqRel);
                    }
                }
                shared.stopped.store(true, Ordering::Release);
                shared.completion.notify();
            })?;
        self.shared.started.store(true, Ordering::Release);
        self.started = true;
        Ok(Service {
            shared: self.shared.clone(),
            thread: Some(thread),
        })
    }
}
impl Drop for Builder {
    fn drop(&mut self) {
        if !self.started {
            self.shared.gate.fetch_or(CLOSED, Ordering::AcqRel);
            self.shared.stopped.store(true, Ordering::Release);
        }
    }
}

fn execute(shared: &Shared, priority: Priority, packet: Packet, closing: bool) {
    let mut task = packet.task;
    let counts = &shared.counts[priority.index()];
    counts.queued.fetch_sub(1, Ordering::Relaxed);
    let started = Instant::now();
    counts.queue_ns.fetch_max(
        started
            .duration_since(packet.submitted)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64,
        Ordering::Relaxed,
    );
    if closing && task.state.cancelable {
        task.state
            .status
            .store(Status::Cancelled as u8, Ordering::Release);
    }
    let running = task
        .state
        .status
        .compare_exchange(
            Status::Queued as u8,
            Status::Running as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok();
    let run = task.run.take().expect("accepted job is executed once");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if running {
            run();
        } else {
            drop(run);
        }
    }));
    let status = if result.is_err() {
        counts.panicked.fetch_add(1, Ordering::Relaxed);
        Status::Panicked
    } else if running {
        counts.completed.fetch_add(1, Ordering::Relaxed);
        Status::Completed
    } else {
        counts.cancelled.fetch_add(1, Ordering::Relaxed);
        Status::Cancelled
    };
    counts.run_ns.fetch_max(
        started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
        Ordering::Relaxed,
    );
    task.state.status.store(status as u8, Ordering::Release);
    // Every client payload/result and terminal status precedes notification.
    match &task.notify {
        Notify::Execution => shared.completion.signal(),
        Notify::Custom(wake) => wake.signal(),
        Notify::None => {}
    }
}

/// Lifetime owner of the one worker. Shutdown cancels queued ordinary work,
/// finishes accepted required jobs, then joins the active job. It belongs only
/// to teardown, after guest/UI submission has stopped; it is not backpressure.
pub struct Service {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl Service {
    pub fn stats(&self, priority: Priority) -> LaneStats {
        self.shared.counts[priority.index()].snapshot()
    }
    pub fn shutdown(&mut self) {
        self.shared.gate.fetch_or(CLOSED, Ordering::AcqRel);
        self.shared.request.notify();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::time::Duration;

    fn wait(ticket: &Ticket) {
        let until = Instant::now() + Duration::from_secs(5);
        while matches!(ticket.status(), Status::Queued | Status::Running) {
            assert!(
                Instant::now() < until,
                "background task never became terminal"
            );
            thread::yield_now();
        }
    }
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    #[test]
    fn registration_bounds_and_service_affinity_reject_unstarted_or_foreign_work() {
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        assert!(builder.sender(0, 8).is_err());
        assert!(builder.sender(8, MAX_CAPACITY + 1).is_err());
        let mut first = builder.sender(2, 2).unwrap();
        assert!(matches!(
            first.try_submit(Priority::Urgent, Task::new(|| {})),
            Err(TrySendError::Disconnected(_))
        ));
        for _ in 1..MAX_PORTS {
            builder.sender(1, 1).unwrap();
        }
        assert!(builder.sender(1, 1).is_err());
        let _service = builder.start().unwrap();
        let mut second_builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let mut second = second_builder.sender(1, 1).unwrap();
        let _second_service = second_builder.start().unwrap();
        let task = Task::new(|| {})
            .affine_to(first.affinity())
            .affine_to(second.affinity());
        let Err(TrySendError::Disconnected(task)) = second.try_submit(Priority::Urgent, task)
        else {
            panic!("foreign affine task was accepted")
        };
        let ticket = first.try_submit(Priority::Urgent, task).unwrap();
        wait(&ticket);
        assert_eq!(ticket.status(), Status::Completed);
    }

    #[test]
    fn startup_policy_is_visible_to_already_registered_unique_endpoints() {
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let sender = builder.sender(1, 1).unwrap();
        assert_eq!(sender.work_policy(), WorkPolicy::default());
        let _service = builder.with_work_policy(WorkPolicy::responsive(128)).start().unwrap();
        assert_eq!(sender.work_policy().background_limit(), Some(128));
    }

    #[test]
    fn urgent_precedes_background_and_same_level_owners_are_fair() {
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let mut execution = builder.sender(4, 4).unwrap();
        let mut ui = builder.sender(4, 4).unwrap();
        let _service = builder.start().unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let _release = Release(released.clone());
        let started = WakeEvent::new().unwrap();
        let signal = started.clone();
        execution
            .try_submit(
                Priority::Background,
                Task::new(move || {
                    signal.notify();
                    while !released.load(Ordering::Acquire) {
                        thread::yield_now();
                    }
                })
                .without_wake(),
            )
            .unwrap();
        assert!(started.wait(Some(Duration::from_secs(5))).unwrap());
        let positions: Arc<[AtomicUsize; 7]> =
            Arc::new(std::array::from_fn(|_| AtomicUsize::new(0)));
        let next = Arc::new(AtomicUsize::new(0));
        let mut tickets = Vec::new();
        for (sender, base) in [(&mut execution, 10), (&mut ui, 20)] {
            for offset in 0..3 {
                let positions = positions.clone();
                let next = next.clone();
                tickets.push(
                    sender
                        .try_submit(
                            Priority::Urgent,
                            Task::new(move || {
                                positions[next.fetch_add(1, Ordering::Relaxed)]
                                    .store(base + offset, Ordering::Release);
                            }),
                        )
                        .unwrap(),
                );
            }
        }
        let log = positions.clone();
        let cursor = next.clone();
        tickets.push(
            execution
                .try_submit(
                    Priority::Background,
                    Task::new(move || {
                        log[cursor.fetch_add(1, Ordering::Relaxed)].store(30, Ordering::Release);
                    }),
                )
                .unwrap(),
        );
        drop(_release);
        for ticket in tickets {
            wait(&ticket);
        }
        assert_eq!(
            positions
                .each_ref()
                .map(|position| position.load(Ordering::Acquire)),
            [10, 20, 11, 21, 12, 22, 30]
        );
    }

    #[test]
    fn full_queue_returns_the_owned_task_without_running_or_leaking_it() {
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let mut sender = builder.sender(1, 1).unwrap();
        let _service = builder.start().unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let _release = Release(released.clone());
        let started = WakeEvent::new().unwrap();
        let signal = started.clone();
        sender
            .try_submit(
                Priority::Background,
                Task::new(move || {
                    signal.notify();
                    while !released.load(Ordering::Acquire) {
                        thread::yield_now();
                    }
                })
                .without_wake(),
            )
            .unwrap();
        assert!(started.wait(Some(Duration::from_secs(5))).unwrap());
        sender
            .try_submit(Priority::Background, Task::new(|| {}))
            .unwrap();
        let resource = Arc::new(());
        let lease = resource.clone();
        let Err(TrySendError::Full(task)) =
            sender.try_submit(Priority::Background, Task::new(move || drop(lease)))
        else {
            panic!("saturated queue accepted work")
        };
        assert_eq!(Arc::strong_count(&resource), 2);
        drop(task);
        assert_eq!(Arc::strong_count(&resource), 1);
    }

    #[test]
    fn rejected_owner_is_woken_by_a_free_slot_before_the_running_job_completes() {
        let execution = WakeEvent::new().unwrap();
        let capacity = WakeEvent::new().unwrap();
        let mut builder = Builder::new(execution.clone()).unwrap();
        let mut sender = builder
            .sender_with_capacity_wake(1, 1, capacity.clone())
            .unwrap();
        let _service = builder.start().unwrap();
        let first_release = Arc::new(AtomicBool::new(false));
        let second_release = Arc::new(AtomicBool::new(false));
        let _first_guard = Release(first_release.clone());
        let _second_guard = Release(second_release.clone());
        let started = WakeEvent::new().unwrap();
        let signal = started.clone();
        sender
            .try_submit(
                Priority::Background,
                Task::new(move || {
                    signal.notify();
                    while !first_release.load(Ordering::Acquire) {
                        thread::yield_now();
                    }
                })
                .without_wake(),
            )
            .unwrap();
        assert!(started.wait(Some(Duration::from_secs(5))).unwrap());
        let second_started = WakeEvent::new().unwrap();
        let signal = second_started.clone();
        let running = sender
            .try_submit(
                Priority::Urgent,
                Task::new(move || {
                    signal.notify();
                    while !second_release.load(Ordering::Acquire) {
                        thread::yield_now();
                    }
                })
                .without_wake(),
            )
            .unwrap();
        let Err(TrySendError::Full(task)) =
            sender.try_submit(Priority::Urgent, Task::new(|| {}).without_wake())
        else {
            panic!("full urgent lane did not reject")
        };
        drop(_first_guard);
        assert!(capacity.wait(Some(Duration::from_secs(5))).unwrap());
        assert!(second_started.wait(Some(Duration::from_secs(5))).unwrap());
        assert_eq!(running.status(), Status::Running);
        assert!(!execution.take_interrupt());
        let retry = sender.try_submit(Priority::Urgent, task).unwrap();
        drop(_second_guard);
        wait(&retry);
        assert_eq!(retry.status(), Status::Completed);
    }

    #[test]
    fn cancelled_work_notifies_its_owner_and_required_work_finishes_on_shutdown() {
        let execution_wake = WakeEvent::new().unwrap();
        let ui_wake = WakeEvent::new().unwrap();
        let mut builder = Builder::new(execution_wake.clone()).unwrap();
        let mut sender = builder.sender(4, 4).unwrap();
        let shared = builder.shared.clone();
        let mut service = builder.start().unwrap();
        let started = WakeEvent::new().unwrap();
        let signal = started.clone();
        sender
            .try_submit(
                Priority::Background,
                Task::new(move || {
                    signal.notify();
                    while shared.gate.load(Ordering::Acquire) & CLOSED == 0 {
                        thread::yield_now();
                    }
                })
                .without_wake(),
            )
            .unwrap();
        assert!(started.wait(Some(Duration::from_secs(5))).unwrap());
        let cancelled = sender
            .try_submit(
                Priority::Background,
                Task::new(|| panic!("cancelled body ran")),
            )
            .unwrap();
        assert!(cancelled.cancel());
        let resource = Arc::new(());
        let lease = resource.clone();
        let returned = Arc::new(AtomicBool::new(false));
        let mark = returned.clone();
        let required = sender
            .try_submit(
                Priority::Urgent,
                Task::new(move || {
                    drop(lease);
                    mark.store(true, Ordering::Release);
                })
                .finish_on_shutdown()
                .with_wake(ui_wake.clone()),
            )
            .unwrap();
        assert!(!required.cancel());
        service.shutdown();
        assert_eq!(required.status(), Status::Completed);
        assert_eq!(cancelled.status(), Status::Cancelled);
        assert!(returned.load(Ordering::Acquire));
        assert_eq!(Arc::strong_count(&resource), 1);
        assert!(ui_wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(execution_wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(matches!(
            sender.try_submit(Priority::Urgent, Task::new(|| {})),
            Err(TrySendError::Disconnected(_))
        ));
    }

    #[test]
    fn task_panic_is_contained_and_ui_completion_does_not_interrupt_execution() {
        let execution = WakeEvent::new().unwrap();
        let ui = WakeEvent::new().unwrap();
        let mut builder = Builder::new(execution.clone()).unwrap();
        let mut sender = builder.sender(2, 2).unwrap();
        let service = builder.start().unwrap();
        let panicked = sender
            .try_submit(
                Priority::Urgent,
                Task::new(|| panic!("contained task panic")).with_wake(ui.clone()),
            )
            .unwrap();
        assert!(ui.wait(Some(Duration::from_secs(5))).unwrap());
        wait(&panicked);
        assert_eq!(panicked.status(), Status::Panicked);
        assert!(!execution.take_interrupt());
        assert!(!execution.wait(Some(Duration::ZERO)).unwrap());
        let completed = sender
            .try_submit(Priority::Urgent, Task::new(|| {}).with_wake(ui.clone()))
            .unwrap();
        wait(&completed);
        assert_eq!(completed.status(), Status::Completed);
        assert_eq!(service.stats(Priority::Urgent).panicked, 1);
        assert_eq!(service.stats(Priority::Urgent).completed, 1);
    }

    #[test]
    fn concurrent_owner_publication_and_shutdown_finish_every_accepted_job() {
        for _ in 0..32 {
            let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
            let mut ui = builder.sender(4, 1).unwrap();
            let mut service = builder.start().unwrap();
            let accepted = Arc::new(AtomicUsize::new(0));
            let announced = accepted.clone();
            let completed = Arc::new(AtomicUsize::new(0));
            let performed = completed.clone();
            let start = Arc::new(Barrier::new(2));
            let ready = start.clone();
            let producing = thread::spawn(move || {
                ready.wait();
                let mut tickets = Vec::new();
                for _ in 0..1024 {
                    let performed = performed.clone();
                    match ui.try_submit(
                        Priority::Urgent,
                        Task::new(move || {
                            performed.fetch_add(1, Ordering::Relaxed);
                        })
                        .finish_on_shutdown(),
                    ) {
                        Ok(ticket) => {
                            tickets.push(ticket);
                            announced.fetch_add(1, Ordering::Release);
                        }
                        Err(TrySendError::Full(_)) => thread::yield_now(),
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                }
                tickets
            });
            start.wait();
            while accepted.load(Ordering::Acquire) == 0 {
                thread::yield_now();
            }
            service.shutdown();
            let tickets = producing.join().unwrap();
            assert_eq!(completed.load(Ordering::Acquire), tickets.len());
            assert!(
                tickets
                    .iter()
                    .all(|ticket| ticket.status() == Status::Completed)
            );
            assert_eq!(service.stats(Priority::Urgent).queued, 0);
        }
    }

    #[test]
    fn shutdown_rechecks_queues_after_the_last_publisher_releases_its_gate() {
        let scanned = WakeEvent::new().unwrap();
        let reserved = WakeEvent::new().unwrap();
        let allow_exit_check = WakeEvent::new().unwrap();
        let allow_publish = WakeEvent::new().unwrap();
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let mut sender = builder.sender(1, 1).unwrap();
        builder.after_empty = Some(Box::new({
            let scanned = scanned.clone();
            let allow_exit_check = allow_exit_check.clone();
            move || {
                scanned.notify();
                allow_exit_check.wait(None).unwrap();
            }
        }));
        sender.before_publish = Some(Box::new({
            let reserved = reserved.clone();
            let allow_publish = allow_publish.clone();
            move || {
                reserved.notify();
                allow_publish.wait(None).unwrap();
            }
        }));
        let mut service = builder.start().unwrap();
        // Release both hooks before joining on any assertion failure.
        struct ReleaseHooks([WakeEvent; 2]);
        impl Drop for ReleaseHooks {
            fn drop(&mut self) {
                for wake in &self.0 {
                    wake.notify();
                }
            }
        }
        let _release = ReleaseHooks([allow_exit_check.clone(), allow_publish.clone()]);
        assert!(scanned.wait(Some(Duration::from_secs(5))).unwrap());
        let completed = Arc::new(AtomicUsize::new(0));
        let performed = completed.clone();
        let producing = thread::spawn(move || {
            sender.try_submit(
                Priority::Urgent,
                Task::new(move || {
                    performed.fetch_add(1, Ordering::Relaxed);
                })
                .finish_on_shutdown(),
            )
        });
        assert!(reserved.wait(Some(Duration::from_secs(5))).unwrap());
        assert_eq!(service.shared.gate.load(Ordering::Acquire), 1);
        // Reproduce shutdown's close operation while publication is reserved,
        // and the worker has already scanned every queue as empty.
        service.shared.gate.fetch_or(CLOSED, Ordering::AcqRel);
        service.shared.request.notify();
        allow_publish.notify();
        let accepted = producing.join().unwrap().unwrap();
        assert_eq!(service.shared.gate.load(Ordering::Acquire), CLOSED);
        assert_eq!(accepted.status(), Status::Queued);
        allow_exit_check.notify();
        service.shutdown();
        assert_eq!(completed.load(Ordering::Acquire), 1);
        assert_eq!(accepted.status(), Status::Completed);
        assert_eq!(service.stats(Priority::Urgent).queued, 0);
    }
}
