//! A sleeping timer signals expiration; only the engine's owner stops at a block boundary.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    deadline: Option<Instant>,
    wake_at: Option<Instant>,
    shutdown: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    expired: Arc<AtomicBool>,
}

pub(super) struct Watchdog {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    pub(super) fn new(expired: Arc<AtomicBool>) -> Self {
        let shared = Arc::new(Shared { expired, ..Shared::default() });
        let timer = shared.clone();
        let thread = thread::Builder::new()
            .name("pal-cpu-watchdog".into())
            .spawn(move || {
                let mut state = timer.state.lock().unwrap();
                loop {
                    if state.shutdown {
                        break;
                    }
                    let Some(wake_at) = state.wake_at else {
                        state = timer.changed.wait(state).unwrap();
                        continue;
                    };
                    let now = Instant::now();
                    if now < wake_at {
                        (state, _) = timer.changed.wait_timeout(state, wake_at - now).unwrap();
                        continue;
                    }
                    match state.deadline {
                        Some(deadline) if now >= deadline => {
                            timer.expired.store(true, Ordering::Release);
                            state.wake_at = None;
                        }
                        deadline => state.wake_at = deadline,
                    }
                }
            })
            .expect("spawn CPU watchdog");
        Self { shared, thread: Some(thread) }
    }

    pub(super) fn arm(&self, budget: Duration) {
        let deadline = Instant::now().checked_add(budget).expect("execution budget exceeds host clock range");
        let mut state = self.shared.state.lock().unwrap();
        state.deadline = Some(deadline);
        self.shared.expired.store(false, Ordering::Release);
        // Keep an already scheduled earlier wake-up. Short runs need neither a timer
        // wake-up on every park nor a syscall on every new arm.
        if state.wake_at.is_none_or(|wake| deadline < wake) {
            state.wake_at = Some(deadline);
            self.shared.changed.notify_one();
        }
    }

    pub(super) fn disarm(&self) -> bool {
        let mut state = self.shared.state.lock().unwrap();
        state.deadline = None;
        self.shared.expired.load(Ordering::Acquire)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.shutdown = true;
            self.shared.changed.notify_one();
        }
        self.thread.take().unwrap().join().expect("CPU watchdog panicked");
    }
}
