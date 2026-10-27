//! Productive polling (PROC-CLOCK-5): a Sleep(0) loop doing new work between
//! yields runs on without host idles; once it only repeats itself, it idles.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, Wait};
use adv_unicorn::UnicornCpu;
use std::time::Duration;

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0050_0000;
/// Distinct work steps, each a different host call between two yields.
const STEPS: usize = 32;

/// Step addresses in order, then the steps taken.
struct Steps(Vec<u32>, usize);

fn machine() -> Machine<UnicornCpu> {
    let mut registry = Registry::default();
    registry.add("test", "poll", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::block_then_return(Wait::Yield, 0)
    });
    // Each step points the loop at the next one; after the last, at `idle`.
    for step in 0..STEPS {
        registry.add(
            "test",
            &format!("step{step}"),
            Conv::Stdcall { argc: 0 },
            |e| {
                let steps = e.ext.get_mut::<Steps>();
                steps.1 += 1;
                let next = steps.0.get(steps.1).copied().unwrap_or(steps.0[STEPS]);
                e.cpu.write_u32(DATA, next).unwrap();
                Outcome::Return(0)
            },
        );
    }
    // This unclassified call deliberately exercises the general 64-repeat proof.
    registry.add("test", "idle", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::Return(0)
    });
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::real()).unwrap();
    m.set_execution_budget(Some(Duration::from_secs(2)));
    let mut steps: Vec<u32> = (0..STEPS)
        .map(|step| m.resolve_import("test", &format!("step{step}")).unwrap())
        .collect();
    steps.push(m.resolve_import("test", "idle").unwrap());
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write_u32(DATA, steps[0]).unwrap();
    m.ext.insert(Steps(steps, 0));
    let poll = m.resolve_import("test", "poll").unwrap();
    let mut code = Asm::new(CODE);
    // call [DATA]; call poll; jmp CODE
    code.bytes.extend_from_slice(&[0xff, 0x15]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.call(poll).jmp(CODE);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("main", CODE, &[], 65536).unwrap();
    m
}

// spec: PROC-CLOCK-5, PROC-CLOCK-7
#[test]
fn new_work_between_yields_runs_before_the_first_host_idle() {
    let mut m = machine();
    assert!(matches!(m.run(), RunResult::Idle { .. }));
    assert_eq!(
        m.ext.get::<Steps>().1,
        STEPS,
        "the host idled during new work"
    );
    let before = m.sys.scheduling.polling_rounds;
    assert!(matches!(m.run(), RunResult::Idle { .. }));
    assert_eq!(
        m.sys.scheduling.polling_rounds,
        before + 1,
        "repeats idle each round"
    );
}

struct Quiet {
    polls: u32,
    pending: std::sync::Arc<std::sync::atomic::AtomicU32>,
    signal_at: u32,
}

fn quiet_machine() -> Machine<adv_cpu::Cpu> {
    let mut registry = Registry::default();
    registry.add("test", "PeekMessage", Conv::Stdcall { argc: 0 }, |env| {
        let quiet = env.ext.get_mut::<Quiet>();
        quiet.polls += 1;
        let pending = quiet.pending.swap(0, std::sync::atomic::Ordering::AcqRel);
        if pending != 0 {
            return Outcome::ExitProcess(pending);
        }
        env.sys.poll_observation();
        Outcome::Return(0)
    });
    registry.add("test", "timeGetTime", Conv::Stdcall { argc: 0 }, |env| {
        if env.ext.get::<Quiet>().polls == env.ext.get::<Quiet>().signal_at {
            env.sys.poll_wake.as_ref().unwrap().signal();
        }
        env.sys.poll_observation();
        Outcome::Return((env.sys.clock.now_us() / 1000) as u32)
    });
    registry.add("test", "yield", Conv::Stdcall { argc: 0 }, |env| {
        env.sys.poll_observation();
        Outcome::block_then_return(Wait::Yield, 0)
    });
    let mut machine = Machine::new(
        adv_cpu::Cpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::real(),
    )
    .unwrap();
    machine.set_execution_budget(Some(Duration::from_secs(2)));
    machine.sys.poll_wake = Some(adv_sync::WakeEvent::new().unwrap());
    machine.ext.insert(Quiet {
        polls: 0,
        pending: Default::default(),
        signal_at: 0,
    });
    let mut code = Asm::new(CODE);
    for name in ["PeekMessage", "timeGetTime", "yield"] {
        code.call(machine.resolve_import("test", name).unwrap());
    }
    code.jmp(CODE);
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("poller", CODE, &[], 65536).unwrap();
    machine
}

// spec: PROC-CLOCK-7, PROC-CLOCK-6
#[test]
fn empty_peeks_and_clock_reads_idle_before_sixty_four_repeats() {
    let mut machine = quiet_machine();
    let before = machine.sys.clock.now_us();
    let RunResult::Idle {
        next_deadline: Some(deadline),
    } = machine.run()
    else {
        panic!("poller did not idle");
    };
    assert!(machine.ext.get::<Quiet>().polls < 10);
    assert!(deadline > before);
    assert_eq!(deadline % 1000, 0);
    assert!(deadline <= machine.sys.clock.now_us() + 1000);
}

// spec: PROC-CLOCK-7, PROC-MACH-2
#[test]
fn input_messages_events_audio_io_and_apcs_interrupt_a_quiet_round() {
    // All asynchronous execution deliveries share the retained wake. Events also
    // invalidate the machine's object generation, including before any waiter exists.
    for delivery in 1..=7 {
        let mut machine = quiet_machine();
        assert!(matches!(machine.run(), RunResult::Idle { .. }));
        let wake = machine.sys.poll_wake.clone().unwrap();
        let generation = wake.generation();
        let event = machine.sys.objects.insert(adv_machine::Object::Event {
            manual_reset: true,
            signaled: false,
        });
        let pending = machine.ext.get::<Quiet>().pending.clone();
        if delivery == 3 {
            assert!(machine.sys.set_event(event));
        }
        // Input, posted/sent messages, server signals, audio/video, I/O and APC
        // producers publish their queue data before signaling the same event.
        pending.store(delivery, std::sync::atomic::Ordering::Release);
        wake.signal();
        assert_ne!(wake.generation(), generation);
        assert!(
            wake.wait(Some(Duration::ZERO)).unwrap(),
            "delivery {delivery} was lost before wait"
        );
        let polls = machine.ext.get::<Quiet>().polls;
        assert!(matches!(machine.run(), RunResult::Exited(code) if code == delivery));
        assert_eq!(machine.ext.get::<Quiet>().polls, polls + 1);
    }
}

// spec: PROC-CLOCK-7
#[test]
fn a_delivery_during_the_round_requires_a_new_observation_round() {
    let mut machine = quiet_machine();
    machine.ext.get_mut::<Quiet>().signal_at = 2;
    assert!(matches!(machine.run(), RunResult::Idle { .. }));
    assert_eq!(machine.sys.poll_wake.as_ref().unwrap().generation(), 1);
    assert!((4..10).contains(&machine.ext.get::<Quiet>().polls));
}

// spec: PROC-CLOCK-5, PROC-CLOCK-7
#[test]
fn new_guest_work_between_observations_runs_before_the_first_idle() {
    let mut machine = quiet_machine();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let mut code = Asm::new(CODE);
    for step in 0..STEPS {
        code.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [DATA]
        code.bytes.extend_from_slice(&DATA.to_le_bytes());
        code.bytes.extend(std::iter::repeat_n(0x90, step));
        for name in ["PeekMessage", "timeGetTime", "yield"] {
            code.call(machine.resolve_import("test", name).unwrap());
        }
    }
    let idle = CODE + code.bytes.len() as u32;
    for name in ["PeekMessage", "timeGetTime", "yield"] {
        code.call(machine.resolve_import("test", name).unwrap());
    }
    code.jmp(idle);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    assert!(matches!(machine.run(), RunResult::Idle { .. }));
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), STEPS as u32);
}

// spec: PROC-CLOCK-5, PROC-CLOCK-7
#[test]
fn ctrl_skip_retains_the_full_proof_for_bounded_iteration_work() {
    let mut machine = quiet_machine();
    machine.set_time_skip_source(Some(std::sync::Arc::new(|_| true)));
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [DATA]
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    for name in ["PeekMessage", "timeGetTime", "yield"] {
        code.call(machine.resolve_import("test", name).unwrap());
    }
    code.cmp_mem_imm8(DATA, STEPS as i8).jnz(CODE);
    let idle = CODE + code.bytes.len() as u32;
    for name in ["PeekMessage", "timeGetTime", "yield"] {
        code.call(machine.resolve_import("test", name).unwrap());
    }
    code.jmp(idle);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    assert!(matches!(machine.run(), RunResult::Idle { .. }));
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), STEPS as u32);
    assert!(machine.ext.get::<Quiet>().polls >= 64);
}
