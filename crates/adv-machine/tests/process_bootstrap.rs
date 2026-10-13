//! Authored process initialization is independent of the requested initial EIP.
use adv_abi::Registry;
use adv_cpu::{
    ArchitecturalState, JitCpu,
    backend::{Backend, Prot, Reg, asm::Asm},
};
use adv_machine::{
    Clock, Conv, Env, InitCall, Machine, Outcome, RunResult, SliceResult, Wait, layout::PARK_ADDR,
};
use adv_unicorn::UnicornCpu;
use std::time::{Duration, Instant};

const CODE: u32 = 0x0040_0000;
const REDIRECT: u32 = CODE + 0x100;
const CALLBACK: u32 = CODE + 0x200;
const PEER: u32 = CODE + 0x300;
const DATA: u32 = 0x0041_0000;

#[derive(Default)]
struct Seen {
    begun: u32,
    init: u32,
    ready: u32,
    entry: u32,
    redirected: u32,
    deferred: u32,
    nested: u32,
    peer: u32,
    returned: Vec<u32>,
}

struct Requested {
    state: ArchitecturalState,
    redirected: bool,
}

fn init(target: u32, failure_exit: Option<u32>) -> InitCall {
    InitCall {
        target,
        args: vec![],
        what: "authored initializer".into(),
        failure_exit,
        begin: Some((|env, _| env.ext.get_mut::<Seen>().begun += 1, 0)),
    }
}

fn fixture<B: Backend>(cpu: B, registry: Registry, clock: Clock) -> Machine<B> {
    let mut machine = Machine::new(cpu, registry, clock).unwrap();
    machine.ext.insert(Seen::default());
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.set_execution_budget(Some(Duration::from_secs(3)));
    machine
}

fn emit<B: Backend>(machine: &mut Machine<B>, at: u32, body: impl FnOnce(&mut Asm)) {
    let mut code = Asm::new(at);
    body(&mut code);
    machine.cpu.write(at, &code.bytes).unwrap();
}

macro_rules! backends {
    ($test:ident $(, $argument:expr)*) => {{
        $test(UnicornCpu::new(PARK_ADDR).unwrap() $(, $argument)*);
        $test(JitCpu::new(PARK_ADDR, false).unwrap() $(, $argument)*);
        let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
        jit.set_async_compilation(false);
        $test(jit $(, $argument)*);
    }};
}

fn observe_requested(env: &mut Env<'_>) -> Outcome {
    let actual = env.cpu.capture_architecture().unwrap();
    let expected = env.ext.get::<Requested>();
    for index in [0, 1, 2, 3, 5, 6, 7] {
        assert_eq!(
            actual.regs[index], expected.state.regs[index],
            "GPR {index}"
        );
    }
    assert_eq!(
        actual.regs[4],
        expected.state.regs[4] - if expected.redirected { 4 } else { 8 },
        "requested stack and ordinary call frames"
    );
    assert_eq!(actual.eflags, expected.state.eflags);
    assert_eq!(actual.fs_base, expected.state.fs_base);
    assert_eq!(actual.selectors, expected.state.selectors);
    let mut initialized_x87 = expected.state.x87.clone();
    initialized_x87.control = 0x037f;
    assert_eq!(actual.x87, initialized_x87);
    let mut initialized_xmm = expected.state.xmm;
    initialized_xmm[0] = [0; 16];
    assert_eq!(actual.xmm, initialized_xmm);
    assert_eq!(actual.mxcsr, 0x1f80);
    let redirected = expected.redirected;
    let seen = env.ext.get_mut::<Seen>();
    assert_eq!((seen.begun, seen.init, seen.ready), (1, 1, 1));
    if redirected {
        seen.redirected += 1;
    } else {
        seen.entry += 1;
    }
    Outcome::Return(37)
}

fn startup<B: Backend>(cpu: B, redirected: bool) {
    let mut registry = Registry::default();
    registry.add("fixture", "init", Conv::Stdcall { argc: 0 }, |env| {
        env.ext.get_mut::<Seen>().init += 1;
        let mut state = env.cpu.capture_architecture().unwrap();
        state.regs[0] = 0xdead;
        state.regs[1] = 0xbeef;
        state.regs[2] = 0xcafe;
        state.eflags ^= 0x8d5;
        state.x87.control = 0x037f;
        state.mxcsr = 0x1f80;
        state.xmm[0] = [0; 16];
        env.cpu.restore_architecture(&state).unwrap();
        Outcome::Return(1)
    });
    registry.add("fixture", "ready", Conv::Stdcall { argc: 0 }, |env| {
        let seen = env.ext.get_mut::<Seen>();
        assert_eq!(seen.init, 1);
        seen.ready += 1;
        Outcome::Return(1)
    });
    registry.add(
        "fixture",
        "observe",
        Conv::Stdcall { argc: 0 },
        observe_requested,
    );
    let mut machine = fixture(cpu, registry, Clock::virtual_time());
    let initialize = machine.resolve_import("fixture", "init").unwrap();
    let ready = machine.resolve_import("fixture", "ready").unwrap();
    let observe = machine.resolve_import("fixture", "observe").unwrap();
    emit(&mut machine, CODE, |a| {
        a.call(observe).ret();
    });
    emit(&mut machine, REDIRECT, |a| {
        a.call(observe).ret();
    });
    machine.sys.process_init.push_back(init(initialize, None));
    machine.sys.process_ready.push(init(ready, None));
    // Only the initializer's observation counts as InitBegin in this assertion.
    machine.sys.process_ready[0].begin = None;
    let main = machine
        .start_main(CODE, CODE, 65536, "authored process")
        .unwrap();
    machine.sys.suspend_context(main);
    machine
        .visit_context(Some(main), |env| {
            // Materialization consumes start, but cannot consume process bootstrap.
            let mut state = env.cpu.capture_architecture().unwrap();
            if redirected {
                state.eip = CODE + 0x800;
            }
            state.regs[0] = 0x12345678;
            state.regs[1] = 0x23456789;
            state.regs[2] = 0x3456789a;
            state.regs[3] = 0x456789ab;
            state.regs[5] = 0x56789abc;
            state.regs[6] = 0x6789abcd;
            state.regs[7] = 0x789abcde;
            state.eflags = (state.eflags & !0x8d5) | 0x895;
            state.x87.control = 0x067f;
            state.mxcsr = 0x5f80;
            state.xmm[0] = [0x5a; 16];
            env.cpu.restore_architecture(&state).unwrap();
        })
        .unwrap();
    assert!(matches!(machine.run_virtual(1000), RunResult::Idle { .. }));
    machine
        .visit_context(Some(main), |env| {
            assert_eq!(env.ext.get::<Seen>().begun, 0);
            assert_eq!(env.ext.get::<Seen>().init, 0);
            assert_eq!(env.ext.get::<Seen>().ready, 0);
            if redirected {
                env.cpu.set_reg(Reg::Eip, REDIRECT);
            }
            let state = env.cpu.capture_architecture().unwrap();
            env.ext.insert(Requested { state, redirected });
        })
        .unwrap();
    machine.sys.resume_context(main);
    let result = machine.run_virtual(100_000);
    assert!(matches!(result, RunResult::Exited(37)), "{result:?}");
    let seen = machine.ext.get::<Seen>();
    assert_eq!((seen.begun, seen.init, seen.ready), (1, 1, 1));
    assert_eq!(
        (seen.entry, seen.redirected),
        if redirected { (0, 1) } else { (1, 0) }
    );
}

// spec: PROC-CREATE-4
#[test]
fn suspended_visits_and_latest_redirect_preserve_startup_and_architecture() {
    backends!(startup, true);
}

#[test]
fn ordinary_entry_runs_after_initializers_and_readiness_once() {
    backends!(startup, false);
}

fn drive<B: Backend>(machine: &mut Machine<B>) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < deadline, "startup failed to progress");
        match machine.run_slice(Duration::from_millis(4)) {
            SliceResult::Complete(RunResult::Exited(code)) => return code,
            SliceResult::Complete(RunResult::Failed(error)) => panic!("{error:?}"),
            SliceResult::Complete(RunResult::HostRequest) => panic!("unexpected service request"),
            SliceResult::Complete(RunResult::Idle { .. }) => std::thread::yield_now(),
            SliceResult::Yielded => {}
        }
    }
}

fn continued<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    registry.add("fixture", "init", Conv::Stdcall { argc: 0 }, |env| {
        env.ext.get_mut::<Seen>().init += 1;
        Outcome::Defer {
            then: Box::new(|env| {
                env.ext.get_mut::<Seen>().deferred += 1;
                Outcome::call(CALLBACK, [], |env, result| {
                    assert_eq!(result.value(), 9);
                    env.ext.get_mut::<Seen>().nested += 1;
                    Outcome::Return(1)
                })
            }),
        }
    });
    registry.add("fixture", "yield", Conv::Stdcall { argc: 0 }, |env| {
        env.sys.complete_frame();
        Outcome::block_then_return(Wait::Yield, 9)
    });
    registry.add("fixture", "peer", Conv::Stdcall { argc: 0 }, |env| {
        env.ext.get_mut::<Seen>().peer += 1;
        Outcome::block(Wait::Until(u64::MAX), |_, _| {
            panic!("peer must end with the process")
        })
    });
    registry.add("fixture", "entry", Conv::Stdcall { argc: 0 }, |env| {
        let seen = env.ext.get_mut::<Seen>();
        assert_eq!(
            (seen.init, seen.deferred, seen.nested, seen.peer),
            (1, 1, 1, 1)
        );
        seen.entry += 1;
        Outcome::Return(37)
    });
    let mut machine = fixture(cpu, registry, Clock::real());
    let initialize = machine.resolve_import("fixture", "init").unwrap();
    let yield_ = machine.resolve_import("fixture", "yield").unwrap();
    let entry = machine.resolve_import("fixture", "entry").unwrap();
    let peer = machine.resolve_import("fixture", "peer").unwrap();
    emit(&mut machine, CALLBACK, |a| {
        a.call(yield_).ret();
    });
    emit(&mut machine, CODE, |a| {
        a.call(entry).ret();
    });
    emit(&mut machine, PEER, |a| {
        a.call(peer).ret();
    });
    machine.sys.process_init.push_back(init(initialize, None));
    machine.sys.process_return = Some(|env, value| {
        env.ext.get_mut::<Seen>().returned.push(value);
        Outcome::ExitProcess(value)
    });
    machine
        .start_main(CODE, CODE, 65536, "authored process")
        .unwrap();
    let peer = machine.spawn("worker", PEER, &[], 65536).unwrap();
    assert_eq!(drive(&mut machine), 37);
    assert_eq!(machine.ext.get::<Seen>().returned, [37]);
    assert_eq!(machine.ext.get::<Seen>().begun, 1);
    assert_eq!(machine.ext.get::<Seen>().entry, 1);
    assert_eq!(machine.sys.contexts[peer as usize].exit_code(), Some(37));
    assert_eq!(machine.sys.completed_frames, 1);
}

#[test]
fn bootstrap_defers_and_nested_callback_yields_without_replay_or_losing_process_return() {
    backends!(continued);
}

fn failed<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    registry.add("fixture", "fail", Conv::Stdcall { argc: 0 }, |env| {
        env.ext.get_mut::<Seen>().init += 1;
        Outcome::Return(0)
    });
    let mut machine = fixture(cpu, registry, Clock::virtual_time());
    let failure = machine.resolve_import("fixture", "fail").unwrap();
    machine
        .sys
        .process_init
        .push_back(init(failure, Some(0xc0000142)));
    // Unmapped targets prove neither readiness nor requested/application code runs.
    machine.sys.process_ready.push(init(0x0090_0000, None));
    machine.sys.process_return = Some(|env, value| {
        env.ext.get_mut::<Seen>().returned.push(value);
        Outcome::ExitProcess(value)
    });
    let main = machine
        .start_main(CODE, 0x0090_0000, 65536, "failing process")
        .unwrap();
    machine
        .visit_context(Some(main), |env| env.cpu.set_reg(Reg::Eip, 0x0091_0000))
        .unwrap();
    let result = machine.run_virtual(100_000);
    assert!(
        matches!(result, RunResult::Exited(0xc0000142)),
        "{result:?}"
    );
    let seen = machine.ext.get::<Seen>();
    assert_eq!(
        (
            seen.begun,
            seen.init,
            seen.ready,
            seen.entry,
            seen.redirected
        ),
        (1, 1, 0, 0, 0)
    );
    assert_eq!(seen.returned, [0xc0000142]);
}

#[test]
fn initializer_failure_terminates_without_resuming_requested_or_application_code() {
    backends!(failed);
}
