//! Nonblocking submission exclusion for native swapchain configuration.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU32, Ordering},
};
use std::time::{Duration, Instant};

use super::Error;
use adv_sync::WakeEvent;

const PAUSED: u32 = 1 << 31;
const READERS: u32 = PAUSED - 1;

pub(super) struct Gate {
    state: AtomicU32,
    drained: WakeEvent,
    execution: OnceLock<WakeEvent>,
    frame_waiter: AtomicBool,
}
impl Gate {
    pub(super) fn new() -> std::io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            state: AtomicU32::new(0),
            drained: WakeEvent::new()?,
            execution: OnceLock::new(),
            frame_waiter: AtomicBool::new(false),
        }))
    }
    pub(super) fn enter(self: &Arc<Self>) -> Result<Lease, Error> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & PAUSED == 0 && state & READERS < READERS).then(|| state + 1)
            })
            .map_err(|_| Error::Busy)?;
        Ok(Lease(self.clone()))
    }
    pub(super) fn pause(self: &Arc<Self>) -> Result<SubmissionPause, Error> {
        self.state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & PAUSED == 0).then_some(state | PAUSED)
            })
            .map_err(|_| Error::Busy)?;
        Ok(SubmissionPause { gate: self.clone() })
    }
    pub(super) fn set_wake(&self, wake: WakeEvent) -> Result<(), WakeEvent> {
        self.execution.set(wake)
    }
    pub(super) fn wait_for_frame(&self) {
        self.frame_waiter.store(true, Ordering::Release);
    }
    #[cfg(target_vendor = "apple")]
    pub(super) fn notify_execution(&self) {
        if let Some(wake) = self.execution.get() {
            wake.signal();
        }
    }
    pub(super) fn frame_available(&self) {
        if self.frame_waiter.swap(false, Ordering::AcqRel)
            && let Some(wake) = self.execution.get()
        {
            wake.signal();
        }
    }
}

pub(super) struct Lease(Arc<Gate>);
impl Drop for Lease {
    fn drop(&mut self) {
        let previous = self.0.state.fetch_sub(1, Ordering::AcqRel);
        if previous & PAUSED != 0 && previous & READERS == 1 {
            self.0.drained.notify();
        }
    }
}

/// Queue submissions stay paused until this token is dropped. Creating the
/// token never waits. `wait_drained` belongs only to the common backend worker.
pub struct SubmissionPause {
    gate: Arc<Gate>,
}
impl SubmissionPause {
    pub fn wait_drained(&self) -> Result<(), Error> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.gate.state.load(Ordering::Acquire) & READERS != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero()
                || !self
                    .gate
                    .drained
                    .wait(Some(remaining))
                    .map_err(|error| Error::Device(error.to_string()))?
            {
                return Err(Error::Device(
                    "GPU submitter did not release its configuration lease".into(),
                ));
            }
        }
        Ok(())
    }
}
impl Drop for SubmissionPause {
    fn drop(&mut self) {
        let previous = self.gate.state.fetch_and(!PAUSED, Ordering::AcqRel);
        if previous & PAUSED != 0
            && let Some(wake) = self.gate.execution.get()
        {
            wake.signal();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_sync::background::{Builder, Priority, Status, Task};
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    #[test]
    fn pause_is_nonblocking_nested_pause_is_refused_and_only_release_wakes_execution() {
        let gate = Gate::new().unwrap();
        let wake = WakeEvent::new().unwrap();
        assert!(gate.set_wake(wake.clone()).is_ok());
        let lease = gate.enter().unwrap();
        let pause = gate.pause().unwrap();
        assert!(matches!(gate.enter(), Err(Error::Busy)));
        assert!(matches!(gate.pause(), Err(Error::Busy)));
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        drop(lease);
        pause.wait_drained().unwrap();
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
        drop(pause);
        assert!(wake.wait(Some(Duration::ZERO)).unwrap());
        assert!(gate.enter().is_ok());
        assert!(!wake.wait(Some(Duration::ZERO)).unwrap());
    }

    #[test]
    fn pause_and_submission_linearize_and_last_reader_wake_cannot_be_lost() {
        let gate = Gate::new().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicUsize::new(0));
        let worker_gate = gate.clone();
        let worker_stopped = stopped.clone();
        let worker_active = active.clone();
        let worker = std::thread::spawn(move || {
            while !worker_stopped.load(Ordering::Acquire) {
                if let Ok(lease) = worker_gate.enter() {
                    worker_active.fetch_add(1, Ordering::AcqRel);
                    std::thread::yield_now();
                    worker_active.fetch_sub(1, Ordering::AcqRel);
                    drop(lease);
                }
            }
        });
        for _ in 0..2000 {
            let pause = gate.pause().unwrap();
            pause.wait_drained().unwrap();
            assert_eq!(active.load(Ordering::Acquire), 0);
            assert!(matches!(gate.enter(), Err(Error::Busy)));
            drop(pause);
        }
        stopped.store(true, Ordering::Release);
        worker.join().unwrap();
        assert_eq!(gate.state.load(Ordering::Acquire), 0);
    }

    #[test]
    fn rejected_configuration_returns_its_pause_and_accepted_cleanup_survives_shutdown() {
        let gate = Gate::new().unwrap();
        let execution = WakeEvent::new().unwrap();
        let ui = WakeEvent::new().unwrap();
        assert!(gate.set_wake(execution.clone()).is_ok());
        let mut builder = Builder::new(execution.clone()).unwrap();
        let mut sender = builder.sender(1, 1).unwrap();
        let mut service = builder.start().unwrap();
        let entered = WakeEvent::new().unwrap();
        let finish = WakeEvent::new().unwrap();
        let job_entered = entered.clone();
        let job_finish = finish.clone();
        sender
            .try_submit(
                Priority::Urgent,
                Task::new(move || {
                    job_entered.notify();
                    assert!(job_finish.wait(Some(Duration::from_secs(10))).unwrap());
                })
                .with_wake(ui.clone()),
            )
            .unwrap();
        assert!(entered.wait(Some(Duration::from_secs(10))).unwrap());
        // Fill the one-slot lane while its worker is occupied.
        sender
            .try_submit(Priority::Urgent, Task::new(|| {}).with_wake(ui.clone()))
            .unwrap();
        let pause = gate.pause().unwrap();
        let rejected = sender
            .try_submit(
                Priority::Urgent,
                Task::new(move || {
                    pause.wait_drained().unwrap();
                })
                .finish_on_shutdown()
                .with_wake(ui.clone()),
            )
            .err()
            .expect("lane is full");
        assert!(matches!(gate.enter(), Err(Error::Busy)));
        // No job was accepted: dropping its returned ownership releases pause.
        drop(rejected);
        assert!(execution.wait(Some(Duration::ZERO)).unwrap());
        assert!(gate.enter().is_ok());
        finish.notify();
        service.shutdown();
        // Service shutdown separately notifies execution that its owner closed.
        let _ = execution.wait(Some(Duration::ZERO)).unwrap();

        let mut builder = Builder::new(execution.clone()).unwrap();
        let mut sender = builder.sender(1, 1).unwrap();
        let mut service = builder.start().unwrap();
        let reader = gate.enter().unwrap();
        let pause = gate.pause().unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let job_finished = finished.clone();
        let ticket = sender
            .try_submit(
                Priority::Urgent,
                Task::new(move || {
                    pause.wait_drained().unwrap();
                    job_finished.store(true, Ordering::Release);
                })
                .finish_on_shutdown()
                .with_wake(ui.clone()),
            )
            .unwrap();
        assert!(
            !ticket.cancel(),
            "an accepted owned presenter cannot be discarded"
        );
        assert!(matches!(gate.enter(), Err(Error::Busy)));
        drop(reader); // Last reader wakes the required job; no periodic poll.
        service.shutdown();
        assert_eq!(ticket.status(), Status::Completed);
        assert!(finished.load(Ordering::Acquire));
        assert!(execution.wait(Some(Duration::ZERO)).unwrap());
        assert!(gate.enter().is_ok());
    }
}
