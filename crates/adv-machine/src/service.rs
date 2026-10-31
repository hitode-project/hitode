//! Shared per-thread observations. Bodies record work; the machine schedules it.
use std::cell::Cell;

#[derive(Default)]
pub struct Thread {
    empty_peeks: Cell<u32>,
    pub(crate) new_work: Cell<bool>,
}

/// Windows 11 empty A/W peeks, including PM_NOYIELD, hand off about once per
/// 100 calls on one CPU. Return a request; only the execution owner yields.
/// # Safety
/// `services.thread_state` must name the current context's live shared Thread.
pub unsafe fn empty_peek(services: adv_cpu::body::Services) -> bool {
    let thread = unsafe { &*services.thread_state.cast::<Thread>() };
    let count = thread.empty_peeks.get() + 1;
    thread.empty_peeks.set(if count == 100 { 0 } else { count });
    count == 100
}

/// A completed non-poll service invalidates clock/quiet-poll admission.
/// # Safety
/// Services pointers must be live machine-owned shared storage, or null.
pub unsafe fn work(services: adv_cpu::body::Services) {
    if !services.clock.is_null() {
        unsafe { crate::Clock::body_work(services.clock) };
    }
    if !services.thread_state.is_null() {
        unsafe { &*services.thread_state.cast::<Thread>() }
            .new_work
            .set(true);
    }
}

adv_cpu::body!(pub TIME_GET_TIME = time, inputs: &[], outputs: adv_cpu::body::EAX, pop: 0);

fn time<M: adv_cpu::body::Mem>(
    mem: &mut M,
    _: [u32; 7],
) -> Result<adv_cpu::body::Ret, adv_cpu::body::Stop> {
    let services = mem.services();
    // SAFETY: the owner retains the shared clock throughout execution.
    let now =
        unsafe { crate::Clock::body_poll(services.clock, services.epoch_us, services.context) }
            .ok_or(adv_cpu::body::Stop::Guest)?;
    Ok(adv_cpu::body::Ret::from((now / 1000) as u32))
}

/// The full timeGetTime handler shares the body and schedules required waits.
pub fn time_get_time(env: &mut crate::Env<'_>) -> crate::Outcome {
    env.sys.poll_observation();
    let services = env.sys.services(env.ext);
    let mut scratch = adv_cpu::body::Scratch::default();
    let result = (TIME_GET_TIME.full)(
        &mut adv_cpu::body::GuestMem::for_body(env.cpu, &mut scratch, services),
        [0; 7],
    );
    match result {
        Ok(ret) => crate::Outcome::Return(ret.value as u32),
        Err(adv_cpu::body::Stop::Guest) => {
            crate::clock::poll_clock(env, |_, us| crate::Outcome::Return((us / 1000) as u32))
        }
        Err(_) => unreachable!("timeGetTime accesses no guest memory"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: NAT-BODY-18
    #[test]
    fn empty_peek_handoffs_follow_the_measured_per_thread_windows_cadence() {
        let threads = [Thread::default(), Thread::default()];
        for round in 0..3 {
            for thread in &threads {
                let services = adv_cpu::body::Services {
                    thread_state: std::ptr::from_ref(thread).cast(),
                    ..Default::default()
                };
                for _ in 0..99 {
                    assert!(!unsafe { empty_peek(services) });
                }
                assert!(unsafe { empty_peek(services) }, "round {round}");
            }
        }
    }
}
