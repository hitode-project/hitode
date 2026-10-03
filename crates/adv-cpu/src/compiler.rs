//! One bounded compile worker. Requests own small immutable code snapshots;
//! neither this module nor its thread receives or borrows guest memory.
use adv_sync::{
    WakeEvent,
    spsc::{self, Consumer, Producer},
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::Region;
use crate::compile::{self, Compiled, Snapshot};

pub(crate) const CAPACITY: usize = 8;
pub(crate) type CompletionWake = Arc<dyn Fn() + Send + Sync>;

pub(crate) struct Completion {
    pub ticket: u64,
    pub region: Region,
    pub input: Snapshot,
    pub result: Result<Compiled, String>,
    pub duration: Duration,
}

struct Job {
    ticket: u64,
    input: Snapshot,
    wake: Option<CompletionWake>,
}

pub(crate) struct Worker {
    requests: Option<Producer<Job>>,
    results: Option<Consumer<Completion>>,
    // Only the execution owner submits and drains work, so this permit count
    // needs neither atomic operations nor another shared mutable object.
    outstanding: usize,
    closed: Arc<AtomicBool>,
    request_wake: WakeEvent,
    next_ticket: u64,
    thread: Option<JoinHandle<()>>,
    wake: Option<CompletionWake>,
}

struct WorkerExit(Arc<AtomicBool>);
impl Drop for WorkerExit {
    fn drop(&mut self) {
        // Also publish disconnection if a notifier unexpectedly panics.
        self.0.store(true, Ordering::Release);
    }
}

impl Worker {
    pub fn new() -> std::io::Result<Self> {
        Self::with_compiler(compile::compile_snapshot)
    }

    fn with_compiler<F>(compiler: F) -> std::io::Result<Self>
    where
        F: Fn(&Snapshot) -> Result<Compiled, String> + Send + 'static,
    {
        fn send<T: Send>() {}
        send::<Snapshot>();
        send::<Compiled>();
        let (requests, mut jobs) = spsc::channel::<Job>(CAPACITY);
        let (mut completed, results) = spsc::channel::<Completion>(CAPACITY);
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = Arc::clone(&closed);
        let request_wake = WakeEvent::new()?;
        let worker_wake = request_wake.clone();
        let thread = thread::Builder::new()
            .name("adv-compiler".into())
            .spawn(move || {
                let _exit = WorkerExit(worker_closed.clone());
                loop {
                    if worker_closed.load(Ordering::Acquire) {
                        break;
                    }
                    let Some(job) = jobs.pop() else {
                        // The producer publishes before notifying this retained
                        // event. A notification between the empty check and wait
                        // stays pending, so no polling timeout is necessary.
                        if worker_closed.load(Ordering::Acquire) || worker_wake.wait(None).is_err()
                        {
                            break;
                        }
                        continue;
                    };
                    let started = Instant::now();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        compiler(&job.input)
                    }))
                    .unwrap_or_else(|_| Err("background compiler panicked".into()));
                    let completion = Completion {
                        ticket: job.ticket,
                        region: job.input.region.clone(),
                        input: job.input,
                        result,
                        duration: started.elapsed(),
                    };
                    if worker_closed.load(Ordering::Acquire) {
                        break;
                    }
                    // Queued + active + completed is capped at CAPACITY. With
                    // this active job there are at most CAPACITY-1 completions;
                    // publishing its result cannot block on a consumer.
                    if completed.push(completion).is_err() {
                        debug_assert!(false, "compile completion permit invariant");
                        break;
                    }
                    // The execution owner retains its existing unique wake.
                    // The compiler's request event is never an execution wait.
                    if let Some(wake) = job.wake {
                        wake();
                    }
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            results: Some(results),
            outstanding: 0,
            closed,
            request_wake,
            next_ticket: 1,
            thread: Some(thread),
            wake: None,
        })
    }

    pub fn set_waker(&mut self, wake: CompletionWake) {
        debug_assert_eq!(self.outstanding, 0);
        self.wake = Some(wake);
    }

    pub fn try_submit(&mut self, input: Snapshot) -> Result<u64, TrySendError<Snapshot>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TrySendError::Disconnected(input));
        }
        if self.outstanding >= CAPACITY {
            return Err(TrySendError::Full(input));
        }
        let Some(requests) = self.requests.as_mut() else {
            return Err(TrySendError::Disconnected(input));
        };
        let ticket = self.next_ticket;
        self.next_ticket = self.next_ticket.wrapping_add(1);
        let job = Job {
            ticket,
            input,
            wake: self.wake.clone(),
        };
        if let Err(job) = requests.push(job) {
            return Err(TrySendError::Full(job.input));
        }
        self.outstanding += 1;
        self.request_wake.notify();
        Ok(ticket)
    }

    pub fn try_recv(&mut self) -> Result<Completion, TryRecvError> {
        // Warm dispatch pays no queue poll when no queued, active or completed
        // permit remains. The single owner decrements only after taking a value.
        if self.outstanding == 0 {
            return Err(TryRecvError::Empty);
        }
        let results = self.results.as_mut().ok_or(TryRecvError::Disconnected)?;
        if let Some(completion) = results.pop() {
            self.outstanding -= 1;
            return Ok(completion);
        }
        Err(if self.closed.load(Ordering::Acquire) {
            TryRecvError::Disconnected
        } else {
            TryRecvError::Empty
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        // An idle worker must leave its retained wait even with no queued job.
        self.request_wake.notify();
        self.results.take();
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EXEC, Memory, READ, WRITE};
    use std::sync::Barrier;
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
            Err(TrySendError::Full(_))
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
        let finished = Arc::new(AtomicBool::new(false));
        let mark_finished = Arc::clone(&finished);
        let dropping = thread::spawn(move || {
            drop(worker);
            mark_finished.store(true, Ordering::Release);
        });
        while !closed.load(Ordering::Acquire) {
            thread::yield_now();
        }
        released.wait();
        dropping.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
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
                Err(TrySendError::Full(_))
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
            Err(TrySendError::Disconnected(_))
        ));
    }
}
