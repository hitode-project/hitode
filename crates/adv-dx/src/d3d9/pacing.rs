//! Guest-visible refresh pacing uses only the Machine clock and wait mechanism.
use super::*;

/// The next refresh boundary after `now`. With a host vsync grid whose rate
/// is a whole multiple of `hz`, boundaries land on vsyncs one guest period
/// after `previous`, so the guest's frames never beat against the display.
pub(super) fn deadline(
    now: u64,
    interval: u32,
    hz: u32,
    occluded: bool,
    vsync: Option<(std::time::Duration, std::time::Duration)>,
    previous: Option<u64>,
) -> Result<Option<u64>, ()> {
    if interval == 0x80000000 || occluded {
        return Ok(None);
    }
    if !matches!(interval, 0 | 1) || hz == 0 {
        return Err(());
    }
    if let Some(boundary) =
        vsync.and_then(|(since, period)| locked(now, hz, since, period, previous))
    {
        return Ok(Some(boundary));
    }
    // Rational boundaries avoid the accumulated error of repeatedly adding a
    // rounded microsecond period. A late present advances to the next boundary.
    let frame = u128::from(now) * u128::from(hz) / 1_000_000 + 1;
    let boundary = (frame * 1_000_000).div_ceil(u128::from(hz));
    u64::try_from(boundary).map(Some).map_err(|_| ())
}
fn locked(
    now: u64,
    hz: u32,
    since: std::time::Duration,
    period: std::time::Duration,
    previous: Option<u64>,
) -> Option<u64> {
    let period = period.as_secs_f64() * 1e6;
    let rate = 1e6 / period;
    let vsyncs = (rate / f64::from(hz)).round();
    if vsyncs < 1.0 {
        return None;
    }
    let now_f = now as f64;
    let origin = now_f - since.as_secs_f64() * 1e6;
    let after_now = origin + ((now_f - origin) / period).floor() * period + period;
    if (rate - vsyncs * f64::from(hz)).abs() > 0.01 * rate {
        // Keep the rational guest cadence, selecting its nearest display
        // opportunity instead of rounding the refresh ratio for every frame.
        // Advance past the previous boundary's rational slot so an early-snapped
        // vsync never schedules two Presents in the same guest slot.
        let frame_from_now = u128::from(now) * u128::from(hz) / 1_000_000 + 1;
        let frame = match previous {
            Some(last) if last <= now => {
                let last_frame = (u128::from(last) * u128::from(hz) + 500_000) / 1_000_000;
                frame_from_now.max(last_frame + 1)
            }
            _ => frame_from_now,
        };
        let target = (frame * 1_000_000).div_ceil(u128::from(hz)) as f64;
        let snapped = origin + ((target - origin) / period + 0.5).floor() * period;
        let boundary = u64::try_from(snapped.max(after_now).round() as u128).ok()?;
        return Some(boundary.max(now.checked_add(1)?));
    }
    let guest = vsyncs * period;
    let boundary = match previous {
        // On time: one guest period after the last boundary, snapped to its vsync.
        Some(last) if last <= now && last as f64 + guest - period / 2.0 > now_f => {
            let target = last as f64 + guest;
            let snapped = origin + ((target - origin) / period + 0.5).floor() * period;
            if snapped > now_f { snapped } else { after_now }
        }
        // Late or first: the next vsync, never repaying missed frames.
        _ => after_now,
    };
    u64::try_from(boundary.ceil() as u128).ok()
}
pub(super) fn raster_status(env: &mut Env<'_>, output: GuestPtr<[u32; 2]>) -> u32 {
    let provider = env.ext.get::<State>().provider;
    let (_, height, hz) = (provider.display)(env.ext);
    let vsync = (provider.vsync)(env.ext);
    let (blank, line) = crate::pacing::scanline(height, hz, env.sys.clock.now_us(), vsync);
    output
        .write(env.cpu, [u32::from(blank), if blank { 0 } else { line }])
        .map_or(INVALID, |()| 0)
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
            // Display boundaries are not timer ticks.
            env.sys.keep_exact_deadline();
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
            let at = deadline(
                env.sys.clock.now_us(),
                mode.interval,
                60,
                mode.occluded,
                None,
                None,
            )
            .unwrap();
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
    // spec: GFX-PACE-1
    fn vsync_locked_boundaries_keep_the_display_phase() {
        use std::time::Duration;
        let us = Duration::from_micros;
        // 60 Hz grid with vsyncs at ..., 1_000_000 - 4_000, 1_012_667, ...
        let grid = Some((us(4_000), us(16_667)));
        assert_eq!(
            deadline(1_000_000, 1, 60, false, grid, None),
            Ok(Some(1_012_667))
        );
        // On time: one period after the last boundary, even if the guest is quick.
        assert_eq!(
            deadline(
                1_013_000,
                1,
                60,
                false,
                Some((us(333), us(16_667))),
                Some(1_012_667)
            ),
            Ok(Some(1_029_334))
        );
        // Late by more than half a vsync: the next vsync, no catching up.
        assert_eq!(
            deadline(
                1_040_000,
                1,
                60,
                false,
                Some((us(10_666), us(16_667))),
                Some(1_012_667)
            ),
            Ok(Some(1_046_001))
        );
        // 120 Hz keeps every second vsync after the last boundary.
        let fast = Some((us(1_000), us(8_333)));
        assert_eq!(
            deadline(1_001_000, 0, 60, false, fast, Some(1_000_000)),
            Ok(Some(1_016_666))
        );
        // 59.94 Hz follows the panel; 144 Hz selects a rational guest boundary on its grid.
        assert_eq!(
            deadline(
                1_000_000,
                1,
                60,
                false,
                Some((us(0), us(16_683))),
                Some(983_317)
            ),
            Ok(Some(1_000_000 + 16_683))
        );
        assert_eq!(
            deadline(1_000_000, 1, 60, false, Some((us(0), us(6_944))), None),
            Ok(Some(1_013_888))
        );
    }

    #[test]
    // spec: GFX-PACE-1
    fn noninteger_display_rates_keep_guest_cadence_without_phase_drift() {
        for host_hz in [60u64, 90, 120, 144, 165] {
            let period = std::time::Duration::from_nanos(1_000_000_000 / host_hz);
            let period_us = period.as_secs_f64() * 1_000_000.0;
            // Test zero/fast/moderate render times and shifted vsync phases so
            // early-snapped vsyncs never schedule two Presents for one 60 Hz slot.
            for phase_quarter in 0..4u64 {
                let offset_ns = period.as_nanos() as u64 * phase_quarter / 4;
                for render_us in [0u64, 500, 1_500, 4_000] {
                    let mut now = offset_ns / 1000;
                    let mut previous = None;
                    let mut last_slot: Option<u128> = None;
                    let mut first = 0;
                    let mut last = 0;
                    for frame in 0..=600u64 {
                        let now_ns = now * 1000;
                        let since_ns = (now_ns + period.as_nanos() as u64 - offset_ns)
                            % period.as_nanos() as u64;
                        let since = std::time::Duration::from_nanos(since_ns);
                        let boundary = deadline(now, 1, 60, false, Some((since, period)), previous)
                            .unwrap()
                            .unwrap();
                        assert!(
                            boundary > now,
                            "{host_hz} Hz phase={phase_quarter} render={render_us} frame={frame}: boundary {boundary} <= now {now}"
                        );
                        let grid_pos = (boundary as f64 * 1000.0 - offset_ns as f64)
                            / period.as_nanos() as f64;
                        let phase = grid_pos.rem_euclid(1.0);
                        assert!(
                            phase.min(1.0 - phase) < 0.002,
                            "{host_hz} Hz phase={phase_quarter} frame={frame}: off-grid boundary={boundary} phase={phase}"
                        );
                        if host_hz % 60 == 0 {
                            if let Some(prev) = previous {
                                let step = (boundary - prev) as f64;
                                assert!(
                                    (step - 1_000_000.0 / 60.0).abs() <= 2.0,
                                    "{host_hz} Hz whole multiple drifted: step={step}"
                                );
                            }
                        } else {
                            // Every on-time Present must occupy a strictly distinct 60 Hz slot.
                            let slot = (u128::from(boundary) * 60 + 500_000) / 1_000_000;
                            if let Some(prev_slot) = last_slot {
                                assert_eq!(
                                    slot,
                                    prev_slot + 1,
                                    "{host_hz} Hz phase={phase_quarter} render={render_us} frame={frame}: slot jumped {prev_slot} -> {slot} (prev={previous:?} boundary={boundary})"
                                );
                            }
                            last_slot = Some(slot);
                            assert!(
                                (boundary as f64 - slot as f64 * 1_000_000.0 / 60.0).abs()
                                    <= period_us / 2.0 + 2.0,
                                "{host_hz} Hz phase={phase_quarter} frame={frame}: boundary={boundary} slot={slot}"
                            );
                        }
                        if frame == 0 {
                            first = boundary;
                        }
                        now = boundary + render_us;
                        previous = Some(boundary);
                        last = boundary;
                    }
                    let elapsed = last - first;
                    assert!(
                        (elapsed as i64 - 10_000_000).abs() <= period_us as i64 + 1_000,
                        "{host_hz} Hz phase={phase_quarter} render={render_us}: 600 frame intervals took {elapsed} us"
                    );
                }
            }
        }
    }

    #[test]
    // spec: GFX-PACE-1
    fn noninteger_late_present_advances_without_repaying_missed_frames() {
        for host_hz in [90u64, 144, 165] {
            let period = std::time::Duration::from_nanos(1_000_000_000 / host_hz);
            let p_ns = period.as_nanos() as u64;
            let first = deadline(
                0,
                1,
                60,
                false,
                Some((std::time::Duration::ZERO, period)),
                None,
            )
            .unwrap()
            .unwrap();
            // Stall for ~50 ms (3 missed 60 Hz slots), then present.
            let stalled = first + 50_000;
            let since = std::time::Duration::from_nanos((stalled * 1000) % p_ns);
            let after_stall = deadline(stalled, 1, 60, false, Some((since, period)), Some(first))
                .unwrap()
                .unwrap();
            assert!(after_stall > stalled);
            assert!(
                after_stall - stalled <= (p_ns / 1000) * 3,
                "{host_hz} Hz: waited {} us after stall",
                after_stall - stalled
            );
            // Next on-time present immediately resumes 60 Hz cadence without repaying missed slots.
            let next_now = after_stall + 1_000;
            let next_since = std::time::Duration::from_nanos((next_now * 1000) % p_ns);
            let next = deadline(
                next_now,
                1,
                60,
                false,
                Some((next_since, period)),
                Some(after_stall),
            )
            .unwrap()
            .unwrap();
            let step = next - after_stall;
            assert!(
                step >= p_ns / 1000 - 1 && step <= (p_ns * 3).div_ceil(1000) + 1,
                "{host_hz} Hz: step={step} us"
            );
        }
    }

    #[test]
    fn refresh_boundaries_are_exact_and_never_repay_late_frames() {
        assert_eq!(deadline(0, 0, 60, false, None, None), Ok(Some(16_667)));
        assert_eq!(deadline(16_667, 1, 60, false, None, None), Ok(Some(33_334)));
        assert_eq!(deadline(33_334, 0, 60, false, None, None), Ok(Some(50_000)));
        assert_eq!(
            deadline(5_000_001, 1, 60, false, None, None),
            Ok(Some(5_016_667))
        );
        assert_eq!(deadline(0, 1, 0, false, None, None), Err(()));
        assert_eq!(deadline(u64::MAX, 1, 60, false, None, None), Err(()));
        assert_eq!(deadline(0, 0x80000000, 60, false, None, None), Ok(None));
        assert_eq!(deadline(0, 0, 60, true, None, None), Ok(None));
        assert_eq!(deadline(0, 1, 60, true, None, None), Ok(None));
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
