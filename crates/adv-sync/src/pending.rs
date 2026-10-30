//! One word for execution interruptions, deadlines and owner work.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

pub const INTERRUPT: u32 = 1;
pub const DEADLINE: u32 = 2;
pub const STOP: u32 = 4;
pub const WORK: u32 = 8;

/// A retained timer worker sets the pending word; host calls never read a clock.
pub struct Deadline {
    state: Arc<(Mutex<State>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct State {
    at: Option<Instant>,
    closed: bool,
}

impl Deadline {
    pub fn new(pending: Arc<AtomicU32>) -> Self {
        let state = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let shared = state.clone();
        let worker = std::thread::spawn(move || {
            let (lock, wake) = &*shared;
            let mut state = lock.lock().unwrap();
            while !state.closed {
                if let Some(at) = state.at {
                    let now = Instant::now();
                    if now >= at {
                        pending.fetch_or(DEADLINE, Ordering::Release);
                        state.at = None;
                    } else {
                        state = wake.wait_timeout(state, at - now).unwrap().0;
                    }
                } else {
                    state = wake.wait(state).unwrap();
                }
            }
        });
        Self {
            state,
            worker: Some(worker),
        }
    }

    pub fn set(&self, at: Option<Instant>, pending: &AtomicU32) {
        let mut state = self.state.0.lock().unwrap();
        pending.fetch_and(!DEADLINE, Ordering::AcqRel);
        state.at = at;
        self.state.1.notify_one();
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.state.0.lock().unwrap().closed = true;
        self.state.1.notify_one();
        self.worker.take().unwrap().join().unwrap();
    }
}
