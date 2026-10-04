//! CPU compilation client of the one neutral background task service.
//! Snapshots and typed completions retain their bounded SPSC ownership contract.
#[cfg(test)]
use adv_sync::{
    WakeEvent,
    background::{Builder, WorkPolicy},
};
use adv_sync::{
    background::{Affinity, LocalSender, Priority, Service, Task, Ticket},
    spsc::{self, Consumer, Producer},
};
use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{TryRecvError, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use crate::Region;
use crate::compile::{self, Compiled, Snapshot};

pub(crate) const CAPACITY: usize = 8;
pub(crate) type CompletionWake = Arc<dyn Fn() + Send + Sync>;
#[derive(Debug)]
pub(crate) enum SubmitError {
    Full,
    Closed,
    WorkLimit { work: usize, limit: usize },
}
type Compiler = dyn Fn(&Snapshot) -> Result<Compiled, String> + Send + Sync;

pub(crate) struct Completion {
    pub ticket: u64,
    pub region: Region,
    pub input: Snapshot,
    pub result: Result<Compiled, String>,
    pub duration: Duration,
}

// The only writers are sealed compilation tasks bound to one immutable service
// Affinity. Task execution is private to that service's sole worker. No producer
// reference escapes this module or reaches the execution owner. An injected
// LocalSender can be replaced by safe caller code, but affinity checks then
// reject those jobs rather than access this producer from a second worker.
struct Delivery(UnsafeCell<Producer<Completion>>);
unsafe impl Sync for Delivery {}
impl Delivery {
    fn publish(&self, completion: Completion) -> bool {
        // SAFETY: the private caller is a service-affine task; no other thread
        // can call it or execute a second producer operation concurrently.
        unsafe { (&mut *self.0.get()).push(completion).is_ok() }
    }
}

pub(crate) struct Worker {
    sender: LocalSender,
    affinity: Affinity,
    delivery: Arc<Delivery>,
    compiler: Arc<Compiler>,
    results: Option<Consumer<Completion>>,
    outstanding: usize,
    closed: Arc<AtomicBool>,
    next_ticket: u64,
    last_work: usize,
    jobs: Vec<Ticket>,
    // Only standalone CPU tests own a service here. Executor keeps its lazy
    // service alive across disabling JIT, so canceling a client never joins a
    // running compile on the guest execution path.
    service: Option<Service>,
    wake: Option<CompletionWake>,
    pool: Option<Pool>,
}

type Job = Box<dyn FnOnce() + Send>;
/// Dedicated compile threads, so compilation neither waits behind nor delays
/// audio and presentation jobs on the shared service. Dropping the pool lets
/// idle threads exit; a running compile finishes detached and is discarded.
struct Pool {
    jobs: std::sync::mpsc::Sender<Job>,
    results: std::sync::mpsc::Receiver<Completion>,
    deliver: std::sync::mpsc::Sender<Completion>,
}
impl Pool {
    fn new(threads: usize) -> std::io::Result<Self> {
        let (jobs, queue) = std::sync::mpsc::channel::<Job>();
        let queue = Arc::new(std::sync::Mutex::new(queue));
        for index in 0..threads {
            let queue = queue.clone();
            thread::Builder::new()
                .name(format!("adv-jit-{index}"))
                .spawn(move || {
                    loop {
                        // Hold the lock only to take one job.
                        let job = queue
                            .lock()
                            .map_err(|_| ())
                            .and_then(|q| q.recv().map_err(|_| ()));
                        let Ok(job) = job else { return };
                        job();
                    }
                })?;
        }
        let (deliver, results) = std::sync::mpsc::channel();
        Ok(Self {
            jobs,
            results,
            deliver,
        })
    }
}

struct ClientPanic(Arc<AtomicBool>);
impl Drop for ClientPanic {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.store(true, Ordering::Release);
        }
    }
}

impl Worker {
    #[cfg(test)]
    pub fn new() -> std::io::Result<Self> {
        Self::with_compiler(compile::compile_snapshot)
    }

    pub fn with_sender(sender: LocalSender) -> Self {
        Self::with_sender_compiler(sender, compile::compile_snapshot)
    }

    #[cfg(test)]
    fn with_compiler<F>(compiler: F) -> std::io::Result<Self>
    where
        F: Fn(&Snapshot) -> Result<Compiled, String> + Send + Sync + 'static,
    {
        let mut builder = Builder::new(WakeEvent::new()?)?;
        let sender = builder.sender(CAPACITY, CAPACITY)?.into_local();
        let service = builder.start()?;
        let mut worker = Self::with_sender_compiler(sender, compiler);
        worker.service = Some(service);
        Ok(worker)
    }

    fn with_sender_compiler<F>(sender: LocalSender, compiler: F) -> Self
    where
        F: Fn(&Snapshot) -> Result<Compiled, String> + Send + Sync + 'static,
    {
        let affinity = sender.borrow().affinity();
        let (completed, results) = spsc::channel(CAPACITY);
        Self {
            sender,
            affinity,
            delivery: Arc::new(Delivery(UnsafeCell::new(completed))),
            compiler: Arc::new(compiler),
            results: Some(results),
            outstanding: 0,
            closed: Arc::new(AtomicBool::new(false)),
            next_ticket: 1,
            last_work: 0,
            jobs: Vec::with_capacity(CAPACITY),
            service: None,
            wake: None,
            pool: None,
        }
    }

    /// Compile on `threads` dedicated threads instead of the shared service.
    pub fn with_threads(sender: LocalSender, threads: usize) -> std::io::Result<Self> {
        let mut worker = Self::with_sender(sender);
        worker.pool = Some(Pool::new(threads.max(1))?);
        Ok(worker)
    }

    pub fn set_waker(&mut self, wake: CompletionWake) {
        debug_assert_eq!(self.outstanding, 0);
        self.wake = Some(wake);
    }
    pub fn has_outstanding(&self) -> bool {
        self.outstanding != 0
    }
    pub fn last_work(&self) -> usize {
        self.last_work
    }

    pub fn try_submit(&mut self, input: Snapshot) -> Result<u64, SubmitError> {
        // Acquiring the started flag before reading policy also covers a client
        // created before Builder finishes configuring its registered endpoints.
        if self.sender.borrow().is_closed() {
            return Err(SubmitError::Closed);
        }
        let work = if let Some(limit) = self.sender.borrow().work_policy().background_limit() {
            let work = crate::compile_budget::work(&input);
            if work > limit {
                return Err(SubmitError::WorkLimit { work, limit });
            }
            work
        } else {
            0
        }; // Unrestricted clients do not decode solely for statistics.
        if self.pool.is_some() {
            return self.submit_pooled(input, work);
        }
        let ticket = self.next_ticket;
        let compiler = self.compiler.clone();
        let delivery = self.delivery.clone();
        let closed = self.closed.clone();
        let wake = self.wake.clone();
        let notify_here = wake.is_some();
        let mut task = Task::new(move || {
            if closed.load(Ordering::Acquire) {
                return;
            }
            let _panic = ClientPanic(closed.clone());
            let started = Instant::now();
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compiler(&input)))
                    .unwrap_or_else(|_| Err("background compiler panicked".into()));
            let completion = Completion {
                ticket,
                region: (*input.region).clone(),
                input,
                result,
                duration: started.elapsed(),
            };
            if closed.load(Ordering::Acquire) {
                return;
            }
            // Owner-only permits include queued, active AND unconsumed results.
            // This active job leaves at most CAPACITY-1 completed results.
            if !delivery.publish(completion) {
                closed.store(true, Ordering::Release);
                debug_assert!(false, "compile completion permit invariant");
                return;
            }
            if let Some(wake) = wake {
                wake();
            }
        })
        .affine_to(self.affinity);
        if notify_here {
            task = task.without_wake();
        }
        if self.closed.load(Ordering::Acquire) {
            return Err(SubmitError::Closed);
        }
        if self.outstanding >= CAPACITY {
            return Err(SubmitError::Full);
        }
        let job = self
            .sender
            .borrow_mut()
            .try_submit(Priority::Background, task)
            .map_err(|error| match error {
                TrySendError::Full(_) => SubmitError::Full,
                TrySendError::Disconnected(_) => SubmitError::Closed,
            })?;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.last_work = work;
        self.outstanding += 1;
        self.jobs.push(job);
        Ok(ticket)
    }

    fn submit_pooled(&mut self, input: Snapshot, work: usize) -> Result<u64, SubmitError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(SubmitError::Closed);
        }
        if self.outstanding >= CAPACITY {
            return Err(SubmitError::Full);
        }
        let pool = self.pool.as_ref().unwrap();
        let ticket = self.next_ticket;
        let compiler = self.compiler.clone();
        let deliver = pool.deliver.clone();
        let closed = self.closed.clone();
        let wake = self.wake.clone();
        let job: Job = Box::new(move || {
            if closed.load(Ordering::Acquire) {
                return;
            }
            let started = Instant::now();
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compiler(&input)))
                    .unwrap_or_else(|_| Err("background compiler panicked".into()));
            let completion = Completion {
                ticket,
                region: (*input.region).clone(),
                input,
                result,
                duration: started.elapsed(),
            };
            if closed.load(Ordering::Acquire) || deliver.send(completion).is_err() {
                return;
            }
            if let Some(wake) = wake {
                wake();
            }
        });
        pool.jobs.send(job).map_err(|_| SubmitError::Closed)?;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        self.last_work = work;
        self.outstanding += 1;
        Ok(ticket)
    }

    pub fn try_recv(&mut self) -> Result<Completion, TryRecvError> {
        if self.outstanding == 0 {
            return Err(TryRecvError::Empty);
        }
        if let Some(pool) = &self.pool {
            let completion = pool.results.try_recv()?;
            self.outstanding -= 1;
            return Ok(completion);
        }
        let results = self.results.as_mut().ok_or(TryRecvError::Disconnected)?;
        if let Some(completion) = results.pop() {
            self.outstanding -= 1;
            self.jobs.retain(|job| {
                matches!(
                    job.status(),
                    adv_sync::background::Status::Queued | adv_sync::background::Status::Running
                )
            });
            return Ok(completion);
        }
        Err(
            if self.closed.load(Ordering::Acquire) || self.sender.borrow().is_stopped() {
                TryRecvError::Disconnected
            } else {
                TryRecvError::Empty
            },
        )
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        for job in &self.jobs {
            job.cancel();
        }
        self.results.take();
        if let Some(mut service) = self.service.take() {
            service.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EXEC, Memory, READ, WRITE};
    use std::sync::Barrier;
    use std::sync::OnceLock;
    use std::sync::atomic::AtomicUsize;

    fn snapshot() -> Snapshot {
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &[0x90, 0xc3]).unwrap();
        Snapshot::capture(&Region::new(0x10000, 2), &memory).unwrap()
    }

    fn receive(worker: &mut Worker) -> Completion {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            match worker.try_recv() {
                Ok(completion) => return completion,
                Err(TryRecvError::Empty) if Instant::now() < until => thread::yield_now(),
                Err(error) => panic!("compiler did not complete: {error:?}"),
            }
        }
    }

    #[test]
    fn outstanding_limit_includes_completed_jobs_and_tickets_are_preserved() {
        let mut worker = Worker::with_compiler(|_| Err("deterministic completion".into())).unwrap();
        let input = snapshot();
        let tickets: Vec<_> = (0..CAPACITY)
            .map(|_| worker.try_submit(input.clone()).unwrap())
            .collect();
        assert!(matches!(
            worker.try_submit(input.clone()),
            Err(SubmitError::Full)
        ));
        for expected in tickets {
            let completion = receive(&mut worker);
            assert_eq!(completion.ticket, expected);
            assert!(completion.result.is_err());
        }
        assert!(worker.try_submit(input).is_ok());
        receive(&mut worker);
    }

    #[test]
    fn drop_cancels_queued_requests_after_the_current_job_without_blocking_on_results() {
        let started = Arc::new(Barrier::new(2));
        let released = Arc::new(Barrier::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_started = Arc::clone(&started);
        let worker_released = Arc::clone(&released);
        let worker_calls = Arc::clone(&calls);
        let mut worker = Worker::with_compiler(move |_| {
            worker_calls.fetch_add(1, Ordering::Relaxed);
            worker_started.wait();
            worker_released.wait();
            Err("completed during shutdown".into())
        })
        .unwrap();
        let input = snapshot();
        worker.try_submit(input.clone()).unwrap();
        started.wait();
        for _ in 1..CAPACITY {
            worker.try_submit(input.clone()).unwrap();
        }
        let closed = Arc::clone(&worker.closed);
        // The producer endpoint stays on its owner. This helper releases the
        // active job once teardown marks the client closed; it never owns Worker.
        let releasing = thread::spawn(move || {
            while !closed.load(Ordering::Acquire) {
                thread::yield_now();
            }
            released.wait();
        });
        drop(worker);
        releasing.join().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn full_completed_queue_does_not_deadlock_drop() {
        let done = Arc::new(AtomicUsize::new(0));
        let worker_done = Arc::clone(&done);
        let mut worker = Worker::with_compiler(move |_| {
            worker_done.fetch_add(1, Ordering::Release);
            Err("done".into())
        })
        .unwrap();
        for _ in 0..CAPACITY {
            worker.try_submit(snapshot()).unwrap();
        }
        while done.load(Ordering::Acquire) != CAPACITY {
            thread::yield_now();
        }
        drop(worker);
    }

    #[test]
    fn real_worker_compiles_a_detached_snapshot_and_returns_the_matching_ticket() {
        let mut worker = Worker::new().unwrap();
        let input = snapshot(); // Its Memory has already been dropped.
        let ticket = worker.try_submit(input).unwrap();
        let completion = receive(&mut worker);
        assert_eq!(completion.ticket, ticket);
        assert_eq!(completion.region.start, 0x10000);
        assert!(completion.result.unwrap().entries.contains(&0x10000));
    }

    #[test]
    fn completion_is_receivable_when_the_retained_os_wake_arrives() {
        for panic in [false, true] {
            let wake = adv_sync::WakeEvent::new().unwrap();
            let signal = wake.clone();
            let mut worker = Worker::with_compiler(move |_| {
                assert!(!panic, "contained compiler panic");
                Err("contained compile failure".into())
            })
            .unwrap();
            worker.set_waker(Arc::new(move || signal.signal()));
            let ticket = worker.try_submit(snapshot()).unwrap();
            assert!(wake.wait(Some(Duration::from_secs(5))).unwrap());
            assert!(wake.take_interrupt());
            let completion = worker.try_recv().expect("publication precedes wake");
            assert_eq!(completion.ticket, ticket);
            assert!(completion.result.is_err());
        }
    }

    #[test]
    fn request_and_completion_queues_wrap_with_ordered_tickets_and_retained_wakes() {
        let wake = WakeEvent::new().unwrap();
        let signal = wake.clone();
        let mut worker = Worker::with_compiler(|_| Err("completed".into())).unwrap();
        worker.set_waker(Arc::new(move || signal.notify()));
        let input = snapshot();
        for _ in 0..32 {
            let tickets: Vec<_> = (0..CAPACITY)
                .map(|_| worker.try_submit(input.clone()).unwrap())
                .collect();
            assert!(matches!(
                worker.try_submit(input.clone()),
                Err(SubmitError::Full)
            ));
            for ticket in tickets {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    match worker.try_recv() {
                        Ok(completion) => {
                            assert_eq!(completion.ticket, ticket);
                            assert!(completion.result.is_err());
                            break;
                        }
                        Err(TryRecvError::Empty) => {
                            assert!(Instant::now() < deadline);
                            assert!(
                                wake.wait(Some(deadline.saturating_duration_since(Instant::now())))
                                    .unwrap()
                            );
                        }
                        Err(error) => panic!("lost compiler worker: {error:?}"),
                    }
                }
            }
            assert_eq!(worker.outstanding, 0);
        }
    }

    #[test]
    fn idle_worker_shutdown_wakes_its_request_wait_without_a_pending_job() {
        for _ in 0..8 {
            let worker = Worker::with_compiler(|_| panic!("no job submitted")).unwrap();
            drop(worker);
        }
    }

    #[test]
    fn notifier_panic_preserves_published_completion_then_disconnects_submission() {
        let mut worker =
            Worker::with_compiler(|_| Err("published before notifier".into())).unwrap();
        worker.set_waker(Arc::new(|| panic!("contained by worker lifetime")));
        let ticket = worker.try_submit(snapshot()).unwrap();
        assert_eq!(receive(&mut worker).ticket, ticket);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker.closed.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
        assert!(matches!(
            worker.try_submit(snapshot()),
            Err(SubmitError::Closed)
        ));
    }

    #[test]
    fn injected_cpu_and_device_jobs_run_on_the_same_background_thread() {
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let sender = builder.sender(2, CAPACITY).unwrap().into_local();
        let _service = builder.start().unwrap();
        let cpu = Arc::new(OnceLock::new());
        let seen = cpu.clone();
        let mut worker = Worker::with_sender_compiler(sender.clone(), move |_| {
            seen.set(thread::current().id()).unwrap();
            Err("same worker".into())
        });
        let device = Arc::new(OnceLock::new());
        let seen = device.clone();
        let job = sender
            .borrow_mut()
            .try_submit(
                Priority::Urgent,
                Task::new(move || {
                    seen.set(thread::current().id()).unwrap();
                }),
            )
            .unwrap();
        worker.try_submit(snapshot()).unwrap();
        receive(&mut worker);
        assert_eq!(job.status(), adv_sync::background::Status::Completed);
        assert_eq!(cpu.get(), device.get());
        assert_ne!(cpu.get(), Some(&thread::current().id()));
    }

    #[test]
    fn dropping_injected_client_does_not_join_the_active_shared_worker() {
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let sender = builder.sender(2, CAPACITY).unwrap().into_local();
        let _service = builder.start().unwrap();
        let started = WakeEvent::new().unwrap();
        let signal = started.clone();
        let released = Arc::new(AtomicBool::new(false));
        let release = released.clone();
        let blocked = Arc::new(AtomicBool::new(false));
        let failed = blocked.clone();
        let mut worker = Worker::with_sender_compiler(sender.clone(), move |_| {
            signal.notify();
            let until = Instant::now() + Duration::from_secs(2);
            while !release.load(Ordering::Acquire) {
                if Instant::now() > until {
                    failed.store(true, Ordering::Release);
                    break;
                }
                thread::yield_now();
            }
            Err("client cancelled".into())
        });
        worker.try_submit(snapshot()).unwrap();
        assert!(started.wait(Some(Duration::from_secs(5))).unwrap());
        drop(worker);
        assert!(
            !blocked.load(Ordering::Acquire),
            "client teardown joined an active shared job"
        );
        let device = sender
            .borrow_mut()
            .try_submit(Priority::Urgent, Task::new(|| {}))
            .unwrap();
        released.store(true, Ordering::Release);
        let until = Instant::now() + Duration::from_secs(5);
        while device.status() != adv_sync::background::Status::Completed {
            assert!(Instant::now() < until);
            thread::yield_now();
        }
    }

    #[test]
    fn replacing_a_local_endpoint_cannot_move_its_completion_writer_to_another_service() {
        let mut first_builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let sender = first_builder.sender(2, CAPACITY).unwrap().into_local();
        let _first_service = first_builder.start().unwrap();
        let mut worker =
            Worker::with_sender_compiler(sender.clone(), |_| Err("still affine".into()));
        let mut second_builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let second = second_builder.sender(2, CAPACITY).unwrap();
        let _second_service = second_builder.start().unwrap();
        let original = std::mem::replace(&mut *sender.borrow_mut(), second);
        assert!(matches!(
            worker.try_submit(snapshot()),
            Err(SubmitError::Closed)
        ));
        *sender.borrow_mut() = original;
        worker.try_submit(snapshot()).unwrap();
        assert!(receive(&mut worker).result.is_err());
    }

    #[test]
    fn every_submission_rechecks_the_service_work_limit_before_enqueue() {
        let mut builder = Builder::new(WakeEvent::new().unwrap())
            .unwrap()
            .with_work_policy(WorkPolicy::responsive(8));
        let sender = builder.sender(2, CAPACITY).unwrap().into_local();
        let service = builder.start().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let called = calls.clone();
        let mut worker = Worker::with_sender_compiler(sender, move |_| {
            called.fetch_add(1, Ordering::Relaxed);
            Err("small admitted input".into())
        });
        assert!(matches!(
            worker.try_submit(snapshot()),
            Err(SubmitError::WorkLimit { work: 36, limit: 8 })
        ));
        assert_eq!(service.stats(Priority::Background).submitted, 0);
        assert_eq!(calls.load(Ordering::Acquire), 0);
        let mut memory = Memory::new(0x10000, 16384).unwrap();
        memory.protect(0x10000, 16384, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &[0x90; 16384]).unwrap();
        let mut authored = Region::new(0x10000, 16384);
        authored.compile_strategy = crate::CompileStrategy::Fast;
        assert!(matches!(
            worker.try_submit(Snapshot::capture(&authored, &memory).unwrap()),
            Err(SubmitError::WorkLimit {
                work: 16384,
                limit: 8
            })
        ));
        assert_eq!(service.stats(Priority::Background).submitted, 0);
        let mut admitted = snapshot();
        admitted.region.compile_strategy = crate::CompileStrategy::Fast;
        admitted.region.len = 1;
        worker.try_submit(admitted).unwrap();
        receive(&mut worker);
        assert_eq!(calls.load(Ordering::Acquire), 1);
    }

    #[test]
    #[ignore = "diagnostic nonpreemptible compilation latency, not a shared-runner gate"]
    fn report_nonpreemptible_compile_latency() {
        let execution = WakeEvent::new().unwrap();
        let ui_wake = WakeEvent::new().unwrap();
        let mut builder = Builder::new(execution).unwrap();
        let sender = builder.sender(2, CAPACITY).unwrap().into_local();
        let mut ui = builder.sender(2, 1).unwrap();
        let service = builder.start().unwrap();
        for strategy in [crate::CompileStrategy::Fast, crate::CompileStrategy::Speed] {
            let mut bytes = Vec::new();
            for _ in 0..63 {
                bytes.extend_from_slice(&[0x8b, 0x05, 0, 0, 2, 0]); // mov eax,[data]
                bytes.extend_from_slice(&[0x03, 0x1d, 4, 0, 2, 0]); // add ebx,[data+4]
                bytes.extend_from_slice(&[0x89, 0x0d, 8, 0, 2, 0]); // mov [data+8],ecx
                bytes.extend_from_slice(&[0x33, 0xd0]); // xor edx,eax
            }
            bytes.push(0xc3);
            let mut memory = Memory::new(0x10000, 0x20000).unwrap();
            memory
                .protect(0x10000, 0x20000, READ | WRITE | EXEC)
                .unwrap();
            memory.write(0x10000, &bytes).unwrap();
            let mut region = Region::new(0x10000, bytes.len());
            region.compile_strategy = strategy;
            let input = Snapshot::capture(&region, &memory).unwrap();
            let begun = WakeEvent::new().unwrap();
            let signal = begun.clone();
            let released = WakeEvent::new().unwrap();
            let go = released.clone();
            let mut worker = Worker::with_sender_compiler(sender.clone(), move |input| {
                signal.notify();
                go.wait(None).unwrap();
                compile::compile_snapshot(input)
            });
            let mut compile_ms = Vec::new();
            let mut urgent_ms = Vec::new();
            for _ in 0..7 {
                worker.try_submit(input.clone()).unwrap();
                assert!(begun.wait(Some(Duration::from_secs(5))).unwrap());
                let queued = Instant::now();
                let ticket = ui
                    .try_submit(
                        Priority::Urgent,
                        Task::new(|| {}).with_wake(ui_wake.clone()),
                    )
                    .unwrap();
                released.notify();
                assert!(ui_wake.wait(Some(Duration::from_secs(30))).unwrap());
                urgent_ms.push(queued.elapsed().as_secs_f64() * 1000.0);
                let completion = receive(&mut worker);
                compile_ms.push(completion.duration.as_secs_f64() * 1000.0);
                assert!(completion.result.is_ok());
                assert_eq!(ticket.status(), adv_sync::background::Status::Completed);
            }
            compile_ms.sort_by(f64::total_cmp);
            urgent_ms.sort_by(f64::total_cmp);
            println!(
                "strategy={strategy:?} instructions=253 samples=7 compile_p50_ms={:.3} compile_max_ms={:.3} urgent_p50_ms={:.3} urgent_max_ms={:.3}",
                compile_ms[3], compile_ms[6], urgent_ms[3], urgent_ms[6]
            );
        }
        println!("service={:?}", service.stats(Priority::Urgent));
    }

    #[test]
    #[ignore = "diagnostic weighted compilation latency, not a shared-runner timing gate"]
    fn report_responsive_compile_latency() {
        #[cfg(windows)]
        unsafe {
            unsafe extern "system" {
                fn GetCurrentProcess() -> *mut std::ffi::c_void;
                fn SetProcessAffinityMask(process: *mut std::ffi::c_void, mask: usize) -> i32;
            }
            assert_ne!(SetProcessAffinityMask(GetCurrentProcess(), 1), 0);
        }
        let ui_wake = WakeEvent::new().unwrap();
        let mut builder = Builder::new(WakeEvent::new().unwrap()).unwrap();
        let sender = builder.sender(2, CAPACITY).unwrap().into_local();
        let mut ui = builder.sender(2, 1).unwrap();
        let _service = builder.start().unwrap();
        println!(
            "case,strategy,limit,work,admitted,compile_p50_ms,compile_p95_ms,compile_max_ms,urgent_p95_ms,code_bytes"
        );
        for (name, op, repeats, limit) in [
            ("scalar16", &[0x8b, 0x05, 0, 0, 2, 0][..], 16, 128),
            ("rmw8", &[0x83, 0x05, 0, 0, 2, 0, 1][..], 8, 128),
            ("simd4", &[0xf3, 0x0f, 0x6f, 0x05, 0, 0, 2, 0][..], 4, 128),
            ("scalar3", &[0x8b, 0x05, 0, 0, 2, 0][..], 3, 128),
            ("scalar32", &[0x8b, 0x05, 0, 0, 2, 0][..], 32, 256),
            ("rmw16", &[0x83, 0x05, 0, 0, 2, 0, 1][..], 16, 256),
            ("simd8", &[0xf3, 0x0f, 0x6f, 0x05, 0, 0, 2, 0][..], 8, 256),
            ("scalar7", &[0x8b, 0x05, 0, 0, 2, 0][..], 7, 256),
            ("scalar64", &[0x8b, 0x05, 0, 0, 2, 0][..], 64, 512),
            ("rmw32", &[0x83, 0x05, 0, 0, 2, 0, 1][..], 32, 512),
            ("simd16", &[0xf3, 0x0f, 0x6f, 0x05, 0, 0, 2, 0][..], 16, 512),
            ("scalar15", &[0x8b, 0x05, 0, 0, 2, 0][..], 15, 512),
        ] {
            for strategy in [crate::CompileStrategy::Fast, crate::CompileStrategy::Speed] {
                let mut bytes = op.repeat(repeats);
                bytes.push(0xc3);
                let mut memory = Memory::new(0x10000, 0x20000).unwrap();
                memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
                memory.write(0x10000, &bytes).unwrap();
                let mut region = Region::new(0x10000, bytes.len());
                region.compile_strategy = strategy;
                let mut input = Snapshot::capture(&region, &memory).unwrap();
                if strategy == crate::CompileStrategy::Fast {
                    input = crate::compile_budget::plan(input, limit).unwrap().remove(0);
                }
                let begun = WakeEvent::new().unwrap();
                let signal = begun.clone();
                let released = WakeEvent::new().unwrap();
                let go = released.clone();
                let mut worker = Worker::with_sender_compiler(sender.clone(), move |input| {
                    signal.notify();
                    go.wait(None).unwrap();
                    compile::compile_snapshot(input)
                });
                let mut compilation = Vec::new();
                let mut urgent = Vec::new();
                let mut size = 0;
                for _ in 0..21 {
                    worker.try_submit(input.clone()).unwrap();
                    assert!(begun.wait(Some(Duration::from_secs(5))).unwrap());
                    let queued = Instant::now();
                    let _ticket = ui
                        .try_submit(
                            Priority::Urgent,
                            Task::new(|| {}).with_wake(ui_wake.clone()),
                        )
                        .unwrap();
                    released.notify();
                    assert!(ui_wake.wait(Some(Duration::from_secs(30))).unwrap());
                    urgent.push(queued.elapsed().as_secs_f64() * 1000.0);
                    let completion = receive(&mut worker);
                    compilation.push(completion.duration.as_secs_f64() * 1000.0);
                    size = completion.result.unwrap().code_size;
                }
                compilation.sort_by(f64::total_cmp);
                urgent.sort_by(f64::total_cmp);
                println!(
                    "{name},{strategy:?},{limit},{},{},{:.3},{:.3},{:.3},{:.3},{size}",
                    crate::compile_budget::work(&input),
                    crate::compile_budget::work(&input) <= limit,
                    compilation[10],
                    compilation[19],
                    compilation[20],
                    urgent[19]
                );
            }
        }
        let mut memory = Memory::new(0x10000, 16384).unwrap();
        memory.protect(0x10000, 16384, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &[0x90; 16384]).unwrap();
        let mut authored = Region::new(0x10000, 16384);
        authored.compile_strategy = crate::CompileStrategy::Fast;
        let input = Snapshot::capture(&authored, &memory).unwrap();
        let mut planning = Vec::new();
        for _ in 0..21 {
            let began = Instant::now();
            let fragments = crate::compile_budget::plan(input.clone(), 128).unwrap();
            planning.push(began.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(fragments.len(), 128);
        }
        planning.sort_by(f64::total_cmp);
        println!(
            "owner_plan_16384_nops_p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
            planning[10], planning[19], planning[20]
        );
    }
}
