#![cfg(feature = "pal")]
//! Complete trap/continuation and host-frame boundaries, using authored guest code.
use std::time::{Duration, Instant};
use pal_cpu::asm::Asm;

use pal_cpu::{Guest, GuestExt, Prot, Reg};
use pal_machine::layout::PARK_ADDR;
use pal_machine::{Clock, Conv, Machine, Outcome, Registry, RunResult, SliceResult, Wait};

type Cpu = adv_jit::pal::JitCpu;
fn new_cpu() -> Cpu { adv_jit::pal::JitCpu::new(PARK_ADDR, true).unwrap() }
fn prepare(m: &mut Machine<Cpu>) {
    // Exercise an actual native call into the trap adapter, not just interpretation.
    let mut region = adv_jit::Region::new(CODE, 512);
    region.reachable = true;
    region.entries = vec![CODE, PROC, PROC + 32];
    m.cpu.machine_mut().add_region(region).unwrap();
}

const CODE: u32 = 0x0040_0000;
const PROC: u32 = CODE + 0x100;
const DATA: u32 = 0x0041_0000;

fn fixture(registry: Registry, clock: Clock) -> Machine<Cpu> {
    let mut m = Machine::new(new_cpu(), registry, clock).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.set_execution_budget(Some(Duration::from_secs(3)));
    m
}
fn emit(m: &mut Machine<Cpu>, at: u32, f: impl FnOnce(&mut Asm)) {
    let mut a = Asm::new(at); f(&mut a); m.cpu.write(at, &a.bytes).unwrap();
}
fn step(m: &mut Machine<Cpu>) -> SliceResult { m.run_slice(Duration::from_millis(200)) }

#[test]
fn completed_frame_yields_without_losing_pc_registers_or_replaying_api() {
    let mut r = Registry::default();
    r.add("test", "present", Conv::Stdcall { argc: 1 }, |e| {
        assert_eq!(e.arg(0), 99);
        e.sys.frame += 1;
        Outcome::Return(42)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "present").unwrap();
    emit(&mut m, CODE, |a| { a.push_imm(99).call(present).mov_mem_eax(DATA).ret(); });
    m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert_eq!(m.sys.frame, 1);
    assert_eq!(m.cpu.reg(Reg::Eax), 42);
    assert_ne!(m.cpu.reg(Reg::Eip), PARK_ADDR);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0, "ran past the frame boundary");
    assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Exited(42))));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 42);
    assert_eq!(m.sys.frame, 1, "replayed the present handler");
    assert_eq!(m.sys.scheduling.frame_host_yields, 1);
}

#[test]
fn frame_inside_guest_callback_preserves_continuation_and_stack() {
    let mut r = Registry::default();
    r.add("test", "outer", Conv::Cdecl { argc: 0 }, |_| {
        Outcome::call(PROC, [], |_, value| Outcome::Return(value.value() + 2))
    });
    r.add("test", "present", Conv::Stdcall { argc: 0 }, |e| {
        e.sys.frame += 1; Outcome::Return(8)
    });
    let mut m = fixture(r, Clock::real());
    let outer = m.resolve_import("test", "outer").unwrap();
    let present = m.resolve_import("test", "present").unwrap();
    emit(&mut m, CODE, |a| { a.call(outer).mov_mem_eax(DATA).ret(); });
    emit(&mut m, PROC, |a| { a.call(present).ret(); });
    m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Exited(10))));
    assert_eq!(m.sys.frame, 1);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 10);
}

#[test]
fn timed_host_continuation_can_present_then_resume_exactly_once() {
    let mut r = Registry::default();
    r.add("test", "later", Conv::Stdcall { argc: 0 }, |e| {
        Outcome::block(Wait::Until(e.sys.clock.now_us() + 1_000), |e, _| {
            e.sys.frame += 1; Outcome::Return(17)
        })
    });
    let mut m = fixture(r, Clock::real());
    let later = m.resolve_import("test", "later").unwrap();
    emit(&mut m, CODE, |a| { a.call(later).mov_mem_eax(DATA).ret(); });
    m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
    assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Idle { .. })));
    std::thread::sleep(Duration::from_millis(2));
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
    assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Exited(17))));
    assert_eq!(m.sys.frame, 1);
}

#[test]
fn present_then_block_keeps_the_real_wait_and_exit_wins_over_host_yield() {
    let mut r = Registry::default();
    r.add("test", "present_wait", Conv::Stdcall { argc: 0 }, |e| {
        e.sys.frame += 1;
        Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 50_000), 0)
    });
    r.add("test", "exit_frame", Conv::Stdcall { argc: 0 }, |e| {
        e.sys.frame += 1; Outcome::ExitProcess(23)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "present_wait").unwrap();
    let exit = m.resolve_import("test", "exit_frame").unwrap();
    emit(&mut m, CODE, |a| { a.call(present).call(exit); });
    m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
    assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Idle { .. })));
    let until = match m.sys.context(0).waiting_on() { Some(Wait::Until(t)) => *t, other => panic!("{other:?}") };
    while m.sys.clock.now_us() < until { std::thread::sleep(Duration::from_millis(1)); }
    assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Exited(23))));
    assert_eq!(m.sys.frame, 2);
    assert_eq!(m.sys.scheduling.frame_host_yields, 1);
}

#[test]
fn frame_yields_do_not_renew_the_hard_watchdog() {
    let mut r = Registry::default();
    r.add("test", "present", Conv::Stdcall { argc: 0 }, |e| {
        std::thread::sleep(Duration::from_millis(2));
        e.sys.frame += 1; Outcome::Return(0)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "present").unwrap();
    emit(&mut m, CODE, |a| { a.call(present).jmp(CODE); });
    m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
    m.set_execution_budget(Some(Duration::from_millis(15)));
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(3));
        match step(&mut m) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Failed(e)) => {
                assert!(e.message.contains("execution budget exhausted")); break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(m.sys.scheduling.frame_host_yields > 0);
}

#[test]
fn disabled_policy_and_virtual_time_do_not_insert_frame_yields() {
    for virtual_time in [true, false] {
        let mut r = Registry::default();
        r.add("test", "present", Conv::Stdcall { argc: 0 }, |e| { e.sys.frame += 1; Outcome::Return(5) });
        let mut m = fixture(r, if virtual_time { Clock::virtual_time() } else { Clock::real() });
        if !virtual_time { m.set_latency_scheduling(false); }
        let present = m.resolve_import("test", "present").unwrap();
        emit(&mut m, CODE, |a| { a.call(present).ret(); });
        m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
        assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Exited(5))));
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
    }
}

#[derive(Default)]
struct Frames { times: Vec<u64>, target: usize, period: u64 }
fn timer_machine(policy: bool, workers: usize, period: u64, target: usize) -> Machine<Cpu> {
    let mut r = Registry::default();
    r.add("test", "timer", Conv::Stdcall { argc: 0 }, |e| {
        let now = e.sys.clock.now_us(); let f = e.ext.get_mut::<Frames>(); f.times.push(now);
        if f.times.len() == f.target { return Outcome::ExitProcess(0); }
        // Deliberately not a present: isolate wake/queue latency from the host handoff.
        Outcome::block_then_return(Wait::Until(now + f.period), 0)
    });
    let mut m = fixture(r, Clock::real()); m.set_latency_scheduling(policy);
    m.ext.insert(Frames { target, period, ..Frames::default() });
    let timer = m.resolve_import("test", "timer").unwrap();
    emit(&mut m, CODE, |a| { a.call(timer).jmp(CODE); });
    m.spawn("timer", CODE, &[], 65536).unwrap();
    for i in 0..workers {
        let entry = PROC + 32 * i as u32;
        emit(&mut m, entry, |a| {
            a.bytes.extend_from_slice(&[0xff, 0x05]);
            a.bytes.extend_from_slice(&(DATA + 4 * i as u32).to_le_bytes()); a.jmp(entry);
        });
        m.spawn("compute", entry, &[], 65536).unwrap();
    }
    prepare(&mut m); m
}
fn run_timers(m: &mut Machine<Cpu>) {
    let begin = Instant::now();
    loop {
        assert!(begin.elapsed() < Duration::from_secs(10));
        match m.run_slice(Duration::from_millis(8)) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Exited(0)) => break,
            other => panic!("{other:?}"),
        }
    }
    let f = m.ext.get::<Frames>();
    assert_eq!(f.times.len(), f.target);
    assert!(f.times.windows(2).all(|w| w[1] - w[0] >= f.period), "shortened guest wait");
}
#[test]
fn timed_sleepers_do_not_cause_pointless_quantum_polls() {
    let mut m = timer_machine(true, 1, 10_000, 3); run_timers(&mut m);
    assert!(m.sys.scheduling.internal_preemptions <= 6, "{:?}", m.sys.scheduling);
    assert!(m.cpu.read_u32(DATA).unwrap() > 0);
    assert!(m.sys.scheduling.timed_dispatches > 0);
}
#[test]
fn woken_timer_is_preferred_but_both_compute_threads_progress() {
    let mut m = timer_machine(true, 2, 1_000, 10); run_timers(&mut m);
    assert!(m.sys.scheduling.wake_priority_dispatches > 0);
    for i in 0..2 { assert!(m.cpu.read_u32(DATA + 4*i).unwrap() > 0); }
}
#[test]
#[ignore = "diagnostic only: compare response latency, not equal-work throughput"]
fn report_wake_priority_and_demand_driven_quantum() {
    for workers in [1, 2] { for period in [1_000, 16_667] {
        for (round, policy) in [false, true, true, false].into_iter().enumerate() {
            let mut m = timer_machine(policy, workers, period, 65); run_timers(&mut m);
            let mut gaps: Vec<_> = m.ext.get::<Frames>().times.windows(2).map(|w| w[1]-w[0]).collect();
            gaps.sort_unstable();
            println!("WAKE round={round} policy={policy} workers={workers} period_us={period} p50_us={} p95_us={} p99_us={} cpu_runs={} stats={:?}",
                gaps[31], gaps[60], gaps[63], m.sys.stats.total.cpu_runs, m.sys.scheduling);
        }
    }}
}

#[derive(Default)]
struct PresentedAt(Option<Instant>);
#[test]
#[ignore = "diagnostic only: frame-ready to host-return latency, not compute throughput"]
fn report_present_handoff() {
    for (round, policy) in [false, true, true, false].into_iter().enumerate() {
        let mut samples = Vec::new();
        for _ in 0..9 {
            let mut r = Registry::default();
            r.add("test", "present", Conv::Stdcall { argc: 0 }, |e| {
                e.sys.frame += 1;
                e.ext.get_mut::<PresentedAt>().0 = Some(Instant::now());
                Outcome::Return(0)
            });
            let mut m = fixture(r, Clock::real());
            m.set_latency_scheduling(policy); m.ext.insert(PresentedAt::default());
            let present = m.resolve_import("test", "present").unwrap();
            emit(&mut m, CODE, |a| {
                a.call(present); let top = a.here();
                a.bytes.extend_from_slice(&[0xff, 0x05]); a.bytes.extend_from_slice(&DATA.to_le_bytes()); a.jmp(top);
            });
            m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
            assert!(matches!(m.run_slice(Duration::from_millis(8)), SliceResult::Yielded));
            samples.push(m.ext.get::<PresentedAt>().0.unwrap().elapsed().as_nanos());
            assert_eq!(m.sys.frame, 1);
            if policy { assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0); }
        }
        samples.sort_unstable();
        println!("HANDOFF round={round} policy={policy} p50_ns={} max_ns={} samples_ns={samples:?}", samples[4], samples[8]);
    }
}

#[test]
#[ignore = "diagnostic: identical finite guest work, compile/setup excluded"]
fn report_equal_work_throughput() {
    const N: u32 = 500_000;
    for (round, policy) in [false, true, true, false].into_iter().enumerate() {
        let mut samples = Vec::new();
        for _ in 0..7 {
            let mut r = Registry::default();
            r.add("test", "join", Conv::Stdcall { argc: 1 }, |e| {
                let handle = e.arg(0);
                if e.sys.try_acquire(handle) { Outcome::Return(0) }
                else { Outcome::block_then_return(Wait::Object { handle, deadline: None }, 0) }
            });
            let mut m = fixture(r, Clock::real()); m.set_latency_scheduling(policy);
            let join = m.resolve_import("test", "join").unwrap();
            m.spawn("main", CODE, &[], 65536).unwrap();
            let id = m.spawn("worker", PROC, &[], 65536).unwrap();
            let handle = m.sys.context(id).thread_handle;
            for (entry, data) in [(CODE, DATA), (PROC, DATA + 4)] {
                emit(&mut m, entry, |a| {
                    a.mov_ecx_imm(N); let top = a.here();
                    a.bytes.extend_from_slice(&[0xff, 0x05]); a.bytes.extend_from_slice(&data.to_le_bytes());
                    a.dec_ecx().jnz(top);
                    if entry == CODE { a.push_imm(handle).call(join); }
                    a.ret();
                });
            }
            prepare(&mut m); let start = Instant::now();
            loop {
                assert!(start.elapsed() < Duration::from_secs(3));
                match m.run_slice(Duration::from_millis(8)) {
                    SliceResult::Yielded => {}
                    SliceResult::Complete(RunResult::Exited(0)) => break,
                    other => panic!("{other:?}"),
                }
            }
            samples.push(start.elapsed().as_nanos());
            assert_eq!(m.cpu.read_u32(DATA).unwrap(), N);
            assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), N);
        }
        samples.sort_unstable();
        println!("THROUGHPUT round={round} policy={policy} iterations={} median_ns={} samples_ns={samples:?}", N * 2, samples[3]);
    }
}

#[test]
fn hard_watchdog_wins_over_a_frame_handoff() {
    let mut r = Registry::default();
    r.add("test", "slow_present", Conv::Stdcall { argc: 0 }, |e| {
        std::thread::sleep(Duration::from_millis(15));
        e.sys.frame += 1; Outcome::Return(0)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "slow_present").unwrap();
    emit(&mut m, CODE, |a| { a.call(present).ret(); });
    m.spawn("renderer", CODE, &[], 65536).unwrap(); prepare(&mut m);
    m.set_execution_budget(Some(Duration::from_millis(5)));
    assert!(matches!(step(&mut m), SliceResult::Complete(RunResult::Failed(e)) if e.message.contains("execution budget exhausted")));
    assert_eq!(m.sys.scheduling.frame_host_yields, 0);
}
