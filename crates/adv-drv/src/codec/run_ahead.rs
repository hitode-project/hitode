//! Staged packet decoding for admitted codecs. Only the owner accepts results.
use adv_device::codec_workers;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    time::Instant,
};

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub predicted: u64,
    pub committed: u64,
    pub mismatched: u64,
    pub mismatched_controls: u64,
    pub discarded: u64,
    pub no_prediction: u64,
    pub no_prediction_callback: u64,
    pub owner_wait_ns: u64,
    pub wait_not_started_ns: u64,
    pub wait_in_flight_ns: u64,
    pub lead_packets_sum: u64,
    pub linked: u64,
    pub unlinked_wrote: u64,
    pub readahead_packets: u64,
}

pub(super) trait Staging: Send + 'static {
    type Input: PartialEq + Send + Sync + 'static;
    type Control: Clone + PartialEq + Send + 'static;
    type Stage: Send + 'static;

    fn stage(&mut self, input: &Self::Input, control: &mut Self::Control) -> Self::Stage;
    fn restore(&mut self, committed: &Self::Stage);
}

type Input<S> = Arc<OnceLock<<S as Staging>::Input>>;
type ResultOf<S> = Option<(<S as Staging>::Control, <S as Staging>::Stage)>;
const STARTED: u8 = 1;
const READY: u8 = 2;
const UNAVAILABLE: u8 = 3;

struct Prediction<S: Staging> {
    input: Input<S>,
    progress: Arc<AtomicU8>,
    result: Receiver<ResultOf<S>>,
}

impl<S: Staging> Prediction<S> {
    fn unavailable(&self) -> bool {
        self.progress.load(Ordering::Relaxed) == UNAVAILABLE
    }
}

struct Request<S: Staging> {
    input: Input<S>,
    progress: Arc<AtomicU8>,
    result: Sender<ResultOf<S>>,
}

struct Worker<S: Staging> {
    decoder: S,
    control: S::Control,
    predict: Option<Box<dyn FnMut() -> Option<S::Input> + Send>>,
}

struct Queue<S: Staging> {
    idle: Option<Worker<S>>,
    requests: VecDeque<Request<S>>,
    scheduled: bool,
    cancelled: bool,
}

struct Batch<S: Staging> {
    pending: VecDeque<Prediction<S>>,
    queue: Arc<(Mutex<Queue<S>>, Condvar)>,
    file: bool,
}

impl<S: Staging> Batch<S> {
    fn request(&mut self, inputs: impl IntoIterator<Item = Option<S::Input>>) {
        let (lock, _) = &*self.queue;
        let mut queue = lock.lock().unwrap();
        for input in inputs {
            let input = Arc::new(input.map_or_else(OnceLock::new, OnceLock::from));
            let progress = Arc::new(AtomicU8::new(0));
            let (send, result) = mpsc::channel();
            queue.requests.push_back(Request {
                input: Arc::clone(&input),
                progress: Arc::clone(&progress),
                result: send,
            });
            self.pending.push_back(Prediction {
                input,
                progress,
                result,
            });
        }
        if queue.scheduled || queue.requests.is_empty() {
            return;
        }
        queue.scheduled = true;
        let shared = Arc::clone(&self.queue);
        codec_workers::submit_speculation(move || {
            let (lock, ready) = &*shared;
            let mut queue = lock.lock().unwrap();
            let Some(mut worker) = queue.idle.take() else {
                return;
            };
            while let Some(request) = queue.requests.pop_front() {
                request.progress.store(STARTED, Ordering::Relaxed);
                drop(queue);
                let input = request.input.get().or_else(|| {
                    let input = worker.predict.as_mut()?()?;
                    Some(request.input.get_or_init(|| input))
                });
                let result = input.map(|input| {
                    let before = worker.control.clone();
                    let stage = worker.decoder.stage(input, &mut worker.control);
                    request.progress.store(READY, Ordering::Relaxed);
                    (before, stage)
                });
                if result.is_none() {
                    worker.predict = None;
                    request.progress.store(UNAVAILABLE, Ordering::Relaxed);
                }
                let _ = request.result.send(result);
                queue = lock.lock().unwrap();
            }
            queue.idle = Some(worker);
            queue.scheduled = false;
            ready.notify_one();
        });
    }
}

impl<S: Staging> Drop for Batch<S> {
    fn drop(&mut self) {
        let mut queue = self.queue.0.lock().unwrap();
        queue.requests.clear();
        queue.idle.take();
    }
}

pub(super) struct RunAhead<S: Staging> {
    decoder: Option<S>,
    committed: S::Stage,
    batch: Option<Batch<S>>,
}

impl<S: Staging> RunAhead<S> {
    pub(super) fn new(decoder: S, initial: S::Stage) -> Self {
        Self {
            decoder: Some(decoder),
            committed: initial,
            batch: None,
        }
    }

    pub(super) fn committed(&self) -> &S::Stage {
        &self.committed
    }

    pub(super) fn has_predictions(&self) -> bool {
        self.batch
            .as_ref()
            .and_then(|batch| batch.pending.front())
            .is_some_and(|prediction| !prediction.unavailable())
    }

    pub(super) fn publish(&mut self, stage: S::Stage) {
        self.committed = stage;
    }

    pub(super) fn invalidate(&mut self, stats: &mut Stats) {
        if let Some(batch) = &mut self.batch
            && !batch.pending.is_empty()
        {
            let mut queue = batch.queue.0.lock().unwrap();
            queue.cancelled = true;
            queue.requests.clear();
            stats.discarded += batch
                .pending
                .iter()
                .filter(|p| p.input.get().is_some())
                .count() as u64;
            batch.pending.clear();
            if let Some(worker) = &mut queue.idle {
                worker.predict = None;
            }
        }
    }

    pub(super) fn decoder(&mut self, stats: &mut Stats) -> &mut S {
        self.invalidate(stats);
        if let Some(batch) = self.batch.take() {
            let (lock, ready) = &*batch.queue;
            let mut queue = lock.lock().unwrap();
            if queue.idle.is_none() {
                let start = Instant::now();
                queue = ready.wait_while(queue, |q| q.idle.is_none()).unwrap();
                record_wait(stats, true, start);
            }
            let mut decoder = queue.idle.take().unwrap().decoder;
            if queue.cancelled {
                decoder.restore(&self.committed);
            }
            self.decoder = Some(decoder);
        }
        self.decoder.as_mut().unwrap()
    }

    pub(super) fn try_commit(
        &mut self,
        input: &S::Input,
        control: &S::Control,
        stats: &mut Stats,
    ) -> bool {
        let Some(batch) = &mut self.batch else {
            return false;
        };
        let Some(prediction) = batch.pending.front() else {
            return false;
        };
        let started = prediction.progress.load(Ordering::Relaxed) >= STARTED;
        let lead = batch
            .pending
            .iter()
            .take_while(|p| p.progress.load(Ordering::Relaxed) == READY)
            .count();
        let mut result = None;
        if prediction
            .input
            .get()
            .is_none_or(|expected| expected == input)
        {
            result = match prediction.result.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    let start = Instant::now();
                    let result = prediction
                        .result
                        .recv()
                        .expect("codec worker completed its job");
                    record_wait(stats, started, start);
                    result
                }
                Err(TryRecvError::Disconnected) => panic!("codec worker lost its job"),
            };
            if prediction.input.get().is_none() {
                // The file predictor stopped before producing this packet.
                self.invalidate(stats);
                return false;
            }
        }
        if let Some((expected, result)) = result
            && prediction.input.get() == Some(input)
        {
            if expected == *control {
                batch.pending.pop_front();
                self.committed = result;
                stats.committed += 1;
                stats.lead_packets_sum += lead as u64;
                return true;
            }
            stats.mismatched_controls += 1;
        }
        stats.mismatched += 1;
        self.invalidate(stats);
        false
    }

    fn start(
        &mut self,
        control: S::Control,
        predict: Option<Box<dyn FnMut() -> Option<S::Input> + Send>>,
        stats: &mut Stats,
    ) {
        self.decoder(stats);
        self.batch = Some(Batch {
            pending: VecDeque::new(),
            file: predict.is_some(),
            queue: Arc::new((
                Mutex::new(Queue {
                    idle: Some(Worker {
                        decoder: self.decoder.take().unwrap(),
                        control,
                        predict,
                    }),
                    requests: VecDeque::new(),
                    scheduled: false,
                    cancelled: false,
                }),
                Condvar::new(),
            )),
        });
    }

    pub(super) fn predict(
        &mut self,
        control: S::Control,
        depth: usize,
        predict: impl FnOnce(usize) -> Vec<S::Input>,
        stats: &mut Stats,
    ) -> bool {
        if self.has_predictions() || !codec_workers::available() {
            return false;
        }
        let mut inputs = predict(depth);
        inputs.truncate(depth);
        if inputs.is_empty() {
            return false;
        }
        stats.predicted += inputs.len() as u64;
        self.start(control, None, stats);
        self.batch
            .as_mut()
            .unwrap()
            .request(inputs.into_iter().map(Some));
        true
    }

    pub(super) fn predict_file(
        &mut self,
        control: S::Control,
        depth: usize,
        mut predict: impl FnMut() -> Option<(S::Input, bool)> + Send + 'static,
        counts: Arc<FileCounts>,
        stats: &mut Stats,
    ) -> bool {
        if self.has_predictions() || !codec_workers::available() {
            return false;
        }
        self.start(
            control,
            Some(Box::new(move || {
                let (input, from_file) = predict()?;
                counts.predicted.fetch_add(1, Ordering::Relaxed);
                if from_file {
                    counts.readahead_packets.fetch_add(1, Ordering::Relaxed);
                }
                Some(input)
            })),
            stats,
        );
        self.refill_file(depth)
    }

    pub(super) fn refill_file(&mut self, depth: usize) -> bool {
        let Some(batch) = &mut self.batch else {
            return false;
        };
        if !batch.file
            || batch.pending.front().is_some_and(Prediction::unavailable)
            || batch.pending.is_empty() && batch.queue.0.lock().unwrap().cancelled
        {
            return false;
        }
        batch.request((batch.pending.len()..depth).map(|_| None));
        true
    }
}

#[derive(Default)]
pub(super) struct FileCounts {
    pub predicted: AtomicU64,
    pub readahead_packets: AtomicU64,
}

fn record_wait(stats: &mut Stats, started: bool, start: Instant) {
    let elapsed = start.elapsed().as_nanos() as u64;
    stats.owner_wait_ns += elapsed;
    if started {
        stats.wait_in_flight_ns += elapsed;
    } else {
        stats.wait_not_started_ns += elapsed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Decoder(u64);

    impl Staging for Decoder {
        type Input = u64;
        type Control = ();
        type Stage = u64;

        fn stage(&mut self, input: &u64, _: &mut ()) -> u64 {
            self.0 += input;
            self.0
        }

        fn restore(&mut self, committed: &u64) {
            self.0 = *committed;
        }
    }

    // spec: CODEC-CALL-3
    #[test]
    fn exhausted_file_prediction_allows_the_next_page_to_stage() {
        let mut ahead = RunAhead::new(Decoder(0), 0);
        let mut stats = Stats::default();
        let mut first = Some((5, true));
        assert!(ahead.predict_file((), 3, move || first.take(), Arc::default(), &mut stats));
        assert!(ahead.try_commit(&5, &(), &mut stats));

        let blank = ahead.batch.as_ref().unwrap().pending.front().unwrap();
        assert!(blank.result.recv().unwrap().is_none());
        assert!(!ahead.has_predictions());
        assert!(!ahead.refill_file(3));
        assert!(ahead.predict((), 3, |_| vec![8], &mut stats));
        assert!(ahead.try_commit(&8, &(), &mut stats));
        assert_eq!(*ahead.committed(), 13);
        assert_eq!(stats.committed, 2);
        assert_eq!(stats.discarded, 0);
    }
}
