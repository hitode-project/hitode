//! Frame spin (PROC-CLOCK-4): a context that completed a frame and then only
//! polls is waiting for its next frame time. Its polling round may wait for
//! the first vsync after that frame instead of the next millisecond, once per
//! frame. A loop that draws or works between rounds never qualifies.
use crate::Sys;
use std::time::Duration;

/// Guest run time between two rounds below which the context only polled.
const SPIN_RUN_US: u64 = 500;

/// The host display's frame clock, installed only where frames reach a host
/// mailbox.
pub trait FramePacer: Send + Sync {
    /// Time from now until the next vsync, if the host knows its grid.
    fn until_vsync(&self) -> Option<Duration>;
    /// A key or mouse button is held: the user may be skipping, so frames
    /// stay uncapped.
    fn input_held(&self) -> bool;
}

/// What polling rounds know about a context's last frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameSpin {
    /// The first vsync after its last completed frame, until a wait takes it.
    until: Option<u64>,
    /// Polling rounds since that frame.
    rounds: u32,
    /// Run time since its last polling round.
    ran_us: u64,
}

impl FrameSpin {
    pub(crate) fn ran(&mut self, elapsed: Duration) {
        self.ran_us = self
            .ran_us
            .saturating_add(elapsed.as_micros().min(u128::from(u64::MAX)) as u64);
    }
}

impl Sys {
    /// The current context completed a frame.
    pub(crate) fn frame_spin_frame(&mut self) {
        let Some(until) = self.frame_pacer.as_ref().and_then(|p| p.until_vsync()) else {
            return;
        };
        let until = self.clock.now_us() + until.as_micros() as u64;
        if let Some(c) = self.contexts.get_mut(self.current as usize) {
            (c.spin.until, c.spin.rounds) = (Some(until), 0);
        }
    }

    /// A polling round of the ready contexts: the vsync to wait for when
    /// every one of them is spinning after a frame.
    pub(crate) fn frame_spin_round(&mut self, soon: u64) -> Option<u64> {
        let held = self.frame_pacer.as_ref().is_none_or(|p| p.input_held());
        let mut wait = (!held).then_some(u64::MAX);
        for c in self.contexts.iter_mut().filter(|c| c.is_ready()) {
            let spin = &mut c.spin;
            wait = wait.zip(spin.until).and_then(|(w, until)| {
                (spin.rounds > 0 && spin.ran_us < SPIN_RUN_US && until > soon)
                    .then_some(w.min(until))
            });
            spin.rounds = spin.rounds.saturating_add(1);
            spin.ran_us = 0;
        }
        if wait.is_some() {
            for c in self.contexts.iter_mut().filter(|c| c.is_ready()) {
                c.spin.until = None;
            }
            self.scheduling.frame_spin_waits += 1;
        }
        wait
    }
}
