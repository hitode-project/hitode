//! User-directed time skipping never replaces scheduling or an external object's wait.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Object, Outcome, RunResult, Wait, layout::PARK_ADDR};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const CODE: u32 = 0x400000;
struct Control(Arc<AtomicBool>);
struct Event(u32);

fn fixture(
    jit: bool,
    real: bool,
    release: bool,
    object: bool,
    timed: bool,
) -> (Machine<JitCpu>, Arc<AtomicBool>) {
    let mut registry = Registry::default();
    registry.add("skip", "wait", Conv::Stdcall { argc: 0 }, move_wait);
    registry.add("skip", "release", Conv::Stdcall { argc: 0 }, |env| {
        env.ext.get::<Control>().0.store(false, Ordering::Release);
        Outcome::Return(0)
    });
    registry.add("skip", "done", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        if real {
            Clock::real()
        } else {
            Clock::virtual_time()
        },
    )
    .unwrap();
    let event = machine.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    machine.ext.insert(Event(event));
    machine.ext.insert((object, timed));
    let held = Arc::new(AtomicBool::new(true));
    let observe = held.clone();
    machine.set_time_skip_source(Some(Arc::new(move |_| observe.load(Ordering::Acquire))));
    machine.ext.insert(Control(held.clone()));
    machine.cpu.map(CODE, 4096, Prot::RWX).unwrap();
    let wait = machine.resolve_import("skip", "wait").unwrap();
    let up = machine.resolve_import("skip", "release").unwrap();
    let done = machine.resolve_import("skip", "done").unwrap();
    let mut code = Asm::new(CODE);
    code.call(wait);
    if release {
        code.call(up).call(wait);
    }
    code.call(done);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("skip-fixture", CODE, &[], 0x10000).unwrap();
    (machine, held)
}

fn move_wait(env: &mut adv_machine::Env<'_>) -> Outcome {
    let (object, timed) = *env.ext.get::<(bool, bool)>();
    let deadline = env.sys.clock.now_us() + 10_000_000;
    let wait = if object {
        Wait::Object {
            handle: env.ext.get::<Event>().0,
            deadline: Some(deadline),
        }
    } else {
        Wait::Until(deadline)
    };
    if timed {
        Outcome::block_timed(wait, |_, _| Outcome::Return(0))
    } else {
        Outcome::block_then_return(wait, 0)
    }
}

// spec: PROC-CLOCK-1
#[test]
fn held_input_skips_sleep_and_explicit_timer_waits_on_both_clocks() {
    for jit in [false, true] {
        for real in [false, true] {
            for object in [false, true] {
                let (mut machine, _) = fixture(jit, real, false, object, object);
                machine.sys.stats.wait_diagnostics =
                    Some(adv_machine::wait_diagnostics::WaitDiagnostics::default());
                assert!(matches!(machine.run(), RunResult::Exited(0)));
                assert_eq!(machine.sys.scheduling.active_time_skips, 1);
                assert!(machine.sys.scheduling.active_time_skip_us >= 9_000_000);
                let report = machine.sys.stats.wait_diagnostics.as_ref().unwrap().json();
                assert!(report.contains("\"skipped_segments\":1"));
                assert!(report.contains("\"guest_us\":{\"count\":1"));
                assert!(report.contains("\"dispatch_queue_us\":{\"count\":1"));
            }
        }
    }
}

#[test]
fn release_is_observed_before_the_next_deadline_without_a_guest_message() {
    for jit in [false, true] {
        let (mut machine, held) = fixture(jit, false, true, false, false);
        assert!(matches!(
            machine.run(),
            RunResult::Idle {
                next_deadline: Some(_)
            }
        ));
        assert!(!held.load(Ordering::Acquire));
        assert_eq!(machine.sys.scheduling.active_time_skips, 1);
        assert_eq!(machine.sys.clock.now_us(), 11_000_000);
    }
}

// spec: PROC-CLOCK-1
#[test]
fn synchronization_timeouts_and_paused_clocks_do_not_skip() {
    let (mut machine, _) = fixture(true, false, false, true, false);
    assert!(matches!(machine.run(), RunResult::Idle { .. }));
    assert_eq!(machine.sys.scheduling.active_time_skips, 0);
    let (mut paused, _) = fixture(true, true, false, false, false);
    paused.sys.clock.pause();
    let before = paused.sys.clock.now_us();
    assert!(matches!(paused.run(), RunResult::Idle { .. }));
    assert_eq!(paused.sys.clock.now_us(), before);
}

// spec: PROC-CLOCK-1
#[test]
fn runnable_signal_producer_runs_before_any_time_skip() {
    let (mut machine, _) = fixture(true, false, false, true, true);
    machine.sys.thunks.resolver_mut::<Registry>().unwrap().add(
        "skip",
        "signal",
        Conv::Stdcall { argc: 0 },
        |env| {
            env.sys.set_event(env.ext.get::<Event>().0);
            Outcome::ExitThread(0)
        },
    );
    let signal = machine.resolve_import("skip", "signal").unwrap();
    let mut worker = Asm::new(CODE + 0x100);
    worker.call(signal);
    machine.cpu.write(CODE + 0x100, &worker.bytes).unwrap();
    machine
        .spawn("producer", CODE + 0x100, &[], 0x10000)
        .unwrap();
    assert!(matches!(machine.run(), RunResult::Exited(0)));
    assert_eq!(machine.sys.clock.now_us(), 1_000_000);
    assert_eq!(machine.sys.scheduling.active_time_skips, 0);
}

#[test]
fn an_earlier_external_timeout_cannot_be_overtaken_by_a_later_sleep() {
    let (mut machine, _) = fixture(true, false, false, false, false);
    machine.sys.thunks.resolver_mut::<Registry>().unwrap().add(
        "skip",
        "external",
        Conv::Stdcall { argc: 0 },
        |env| {
            Outcome::block_then_return(
                Wait::Object {
                    handle: env.ext.get::<Event>().0,
                    deadline: Some(env.sys.clock.now_us() + 1_000),
                },
                0,
            )
        },
    );
    let wait = machine.resolve_import("skip", "external").unwrap();
    let mut worker = Asm::new(CODE + 0x100);
    worker.call(wait).ret();
    machine.cpu.write(CODE + 0x100, &worker.bytes).unwrap();
    machine
        .spawn("external-waiter", CODE + 0x100, &[], 0x10000)
        .unwrap();
    assert!(matches!(
        machine.run(),
        RunResult::Idle {
            next_deadline: Some(1_001_000)
        }
    ));
    assert_eq!(machine.sys.clock.now_us(), 1_000_000);
    assert_eq!(machine.sys.scheduling.active_time_skips, 0);
}

#[test]
fn the_policy_sees_startup_until_the_first_completed_frame() {
    for framed in [false, true] {
        let (mut machine, _) = fixture(true, false, false, false, true);
        machine.set_time_skip_source(Some(Arc::new(|startup| startup)));
        if framed {
            machine.sys.complete_frame();
        }
        let result = machine.run();
        if framed {
            assert!(matches!(result, RunResult::Idle { .. }));
            assert_eq!(machine.sys.scheduling.active_time_skips, 0);
        } else {
            assert!(matches!(result, RunResult::Exited(0)));
            assert_eq!(machine.sys.scheduling.active_time_skips, 1);
        }
    }
}
