//! One word for execution interruptions, deadlines and owner work.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

pub const INTERRUPT: u32 = 1;
pub const DEADLINE: u32 = 2;
pub const STOP: u32 = 4;
pub const WORK: u32 = 8;

/// A retained timer worker sets the pending word for deadline notifications.
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

    /// Arms `at`, or disarms. Only an earlier deadline wakes the worker: for
    /// a later one or none it wakes at the old time, finds nothing due and
    /// sleeps again, so a run's entry costs no thread switch.
    pub fn set(&self, at: Option<Instant>, pending: &AtomicU32) {
        let mut state = self.state.0.lock().unwrap();
        pending.fetch_and(!DEADLINE, Ordering::AcqRel);
        let earlier = at.is_some_and(|at| state.at.is_none_or(|armed| at < armed));
        state.at = at;
        if earlier {
            self.state.1.notify_one();
        }
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.state.0.lock().unwrap().closed = true;
        self.state.1.notify_one();
        self.worker.take().unwrap().join().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn due(pending: &AtomicU32) -> bool {
        pending.load(Ordering::Acquire) & DEADLINE != 0
    }

    fn wait_due(pending: &AtomicU32) {
        let limit = Instant::now() + Duration::from_secs(5);
        while !due(pending) {
            assert!(Instant::now() < limit, "deadline never fired");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_later_or_cleared_deadline_never_fires_early_and_an_earlier_one_fires() {
        let pending = Arc::new(AtomicU32::new(0));
        let deadline = Deadline::new(pending.clone());
        let ms = Duration::from_millis;
        // Moved later without a wake: the worker wakes at the old time and
        // sleeps again.
        let start = Instant::now();
        deadline.set(Some(start + ms(10)), &pending);
        deadline.set(Some(start + ms(80)), &pending);
        std::thread::sleep(ms(40));
        assert!(!due(&pending) || start.elapsed() >= ms(80));
        wait_due(&pending);
        assert!(start.elapsed() >= ms(80));
        // Cleared: nothing fires.
        deadline.set(Some(Instant::now() + ms(10)), &pending);
        deadline.set(None, &pending);
        std::thread::sleep(ms(40));
        assert!(!due(&pending));
        // An earlier deadline wakes the worker from a long wait.
        deadline.set(Some(Instant::now() + Duration::from_secs(60)), &pending);
        deadline.set(Some(Instant::now() + ms(5)), &pending);
        wait_due(&pending);
    }
}
