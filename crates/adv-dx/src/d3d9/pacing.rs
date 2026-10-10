//! Guest-visible refresh pacing uses only the Machine clock and wait mechanism.
use super::*;

pub(super) fn deadline(
    now: u64,
    interval: u32,
    hz: u32,
    occluded: bool,
) -> Result<Option<u64>, ()> {
    if interval == 0x80000000 || occluded {
        return Ok(None);
    }
    if !matches!(interval, 0 | 1) || hz == 0 {
        return Err(());
    }
    // Rational boundaries avoid the accumulated error of repeatedly adding a
    // rounded microsecond period. A late present advances to the next boundary.
    let frame = u128::from(now) * u128::from(hz) / 1_000_000 + 1;
    let boundary = (frame * 1_000_000).div_ceil(u128::from(hz));
    u64::try_from(boundary).map(Some).map_err(|_| ())
}
pub(super) fn finish(
    env: &mut Env<'_>,
    deadline: Option<u64>,
    result: u32,
    fast_forward: bool,
) -> Outcome {
    if fast_forward {
        // Present acceleration is independent of idle virtual-time jumping:
        // another runnable guest must not hold the rendering thread at 60 Hz.
        return Outcome::Return(result);
    }
    match deadline {
        Some(time) if time > env.sys.clock.now_us() => {
            // This remains the single Machine timed-wait path. GPU fence/object
            // waits retain their distinct, non-skippable completion semantics.
            Outcome::block_then_return(Wait::Until(time), result)
        }
        _ => Outcome::Return(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, Prot, asm::Asm},
    };
    use adv_machine::{Clock, Conv, RunResult};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct Mode {
        interval: u32,
        occluded: bool,
        held: Arc<AtomicBool>,
    }
    fn fixture(
        jit: bool,
        interval: u32,
        occluded: bool,
        held: bool,
        release: bool,
    ) -> Machine<JitCpu> {
        let mut registry = Registry::default();
        registry.add("pacing", "present", Conv::Stdcall { argc: 0 }, |env| {
            let mode = env.ext.get::<Mode>();
            let at = deadline(env.sys.clock.now_us(), mode.interval, 60, mode.occluded).unwrap();
            let held = mode.held.load(Ordering::Acquire);
            finish(env, at, 0, held)
        });
        registry.add("pacing", "release", Conv::Stdcall { argc: 0 }, |env| {
            env.ext.get::<Mode>().held.store(false, Ordering::Release);
            Outcome::Return(0)
        });
        registry.add("pacing", "done", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        let mut machine = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let held = Arc::new(AtomicBool::new(held));
        let source = held.clone();
        machine.set_time_skip_source(Some(Arc::new(move |_| source.load(Ordering::Acquire))));
        machine.ext.insert(Mode {
            interval,
            occluded,
            held,
        });
        let present = machine.resolve_import("pacing", "present").unwrap();
        let up = machine.resolve_import("pacing", "release").unwrap();
        let done = machine.resolve_import("pacing", "done").unwrap();
        let mut code = Asm::new(0x400000);
        code.call(present);
        if release {
            code.call(up).call(present);
        }
        code.call(done);
        machine.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        machine.cpu.write(0x400000, &code.bytes).unwrap();
        machine.spawn("pacing", 0x400000, &[], 0x10000).unwrap();
        machine
    }
    #[test]
    fn refresh_boundaries_are_exact_and_never_repay_late_frames() {
        assert_eq!(deadline(0, 0, 60, false), Ok(Some(16_667)));
        assert_eq!(deadline(16_667, 1, 60, false), Ok(Some(33_334)));
        assert_eq!(deadline(33_334, 0, 60, false), Ok(Some(50_000)));
        assert_eq!(deadline(5_000_001, 1, 60, false), Ok(Some(5_016_667)));
        assert_eq!(deadline(0, 1, 0, false), Err(()));
        assert_eq!(deadline(u64::MAX, 1, 60, false), Err(()));
        assert_eq!(deadline(0, 0x80000000, 60, false), Ok(None));
        assert_eq!(deadline(0, 0, 60, true), Ok(None));
        assert_eq!(deadline(0, 1, 60, true), Ok(None));
    }

    #[test]
    fn default_and_one_use_the_machine_deadline_and_held_control_source() {
        for jit in [false, true] {
            for interval in [0, 1] {
                let mut normal = fixture(jit, interval, false, false, false);
                assert!(matches!(
                    normal.run(),
                    RunResult::Idle {
                        next_deadline: Some(1_016_667)
                    }
                ));
                assert_eq!(normal.sys.clock.now_us(), 1_000_000);
                assert_eq!(normal.sys.scheduling.active_time_skips, 0);
                normal.sys.clock.advance_to(1_016_667);
                assert!(matches!(normal.run(), RunResult::Exited(0)));
                let mut skip = fixture(jit, interval, false, true, false);
                assert!(matches!(skip.run(), RunResult::Exited(0)));
                assert_eq!(skip.sys.scheduling.active_time_skips, 0);
                assert_eq!(skip.sys.clock.now_us(), 1_000_000);
                let mut released = fixture(jit, interval, false, true, true);
                assert!(matches!(
                    released.run(),
                    RunResult::Idle {
                        next_deadline: Some(1_016_667)
                    }
                ));
                assert_eq!(released.sys.scheduling.active_time_skips, 0);
                assert_eq!(
                    released.sys.clock.now_us(),
                    1_000_000,
                    "key release immediately restores pacing"
                );
            }
        }
    }

    #[test]
    fn held_control_present_returns_even_with_another_ready_guest() {
        for jit in [false, true] {
            for interval in [0, 1] {
                let mut machine = fixture(jit, interval, false, true, false);
                machine.cpu.write(0x400100, &[0xeb, 0xfe]).unwrap();
                machine
                    .spawn("always-ready", 0x400100, &[], 0x10000)
                    .unwrap();
                assert_eq!(machine.sys.contexts.len(), 2);
                assert!(matches!(machine.run(), RunResult::Exited(0)));
                assert_eq!(machine.sys.clock.now_us(), 1_000_000);
                assert_eq!(machine.sys.scheduling.active_time_skips, 0);
            }
        }
    }

    #[test]
    fn immediate_and_occluded_present_do_not_create_waits_or_time_skips() {
        for jit in [false, true] {
            for (interval, occluded) in [(0x80000000, false), (0, true), (1, true)] {
                let mut machine = fixture(jit, interval, occluded, true, false);
                assert!(matches!(machine.run(), RunResult::Exited(0)));
                assert_eq!(machine.sys.clock.now_us(), 1_000_000);
                assert_eq!(machine.sys.scheduling.active_time_skips, 0);
            }
        }
    }
}
