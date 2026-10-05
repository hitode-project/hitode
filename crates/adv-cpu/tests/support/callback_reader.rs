//! Test-only progress handshake. A start barrier alone cannot prove that a
//! callback ran before a fast writer completed. Each acknowledgement follows a
//! fresh read after the request; the reader also runs continuously between them.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::Duration;

pub(super) struct CallbackReader {
    stop: Arc<AtomicBool>,
    request: mpsc::Sender<()>,
    observed: mpsc::Receiver<usize>,
    worker: Option<JoinHandle<usize>>,
    last: usize,
}
impl CallbackReader {
    pub(super) fn new(mut check: impl FnMut(usize) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let (request, requests) = mpsc::channel();
        let (observations, observed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut reads = 0;
            while !done.load(Ordering::Acquire) {
                let checkpoint = match requests.try_recv() {
                    Ok(()) => true,
                    Err(mpsc::TryRecvError::Empty) => false,
                    Err(mpsc::TryRecvError::Disconnected) => break,
                };
                check(reads);
                reads += 1;
                if checkpoint && observations.send(reads).is_err() {
                    break;
                }
            }
            reads
        });
        Self {
            stop,
            request,
            observed,
            worker: Some(worker),
            last: 0,
        }
    }
    pub(super) fn checkpoint(&mut self) {
        self.request.send(()).expect("callback reader stopped");
        // A hang bound, not a scheduling or throughput assumption.
        let reads = self
            .observed
            .recv_timeout(Duration::from_secs(30))
            .expect("callback did not complete a requested read");
        assert!(
            reads > self.last,
            "checkpoint must acknowledge a fresh read"
        );
        self.last = reads;
    }
    pub(super) fn finish(mut self) -> usize {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .expect("callback reader panicked")
    }
}
impl Drop for CallbackReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // Clean up even when the writer/checkpoint is unwinding; finish()
            // propagates callback failures on the successful writer path.
            let _ = worker.join();
        }
    }
}
