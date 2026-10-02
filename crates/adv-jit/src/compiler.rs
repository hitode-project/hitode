//! One bounded compile worker. Requests own small immutable code snapshots;
//! neither this module nor its thread receives or borrows guest memory.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::Region;
use crate::compile::{self, Compiled, Snapshot};

pub(crate) const CAPACITY: usize = 8;

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
}

pub(crate) struct Worker {
    requests: Option<SyncSender<Job>>,
    results: Option<Receiver<Completion>>,
    outstanding: Arc<AtomicUsize>,
    closed: Arc<AtomicBool>,
    next_ticket: AtomicU64,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn new() -> std::io::Result<Self> {
        Self::with_compiler(compile::compile_snapshot)
    }

    fn with_compiler<F>(compiler: F) -> std::io::Result<Self>
    where
        F: Fn(&Snapshot) -> Result<Compiled, String> + Send + 'static,
    {
        // Compile-time proof: Cranelift's owned module and the function pointer
        // are Send already. No unsafe Send wrapper or guest pointer is needed.
        fn send<T: Send>() {}
        send::<Snapshot>();
        send::<Compiled>();
        let (requests, jobs) = mpsc::sync_channel::<Job>(CAPACITY);
        let (completed, results) = mpsc::sync_channel::<Completion>(CAPACITY);
        let outstanding = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = Arc::clone(&closed);
        let thread = thread::Builder::new()
            .name("adv-compiler".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    if worker_closed.load(Ordering::Acquire) {
                        break;
                    }
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
                    // Queued + active + completed is capped at CAPACITY. Therefore
                    // this send cannot encounter a full completion queue while a
                    // separate request is active. Shutdown drops its receiver first.
                    if completed.send(completion).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            results: Some(results),
            outstanding,
            closed,
            next_ticket: AtomicU64::new(1),
            thread: Some(thread),
        })
    }

    pub fn try_submit(&self, input: Snapshot) -> Result<u64, TrySendError<Snapshot>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TrySendError::Disconnected(input));
        }
        let mut count = self.outstanding.load(Ordering::Relaxed);
        loop {
            if count >= CAPACITY {
                return Err(TrySendError::Full(input));
            }
            match self.outstanding.compare_exchange_weak(
                count,
                count + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        let job = Job { ticket, input };
        let result = match self.requests.as_ref() {
            Some(requests) => requests.try_send(job),
            None => Err(TrySendError::Disconnected(job)),
        };
        match result {
            Ok(()) => Ok(ticket),
            Err(TrySendError::Full(job)) => {
                self.outstanding.fetch_sub(1, Ordering::Release);
                Err(TrySendError::Full(job.input))
            }
            Err(TrySendError::Disconnected(job)) => {
                self.outstanding.fetch_sub(1, Ordering::Release);
                Err(TrySendError::Disconnected(job.input))
            }
        }
    }

    pub fn try_recv(&self) -> Result<Completion, TryRecvError> {
        // Once warm code has been published, interpreted instructions must not
        // pay for an empty channel poll on every dispatch. A zero permit count
        // proves there is no queued, active, or completed work to receive.
        if self.outstanding.load(Ordering::Acquire) == 0 {
            return Err(TryRecvError::Empty);
        }
        let completion = self
            .results
            .as_ref()
            .ok_or(TryRecvError::Disconnected)?
            .try_recv()?;
        self.outstanding.fetch_sub(1, Ordering::Release);
        Ok(completion)
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
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
    use std::sync::{Barrier, Mutex};

    fn snapshot() -> Snapshot {
        let mut memory = Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, READ | WRITE | EXEC).unwrap();
        memory.write(0x10000, &[0x90, 0xc3]).unwrap();
        Snapshot::capture(&Region::new(0x10000, 2), &memory).unwrap()
    }

    fn receive(worker: &Worker) -> Completion {
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
        let worker = Worker::with_compiler(|_| Err("deterministic completion".into())).unwrap();
        let input = snapshot();
        let tickets: Vec<_> = (0..CAPACITY)
            .map(|_| worker.try_submit(input.clone()).unwrap())
            .collect();
        assert!(matches!(
            worker.try_submit(input.clone()),
            Err(TrySendError::Full(_))
        ));
        for expected in tickets {
            let completion = receive(&worker);
            assert_eq!(completion.ticket, expected);
            assert!(completion.result.is_err());
        }
        assert!(worker.try_submit(input).is_ok());
        receive(&worker);
    }

    #[test]
    fn drop_cancels_queued_requests_after_the_current_job_without_blocking_on_results() {
        let started = Arc::new(Barrier::new(2));
        let released = Arc::new(Barrier::new(2));
        let calls = Arc::new(AtomicUsize::new(0));
        let worker_started = Arc::clone(&started);
        let worker_released = Arc::clone(&released);
        let worker_calls = Arc::clone(&calls);
        let worker = Worker::with_compiler(move |_| {
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
        let finished = Arc::new(Mutex::new(false));
        let mark_finished = Arc::clone(&finished);
        let dropping = thread::spawn(move || {
            drop(worker);
            *mark_finished.lock().unwrap() = true;
        });
        while !closed.load(Ordering::Acquire) {
            thread::yield_now();
        }
        released.wait();
        dropping.join().unwrap();
        assert!(*finished.lock().unwrap());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn full_completed_queue_does_not_deadlock_drop() {
        let done = Arc::new(AtomicUsize::new(0));
        let worker_done = Arc::clone(&done);
        let worker = Worker::with_compiler(move |_| {
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
        let worker = Worker::new().unwrap();
        let input = snapshot(); // Its Memory has already been dropped.
        let ticket = worker.try_submit(input).unwrap();
        let completion = receive(&worker);
        assert_eq!(completion.ticket, ticket);
        assert_eq!(completion.region.start, 0x10000);
        assert!(completion.result.unwrap().entries.contains(&0x10000));
    }
}
