//! The single guest time source. timeGetTime, GetTickCount, QueryPerformanceCounter and
//! every wait deadline read this clock, so a run can be made deterministic (virtual time)
//! or recorded and replayed later.
//! AArch64 reads its monotonic counter directly, with integer scaling shared
//! by the machine and service bodies; no guest floating-point flags change.

use crate::{CtxId, Env, Outcome, Wait};
use std::cell::Cell;
use std::time::Instant;

/// NT system time of 1970-01-01.
pub const UNIX_EPOCH_SYSTEM_TIME: u64 = 116_444_736_000_000_000;
/// Wall-clock start for virtual-time runs: 2020-01-01T00:00:00Z (deterministic).
const VIRTUAL_START_UNIX: u64 = 1_577_836_800;

/// Maximum unplanned wall-time advance. Expected idle waits are credited in
/// full; time discarded after a stall is never repaid as catch-up work.
pub const MAX_PASSIVE_STEP_US: u64 = 50_000;

#[derive(Clone, Copy)]
enum Allowance {
    Working,
    Until(u64),
    UnboundedWait,
}

#[derive(Clone, Copy, Default)]
struct RealProgress {
    observed_us: u64,
    elapsed_us: u64,
    clamps: u64,
    discarded_us: u64,
}
impl RealProgress {
    fn observe(&mut self, wall_us: u64, allowance: Allowance, origin_us: u64) -> u64 {
        let delta = wall_us.saturating_sub(self.observed_us);
        self.observed_us = self.observed_us.max(wall_us);
        let limit = match allowance {
            Allowance::Working => MAX_PASSIVE_STEP_US,
            Allowance::Until(deadline) => deadline
                .saturating_sub(origin_us.saturating_add(self.elapsed_us))
                .saturating_add(MAX_PASSIVE_STEP_US),
            Allowance::UnboundedWait => u64::MAX,
        };
        let accepted = delta.min(limit);
        if accepted != delta {
            self.clamps = self.clamps.saturating_add(1);
            self.discarded_us = self.discarded_us.saturating_add(delta - accepted);
        }
        self.elapsed_us = self.elapsed_us.saturating_add(accepted);
        self.elapsed_us
    }
}

#[derive(Clone, Copy)]
pub enum ClockMode {
    /// Wall-clock time since machine start.
    Real {
        started: Instant,
        paused_at: Option<Instant>,
        paused_for_us: u64,
    },
    /// Time moves only when every context is blocked: it jumps to the earliest deadline.
    Virtual(u64),
}

pub struct Clock {
    /// Added to every reading so tick counts don't start at zero (some code treats 0 as unset).
    pub epoch_us: u64,
    state: std::rc::Rc<ClockState>,
}

#[derive(Clone)]
struct ClockState {
    mode: Cell<ClockMode>,
    active_skip_us: Cell<u64>,
    real_progress: Cell<RealProgress>,
    allowance: Cell<Allowance>,
    #[cfg(target_arch = "aarch64")]
    host: CounterClock,
}

/// The same monotonic hardware counter the native executor uses for deadlines.
/// Scaling at construction keeps clock services free of host library calls.
#[cfg(target_arch = "aarch64")]
#[derive(Clone, Copy)]
struct CounterClock {
    origin: u64,
    multiplier: u64,
    shift: u32,
}

#[cfg(target_arch = "aarch64")]
impl CounterClock {
    fn ticks() -> u64 {
        let ticks;
        // SAFETY: the supported AArch64 hosts expose the virtual counter at EL0.
        unsafe { std::arch::asm!("mrs {}, cntvct_el0", out(reg) ticks, options(nomem, nostack)) };
        ticks
    }

    fn new() -> Self {
        let frequency: u64;
        // SAFETY: CNTFRQ describes the same accessible virtual counter.
        unsafe {
            std::arch::asm!("mrs {}, cntfrq_el0", out(reg) frequency, options(nomem, nostack))
        };
        Self::scaled(Self::ticks(), frequency)
    }

    fn scaled(origin: u64, frequency: u64) -> Self {
        let mut shift = 64;
        let multiplier = loop {
            let multiplier = (1_000_000u128 << shift).div_ceil(u128::from(frequency));
            if let Ok(multiplier) = u64::try_from(multiplier) {
                break multiplier;
            }
            shift -= 1;
        };
        Self {
            origin,
            multiplier,
            shift,
        }
    }

    fn elapsed_us(self) -> u64 {
        self.micros(Self::ticks().wrapping_sub(self.origin))
    }

    fn micros(self, ticks: u64) -> u64 {
        ((u128::from(ticks) * u128::from(self.multiplier)) >> self.shift) as u64
    }
}

impl Clone for Clock {
    fn clone(&self) -> Self {
        Self {
            epoch_us: self.epoch_us,
            state: std::rc::Rc::new((*self.state).clone()),
        }
    }
}

/// QueryPerformanceFrequency. 10 MHz, as on current Windows.
pub const QPC_FREQ: u64 = 10_000_000;

impl Clock {
    pub fn real() -> Self {
        Self::new(ClockMode::Real {
            started: Instant::now(),
            paused_at: None,
            paused_for_us: 0,
        })
    }

    pub fn virtual_time() -> Self {
        Self::new(ClockMode::Virtual(0))
    }

    fn new(mode: ClockMode) -> Self {
        Self {
            epoch_us: 1_000_000,
            state: std::rc::Rc::new(ClockState {
                mode: Cell::new(mode),
                active_skip_us: Cell::new(0),
                real_progress: Cell::new(RealProgress::default()),
                allowance: Cell::new(Allowance::Working),
                #[cfg(target_arch = "aarch64")]
                host: CounterClock::new(),
            }),
        }
    }

    /// Shared clock storage survives exclusive borrows of the machine.
    pub fn body_pointer(&self) -> *const std::ffi::c_void {
        std::rc::Rc::as_ptr(&self.state).cast()
    }

    /// Counter service, including the two virtual-time observations of NT entry.
    /// # Safety
    /// `state` must come from the live execution owner's `body_pointer`.
    #[inline]
    pub unsafe fn body_counter(state: *const std::ffi::c_void, epoch_us: u64) -> u64 {
        // SAFETY: the owner retains this shared allocation for the run.
        let state = unsafe { &*state.cast::<ClockState>() };
        if let ClockMode::Virtual(time) = state.mode.get() {
            state.mode.set(ClockMode::Virtual(time + 2));
        }
        Self::read(state, epoch_us) * 10
    }

    pub fn is_virtual(&self) -> bool {
        matches!(self.state.mode.get(), ClockMode::Virtual(_))
    }

    pub fn now_us(&self) -> u64 {
        Self::read(&self.state, self.epoch_us)
    }

    #[inline]
    fn read(state: &ClockState, epoch_us: u64) -> u64 {
        epoch_us
            + state.active_skip_us.get()
            + match state.mode.get() {
                ClockMode::Real {
                    started,
                    paused_at,
                    paused_for_us,
                } => {
                    let elapsed = || {
                        #[cfg(target_arch = "aarch64")]
                        {
                            state.host.elapsed_us()
                        }
                        #[cfg(not(target_arch = "aarch64"))]
                        {
                            started.elapsed().as_micros() as u64
                        }
                    };
                    let wall_us = paused_at
                        .map_or_else(elapsed, |at| at.duration_since(started).as_micros() as u64)
                        .saturating_sub(paused_for_us);
                    let mut progress = state.real_progress.get();
                    let elapsed = progress.observe(
                        wall_us,
                        state.allowance.get(),
                        epoch_us.saturating_add(state.active_skip_us.get()),
                    );
                    state.real_progress.set(progress);
                    elapsed
                }
                ClockMode::Virtual(t) => t,
            }
    }

    /// Account the host's planned idle interval. An indefinite object/input
    /// wait permits elapsed wall time; no simulation was runnable in that time.
    pub fn begin_wait(&mut self, deadline: Option<u64>) {
        self.now_us();
        self.state
            .allowance
            .set(deadline.map_or(Allowance::UnboundedWait, Allowance::Until));
    }

    /// Settle the prior wait before running contexts again. An early input wake
    /// consumes only actual elapsed time and leaves no future waiting credit.
    pub fn begin_work(&mut self) {
        self.now_us();
        self.state.allowance.set(Allowance::Working);
    }

    pub fn passive_stalls(&self) -> (u64, u64) {
        let progress = self.state.real_progress.get();
        (progress.clamps, progress.discarded_us)
    }

    pub fn now_ms(&self) -> u32 {
        (self.now_us() / 1000) as u32
    }

    pub fn qpc(&self) -> u64 {
        self.now_us() * (QPC_FREQ / 1_000_000)
    }

    /// A reading the guest can observe (timeGetTime, QPC, ...). In virtual mode every
    /// reading advances time by 1 µs, so busy-waits on the clock terminate.
    pub fn guest_now_us(&mut self) -> u64 {
        if let ClockMode::Virtual(t) = self.state.mode.get() {
            self.state.mode.set(ClockMode::Virtual(t + 1));
        }
        self.now_us()
    }

    /// The guest's UTC time as an NT system time (100 ns units since 1601).
    /// Virtual runs start at a fixed date; real runs read the host's wall
    /// clock.
    pub fn system_time(&mut self) -> u64 {
        let unix_us = if self.is_virtual() {
            VIRTUAL_START_UNIX * 1_000_000 + self.guest_now_us() - self.epoch_us
        } else {
            let _ = self.guest_now_us();
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as u64)
                .unwrap_or(0)
        };
        UNIX_EPOCH_SYSTEM_TIME + unix_us * 10
    }

    /// Virtual mode: move time forward to `us` (never backwards). Real mode: no-op.
    pub fn advance_to(&mut self, us: u64) {
        if let ClockMode::Virtual(t) = self.state.mode.get() {
            self.state
                .mode
                .set(ClockMode::Virtual(t.max(us.saturating_sub(self.epoch_us))));
        }
    }

    /// Explicit user-requested time skipping. The scheduler must first prove
    /// there are no runnable contexts and select an eligible timed deadline.
    /// This never changes a device clock or compensates a passive host stall.
    pub(crate) fn skip_to(&mut self, deadline: u64) -> u64 {
        if matches!(
            self.state.mode.get(),
            ClockMode::Real {
                paused_at: Some(_),
                ..
            }
        ) {
            return 0;
        }
        let delta = deadline.saturating_sub(self.now_us());
        match self.state.mode.get() {
            ClockMode::Virtual(time) => self
                .state
                .mode
                .set(ClockMode::Virtual(time.saturating_add(delta))),
            ClockMode::Real { .. } => self
                .state
                .active_skip_us
                .set(self.state.active_skip_us.get().saturating_add(delta)),
        }
        delta
    }

    /// Freeze guest-visible real time while the frontend is backgrounded.
    pub fn pause(&mut self) {
        self.now_us();
        let mut mode = self.state.mode.get();
        if let ClockMode::Real { paused_at, .. } = &mut mode {
            paused_at.get_or_insert_with(Instant::now);
        }
        self.state.mode.set(mode);
    }

    /// Continue real time without counting the interval spent in the background.
    pub fn resume(&mut self) {
        let mut mode = self.state.mode.get();
        if let ClockMode::Real {
            paused_at,
            paused_for_us,
            ..
        } = &mut mode
            && let Some(at) = paused_at.take()
        {
            *paused_for_us += at.elapsed().as_micros() as u64;
        }
        self.state.mode.set(mode);
    }
}

/// Back-to-back reads which prove a context is only polling the clock. Bounded
/// per-item loops read the clock back to back too: Siglus stamps each of 256
/// keys, twice a frame, and a 64-read streak stretched its frames to 200 ms.
pub const POLL_STREAK: u32 = 2048;
/// Longest gap between two reads of one poll: a loop doing real work between
/// reads is not a busy-wait, however often it asks for the time.
const POLL_GAP_US: u64 = 50;

/// The last clock read and how many reads on that context came back to back,
/// with no other host call and almost no guest time between them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClockPoll {
    context: Option<CtxId>,
    call: u64,
    read_us: u64,
    streak: u32,
}

impl ClockPoll {
    fn next_streak(&self, context: CtxId, call: u64, now: u64) -> u32 {
        if self.context == Some(context)
            && self.call + 1 == call
            && now.saturating_sub(self.read_us) <= POLL_GAP_US
        {
            self.streak.saturating_add(1)
        } else {
            1
        }
    }

    fn record(&mut self, context: CtxId, call: u64, now: u64) {
        self.streak = self.next_streak(context, call, now);
        (self.context, self.call, self.read_us) = (Some(context), call, now);
    }
}

/// Read the guest clock for a polling API. A context that does nothing but reread
/// the clock is busy-waiting; once a streak proves it, each read waits for the
/// next millisecond, as a preempted thread would observe, so the spin idles the
/// context and time skipping can reach the time it waits for.
pub fn poll_clock(
    env: &mut Env<'_>,
    finish: impl FnOnce(&mut Env<'_>, u64) -> Outcome + 'static,
) -> Outcome {
    let now = env.sys.clock.guest_now_us();
    let (context, call) = (env.sys.current, env.sys.host_calls);
    let poll = env.ext.get_or_default::<ClockPoll>();
    poll.record(context, call, now);
    if poll.streak < POLL_STREAK {
        return finish(env, now);
    }
    let next_ms = (now / 1000 + 1) * 1000;
    Outcome::block_timed(Wait::Until(next_ms), move |env, _| {
        let now = env.sys.clock.guest_now_us();
        let call = env.sys.host_calls;
        let poll = env.ext.get_or_default::<ClockPoll>();
        (poll.call, poll.read_us) = (call, now);
        finish(env, now)
    })
}

#[cfg(test)]
mod tests {
    use super::{Allowance, Clock, MAX_PASSIVE_STEP_US, RealProgress};

    // spec: NAT-BODY-2
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn counter_scaling_preserves_microseconds_without_floating_point() {
        for frequency in [32768, 1_000_000, 19_200_000, 24_000_000, 1_000_000_000] {
            let clock = super::CounterClock::scaled(0, frequency);
            for ticks in [0, 1, frequency - 1, frequency, frequency + 1, 1 << 48] {
                let reference = u128::from(ticks) * 1_000_000 / u128::from(frequency);
                let got = u128::from(clock.micros(ticks));
                assert!((reference..=reference + 1).contains(&got));
            }
        }
    }

    #[test]
    fn passive_stalls_are_discarded_without_future_catch_up_or_clock_rewind() {
        let mut progress = RealProgress::default();
        assert_eq!(progress.observe(5000, Allowance::Working, 0), 5000);
        assert_eq!(
            progress.observe(2_005_000, Allowance::Working, 0),
            5000 + MAX_PASSIVE_STEP_US
        );
        assert_eq!(
            progress.observe(2_005_001, Allowance::Working, 0),
            5001 + MAX_PASSIVE_STEP_US
        );
        assert_eq!(
            progress.observe(2_000_000, Allowance::Working, 0),
            5001 + MAX_PASSIVE_STEP_US
        );
        assert_eq!(progress.clamps, 1);
        assert_eq!(progress.discarded_us, 2_000_000 - MAX_PASSIVE_STEP_US);
    }

    #[test]
    fn intentional_long_waits_are_not_slowed_but_late_wakeup_is_bounded() {
        let mut exact = RealProgress::default();
        assert_eq!(
            exact.observe(1_000_000, Allowance::Until(2_000_000), 1_000_000),
            1_000_000
        );
        assert_eq!(exact.clamps, 0);
        let mut late = RealProgress::default();
        assert_eq!(
            late.observe(4_000_000, Allowance::Until(2_000_000), 1_000_000),
            1_000_000 + MAX_PASSIVE_STEP_US
        );
        assert_eq!(
            late.observe(4_000_001, Allowance::Working, 1_000_000),
            1_000_001 + MAX_PASSIVE_STEP_US
        );
        // Early input consumes actual elapsed time and cannot lend its unused
        // idle allowance to a later stall while contexts are runnable.
        let mut early = RealProgress::default();
        assert_eq!(
            early.observe(10_000, Allowance::Until(1_000_000), 0),
            10_000
        );
        assert_eq!(
            early.observe(800_000, Allowance::Working, 0),
            10_000 + MAX_PASSIVE_STEP_US
        );
    }

    #[test]
    fn active_skip_and_background_pause_are_separate_from_passive_stalls() {
        let mut clock = Clock::real();
        let start = clock.now_us();
        assert!(clock.skip_to(start + 10_000_000) > 9_000_000);
        assert!(clock.now_us() >= start + 10_000_000);
        assert_eq!(clock.passive_stalls(), (0, 0));
        clock.pause();
        let paused = clock.now_us();
        assert_eq!(clock.skip_to(paused + 10_000_000), 0);
        assert_eq!(clock.now_us(), paused);
        clock.resume();
        assert!(clock.now_us() >= paused);
        let mut indefinite = RealProgress::default();
        assert_eq!(
            indefinite.observe(3_600_000_000, Allowance::UnboundedWait, 0),
            3_600_000_000
        );
    }

    #[test]
    fn paused_real_clock_does_not_advance() {
        let mut clock = Clock::real();
        clock.pause();
        let stopped = clock.now_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert_eq!(clock.now_us(), stopped);
        clock.resume();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(clock.now_us() > stopped);
    }
}
