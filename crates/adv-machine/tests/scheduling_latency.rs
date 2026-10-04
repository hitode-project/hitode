//! Synthetic scheduling contracts. No game code, assets or wall-clock speed claims.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, SliceResult, Wait};
use adv_unicorn::UnicornCpu;
use std::time::{Duration, Instant};

const CODE: u32 = 0x0040_0000;
const WORKER: u32 = CODE + 0x200;
const DATA: u32 = 0x0041_0000;

#[derive(Default)]
struct Frames {
    times_us: Vec<u64>,
    target: usize,
    period_us: u64,
}

fn timer_machine(
    quantum: Option<Duration>,
    frames: usize,
    workers: usize,
    period_us: u64,
) -> Machine<UnicornCpu> {
    let mut registry = Registry::default();
    registry.add("test", "frame", Conv::Stdcall { argc: 0 }, |e| {
        let now = e.sys.clock.now_us();
        let frames = e.ext.get_mut::<Frames>();
        frames.times_us.push(now);
        if frames.times_us.len() == frames.target {
            return Outcome::ExitProcess(0);
        }
        Outcome::block_then_return(Wait::Until(now + frames.period_us), 0)
    });
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::real()).unwrap();
    m.set_scheduling_quantum(quantum).unwrap();
    m.set_execution_budget(Some(Duration::from_secs(10)));
    m.ext.insert(Frames {
        target: frames,
        period_us,
        ..Frames::default()
    });
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let frame = m.resolve_import("test", "frame").unwrap();
    let mut code = Asm::new(CODE);
    code.call(frame).jmp(CODE);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("frame", CODE, &[], 65536).unwrap();
    for worker in 0..workers {
        let at = WORKER + worker as u32 * 16;
        let mut code = Asm::new(at);
        code.bytes.extend_from_slice(&[0xff, 0x05]); // inc dword [DATA+4*worker]
        code.bytes
            .extend_from_slice(&(DATA + 4 * worker as u32).to_le_bytes());
        code.jmp(at);
        m.cpu.write(at, &code.bytes).unwrap();
        m.spawn("compute", at, &[], 65536).unwrap();
    }
    m
}

#[test]
fn timed_context_can_finish_inside_the_same_host_slice() {
    let mut m = timer_machine(Some(Duration::from_millis(1)), 3, 1, 2_000);
    // Broad timeout, not a shared-runner microsecond latency gate. The old policy
    // returns Yielded here with only one frame; it cannot re-enter the timer context.
    let out = m.run_slice(Duration::from_millis(500));
    assert!(
        matches!(out, SliceResult::Complete(RunResult::Exited(0))),
        "{out:?}"
    );
    assert_eq!(m.ext.get::<Frames>().times_us.len(), 3);
    assert!(
        m.ext
            .get::<Frames>()
            .times_us
            .windows(2)
            .all(|w| w[1] - w[0] >= 2_000)
    );
    assert!(m.sys.scheduling.internal_preemptions >= 2);
    assert!(m.cpu.read_u32(DATA).unwrap() > 0);
}

#[test]
fn opt_out_keeps_slice_only_policy() {
    let mut m = timer_machine(None, 3, 1, 2_000);
    assert!(matches!(
        m.run_slice(Duration::from_millis(10)),
        SliceResult::Yielded
    ));
    assert_eq!(m.ext.get::<Frames>().times_us.len(), 1);
    assert_eq!(m.sys.scheduling.internal_preemptions, 0);
}

#[test]
fn real_pollers_all_get_a_turn_before_each_host_idle() {
    let mut registry = Registry::default();
    registry.add("test", "yield", Conv::Stdcall { argc: 0 }, |e| {
        e.ext.get_mut::<[u32; 3]>()[e.context as usize] += 1;
        Outcome::block_then_return(Wait::Yield, 0)
    });
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::real()).unwrap();
    m.set_execution_budget(Some(Duration::from_secs(2)));
    m.ext.insert([0u32; 3]);
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let call = m.resolve_import("test", "yield").unwrap();
    let mut code = Asm::new(CODE);
    code.call(call).jmp(CODE);
    m.cpu.write(CODE, &code.bytes).unwrap();
    for _ in 0..3 {
        m.spawn("poller", CODE, &[], 65536).unwrap();
    }
    for round in 1..=5 {
        assert!(matches!(m.run(), RunResult::Idle { .. }));
        assert_eq!(
            *m.ext.get::<[u32; 3]>(),
            [round; 3],
            "host slept before completing round {round}"
        );
    }
}

#[test]
fn internal_reschedules_do_not_renew_the_hard_watchdog() {
    let mut m = timer_machine(Some(Duration::from_micros(500)), usize::MAX, 2, 1_000);
    // Windows can deliver a sub-millisecond watchdog wake after a coarse timer
    // interval. Leave room for actual internal dispatches inside a host slice;
    // this verifies budget accumulation, not a microsecond latency guarantee.
    m.set_execution_budget(Some(Duration::from_millis(250)));
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(3));
        match m.run_slice(Duration::from_millis(100)) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Failed(e)) => {
                assert!(e.message.contains("execution budget exhausted"));
                break;
            }
            other => panic!("unexpected result {other:?}"),
        }
    }
    assert!(m.sys.scheduling.internal_preemptions > 0);
}

#[test]
fn zero_quantum_is_rejected_without_mutating_execution() {
    let mut m = timer_machine(None, 3, 1, 2_000);
    assert!(m.set_scheduling_quantum(Some(Duration::ZERO)).is_err());
    assert!(matches!(
        m.run_slice(Duration::from_millis(10)),
        SliceResult::Yielded
    ));
    assert_eq!(m.sys.scheduling.internal_preemptions, 0);
}

/// Diagnostic comparison only. The timed "frame" is a synthetic callback and the
/// workers deliberately never block. This is not a game, pixel or audio benchmark.
#[test]
#[ignore = "diagnostic wall-clock benchmark; run explicitly with --ignored --nocapture"]
fn report_contended_frame_intervals() {
    for period_us in [1_000, 16_667] {
        for (run, quantum) in [
            None,
            Some(Duration::from_millis(1)),
            Some(Duration::from_millis(1)),
            None,
        ]
        .into_iter()
        .enumerate()
        {
            let mut m = timer_machine(quantum, 33, 2, period_us);
            let start = Instant::now();
            loop {
                match m.run_slice(Duration::from_millis(8)) {
                    SliceResult::Yielded => {}
                    SliceResult::Complete(RunResult::Exited(0)) => break,
                    other => panic!("synthetic renderer failed: {other:?}"),
                }
                assert!(start.elapsed() < Duration::from_secs(5));
            }
            let mut gaps: Vec<_> = m
                .ext
                .get::<Frames>()
                .times_us
                .windows(2)
                .map(|w| w[1] - w[0])
                .collect();
            assert_eq!(gaps.len(), 32);
            assert!(
                gaps.iter().all(|&gap| gap >= period_us),
                "guest Sleep was shortened"
            );
            gaps.sort_unstable();
            println!(
                "LATENCY run={run} period_us={period_us} policy={} frames=33 samples=32 p50_us={} p95_us={} max_us={} worker0={} worker1={} diagnostics={:?}",
                if quantum.is_some() {
                    "deadline"
                } else {
                    "slice-only"
                },
                gaps[15],
                gaps[30],
                gaps[31],
                m.cpu.read_u32(DATA).unwrap(),
                m.cpu.read_u32(DATA + 4).unwrap(),
                m.sys.scheduling
            );
        }
    }
}
