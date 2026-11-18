//! Staged packet decoding for admitted codecs. Only the owner accepts results.
use adv_device::codec_workers;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
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
}

pub(super) trait Staging: Send + 'static {
    type Input: PartialEq + Send + Sync + 'static;
    type Control: Clone + PartialEq + Send + 'static;
    type Stage: Send + 'static;

    fn stage(&mut self, input: &Self::Input, control: &mut Self::Control) -> Self::Stage;
    fn restore(&mut self, committed: &Self::Stage);
}

struct Prediction<S: Staging> {
    input: Arc<S::Input>,
    result: Receiver<(S::Control, S::Stage)>,
}

struct Batch<S: Staging> {
    pending: VecDeque<Prediction<S>>,
    queued: Arc<Mutex<Option<S>>>,
    finished: Receiver<S>,
    cancelled: Arc<AtomicBool>,
}

impl<S: Staging> Drop for Batch<S> {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.queued.lock().unwrap().take();
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
        self.batch.as_ref().is_some_and(|b| !b.pending.is_empty())
    }

    pub(super) fn publish(&mut self, stage: S::Stage) {
        self.committed = stage;
    }

    pub(super) fn invalidate(&mut self, stats: &mut Stats) {
        if let Some(batch) = &mut self.batch
            && !batch.pending.is_empty()
        {
            batch.cancelled.store(true, Ordering::Relaxed);
            stats.discarded += batch.pending.len() as u64;
            batch.pending.clear();
        }
    }

    pub(super) fn decoder(&mut self, stats: &mut Stats) -> &mut S {
        self.invalidate(stats);
        if let Some(batch) = self.batch.take() {
            let queued = batch.queued.lock().unwrap().take();
            let mut decoder = queued.unwrap_or_else(|| wait(&batch.finished, stats));
            if batch.cancelled.load(Ordering::Relaxed) {
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
        let Some(prediction) = self.batch.as_ref().and_then(|b| b.pending.front()) else {
            return false;
        };
        if *prediction.input == *input {
            let (expected, result) = wait(&prediction.result, stats);
            if expected == *control {
                self.batch.as_mut().unwrap().pending.pop_front();
                self.committed = result;
                stats.committed += 1;
                return true;
            }
            stats.mismatched_controls += 1;
        }
        stats.mismatched += 1;
        self.invalidate(stats);
        false
    }

    pub(super) fn predict(
        &mut self,
        mut control: S::Control,
        depth: usize,
        predict: impl FnOnce(usize) -> Vec<S::Input>,
        stats: &mut Stats,
    ) -> bool {
        if self.has_predictions() || !codec_workers::available() {
            return false;
        }
        let mut jobs = Vec::new();
        let mut pending = VecDeque::new();
        for input in predict(depth).into_iter().take(depth) {
            let input = Arc::new(input);
            let (send, result) = mpsc::channel();
            pending.push_back(Prediction {
                input: Arc::clone(&input),
                result,
            });
            jobs.push((input, send));
        }
        if jobs.is_empty() {
            return false;
        }
        stats.predicted += jobs.len() as u64;
        self.decoder(stats);
        let queued = Arc::new(Mutex::new(self.decoder.take()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (finish, finished) = mpsc::channel();
        self.batch = Some(Batch {
            pending,
            queued: Arc::clone(&queued),
            finished,
            cancelled: Arc::clone(&cancelled),
        });
        codec_workers::submit_speculation(move || {
            let Some(mut decoder) = queued.lock().unwrap().take() else {
                return;
            };
            for (input, send) in jobs {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let before = control.clone();
                let result = decoder.stage(&input, &mut control);
                if send.send((before, result)).is_err() {
                    break;
                }
            }
            let _ = finish.send(decoder);
        });
        true
    }
}

fn wait<T>(receiver: &Receiver<T>, stats: &mut Stats) -> T {
    match receiver.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => {
            let started = Instant::now();
            let result = receiver.recv().expect("codec worker completed its job");
            stats.owner_wait_ns += started.elapsed().as_nanos() as u64;
            result
        }
        Err(TryRecvError::Disconnected) => panic!("codec worker lost its job"),
    }
}
