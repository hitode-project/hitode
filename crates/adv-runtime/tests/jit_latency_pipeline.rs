//! Complete trap/continuation and host-frame boundaries, using authored guest code.
use adv_cpu::backend::asm::Asm;
use std::time::{Duration, Instant};

use adv_abi::Registry;
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, SliceResult, Wait};

type Cpu = adv_cpu::JitCpu;
fn new_cpu() -> Cpu {
    adv_cpu::JitCpu::new(PARK_ADDR, true).unwrap()
}
fn prepare(m: &mut Machine<Cpu>) {
    // Exercise an actual native call into the trap adapter, not just interpretation.
    let mut region = adv_cpu::Region::new(CODE, 512);
    region.reachable = true;
    region.entries = vec![CODE, PROC, PROC + 32, PROC + 128];
    m.cpu.executor_mut().add_region(region).unwrap();
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
    let mut a = Asm::new(at);
    f(&mut a);
    m.cpu.write(at, &a.bytes).unwrap();
}
fn step(m: &mut Machine<Cpu>) -> SliceResult {
    m.run_slice(Duration::from_millis(200))
}

#[test]
fn completed_frame_yields_without_losing_pc_registers_or_replaying_api() {
    let mut r = Registry::default();
    r.add("test", "present", Conv::Stdcall { argc: 1 }, |e| {
        assert_eq!(e.arg(0), 99);
        e.sys.frame += 1;
        e.sys.complete_frame();
        Outcome::Return(42)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "present").unwrap();
    emit(&mut m, CODE, |a| {
        a.push_imm(99).call(present).mov_mem_eax(DATA).ret();
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert_eq!(m.sys.frame, 1);
    assert_eq!(m.cpu.reg(Reg::Eax), 42);
    assert_ne!(m.cpu.reg(Reg::Eip), PARK_ADDR);
    assert_eq!(
        m.cpu.read_u32(DATA).unwrap(),
        0,
        "ran past the frame boundary"
    );
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Exited(42))
    ));
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
        e.sys.frame += 1;
        e.sys.complete_frame();
        Outcome::Return(8)
    });
    let mut m = fixture(r, Clock::real());
    let outer = m.resolve_import("test", "outer").unwrap();
    let present = m.resolve_import("test", "present").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(outer).mov_mem_eax(DATA).ret();
    });
    emit(&mut m, PROC, |a| {
        a.call(present).ret();
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Exited(10))
    ));
    assert_eq!(m.sys.frame, 1);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 10);
}

#[test]
fn timed_host_continuation_can_present_then_resume_exactly_once() {
    let mut r = Registry::default();
    r.add("test", "later", Conv::Stdcall { argc: 0 }, |e| {
        Outcome::block(Wait::Until(e.sys.clock.now_us() + 1_000), |e, _| {
            e.sys.frame += 1;
            e.sys.complete_frame();
            Outcome::Return(17)
        })
    });
    let mut m = fixture(r, Clock::real());
    let later = m.resolve_import("test", "later").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(later).mov_mem_eax(DATA).ret();
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Idle { .. })
    ));
    std::thread::sleep(Duration::from_millis(2));
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Exited(17))
    ));
    assert_eq!(m.sys.frame, 1);
}

#[test]
fn present_then_block_keeps_the_real_wait_and_exit_wins_over_host_yield() {
    let mut r = Registry::default();
    r.add("test", "present_wait", Conv::Stdcall { argc: 0 }, |e| {
        e.sys.frame += 1;
        e.sys.complete_frame();
        Outcome::block_then_return(Wait::Until(e.sys.clock.now_us() + 50_000), 0)
    });
    r.add("test", "exit_frame", Conv::Stdcall { argc: 0 }, |e| {
        e.sys.frame += 1;
        e.sys.complete_frame();
        Outcome::ExitProcess(23)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "present_wait").unwrap();
    let exit = m.resolve_import("test", "exit_frame").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(present).call(exit);
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Idle { .. })
    ));
    let until = match m.sys.context(0).waiting_on() {
        Some(Wait::Until(t)) => *t,
        other => panic!("{other:?}"),
    };
    while m.sys.clock.now_us() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Exited(23))
    ));
    assert_eq!(m.sys.frame, 2);
    assert_eq!(m.sys.scheduling.frame_host_yields, 1);
}

#[test]
fn frame_yields_do_not_renew_the_hard_watchdog() {
    let mut r = Registry::default();
    r.add("test", "present", Conv::Stdcall { argc: 0 }, |e| {
        std::thread::sleep(Duration::from_millis(2));
        e.sys.frame += 1;
        e.sys.complete_frame();
        Outcome::Return(0)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "present").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(present).jmp(CODE);
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    // Compile the first frame before measuring the watchdog across handoffs.
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert!(m.sys.scheduling.frame_host_yields > 0);
    m.set_execution_budget(Some(Duration::from_millis(15)));
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(3));
        match step(&mut m) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Failed(e)) => {
                assert!(e.message.contains("execution budget exhausted"));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn disabled_policy_and_virtual_time_do_not_insert_frame_yields() {
    for virtual_time in [true, false] {
        let mut r = Registry::default();
        r.add("test", "present", Conv::Stdcall { argc: 0 }, |e| {
            e.sys.frame += 1;
            e.sys.complete_frame();
            Outcome::Return(5)
        });
        let mut m = fixture(
            r,
            if virtual_time {
                Clock::virtual_time()
            } else {
                Clock::real()
            },
        );
        if !virtual_time {
            m.set_latency_scheduling(false);
        }
        let present = m.resolve_import("test", "present").unwrap();
        emit(&mut m, CODE, |a| {
            a.call(present).ret();
        });
        m.spawn("renderer", CODE, &[], 65536).unwrap();
        prepare(&mut m);
        assert!(matches!(
            step(&mut m),
            SliceResult::Complete(RunResult::Exited(5))
        ));
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
    }
}

#[derive(Default)]
struct Frames {
    times: Vec<u64>,
    target: usize,
    period: u64,
}
fn timer_machine(policy: bool, workers: usize, period: u64, target: usize) -> Machine<Cpu> {
    let mut r = Registry::default();
    r.add("test", "timer", Conv::Stdcall { argc: 0 }, |e| {
        let now = e.sys.clock.now_us();
        let f = e.ext.get_mut::<Frames>();
        f.times.push(now);
        if f.times.len() == f.target {
            return Outcome::ExitProcess(0);
        }
        // Deliberately not a present: isolate wake/queue latency from the host handoff.
        Outcome::block_then_return(Wait::Until(now + f.period), 0)
    });
    let mut m = fixture(r, Clock::real());
    m.set_latency_scheduling(policy);
    m.ext.insert(Frames {
        target,
        period,
        ..Frames::default()
    });
    let timer = m.resolve_import("test", "timer").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(timer).jmp(CODE);
    });
    m.spawn("timer", CODE, &[], 65536).unwrap();
    for i in 0..workers {
        let entry = PROC + 32 * i as u32;
        emit(&mut m, entry, |a| {
            a.bytes.extend_from_slice(&[0xff, 0x05]);
            a.bytes
                .extend_from_slice(&(DATA + 4 * i as u32).to_le_bytes());
            a.jmp(entry);
        });
        m.spawn("compute", entry, &[], 65536).unwrap();
    }
    prepare(&mut m);
    m
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
    assert!(
        f.times.windows(2).all(|w| w[1] - w[0] >= f.period),
        "shortened guest wait"
    );
}
#[test]
fn timed_sleepers_do_not_cause_pointless_quantum_polls() {
    let mut m = timer_machine(true, 1, 10_000, 3);
    run_timers(&mut m);
    assert!(
        m.sys.scheduling.internal_preemptions <= 6,
        "{:?}",
        m.sys.scheduling
    );
    assert!(m.cpu.read_u32(DATA).unwrap() > 0);
    assert!(m.sys.scheduling.timed_dispatches > 0);
}
#[test]
fn woken_timer_is_preferred_but_both_compute_threads_progress() {
    let mut m = timer_machine(true, 2, 1_000, 10);
    run_timers(&mut m);
    assert!(m.sys.scheduling.wake_priority_dispatches > 0);
    for i in 0..2 {
        assert!(m.cpu.read_u32(DATA + 4 * i).unwrap() > 0);
    }
}
#[test]
#[ignore = "diagnostic only: compare response latency, not equal-work throughput"]
fn report_wake_priority_and_demand_driven_quantum() {
    for workers in [1, 2] {
        for period in [1_000, 16_667] {
            for (round, policy) in [false, true, true, false].into_iter().enumerate() {
                let mut m = timer_machine(policy, workers, period, 65);
                run_timers(&mut m);
                let mut gaps: Vec<_> = m
                    .ext
                    .get::<Frames>()
                    .times
                    .windows(2)
                    .map(|w| w[1] - w[0])
                    .collect();
                gaps.sort_unstable();
                println!(
                    "WAKE round={round} policy={policy} workers={workers} period_us={period} p50_us={} p95_us={} p99_us={} cpu_runs={} stats={:?}",
                    gaps[31], gaps[60], gaps[63], m.sys.stats.total.cpu_runs, m.sys.scheduling
                );
            }
        }
    }
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
                e.sys.complete_frame();
                e.ext.get_mut::<PresentedAt>().0 = Some(Instant::now());
                Outcome::Return(0)
            });
            let mut m = fixture(r, Clock::real());
            m.set_latency_scheduling(policy);
            m.ext.insert(PresentedAt::default());
            let present = m.resolve_import("test", "present").unwrap();
            emit(&mut m, CODE, |a| {
                a.call(present);
                let top = a.here();
                a.bytes.extend_from_slice(&[0xff, 0x05]);
                a.bytes.extend_from_slice(&DATA.to_le_bytes());
                a.jmp(top);
            });
            m.spawn("renderer", CODE, &[], 65536).unwrap();
            prepare(&mut m);
            assert!(matches!(
                m.run_slice(Duration::from_millis(8)),
                SliceResult::Yielded
            ));
            samples.push(m.ext.get::<PresentedAt>().0.unwrap().elapsed().as_nanos());
            assert_eq!(m.sys.frame, 1);
            if policy {
                assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
            }
        }
        samples.sort_unstable();
        println!(
            "HANDOFF round={round} policy={policy} p50_ns={} max_ns={} samples_ns={samples:?}",
            samples[4], samples[8]
        );
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
                if e.sys.try_acquire(handle) {
                    Outcome::Return(0)
                } else {
                    Outcome::block_then_return(
                        Wait::Object {
                            handle,
                            deadline: None,
                        },
                        0,
                    )
                }
            });
            let mut m = fixture(r, Clock::real());
            m.set_latency_scheduling(policy);
            let join = m.resolve_import("test", "join").unwrap();
            m.spawn("main", CODE, &[], 65536).unwrap();
            let id = m.spawn("worker", PROC, &[], 65536).unwrap();
            let handle = m.sys.context(id).thread_handle;
            for (entry, data) in [(CODE, DATA), (PROC, DATA + 4)] {
                emit(&mut m, entry, |a| {
                    a.mov_ecx_imm(N);
                    let top = a.here();
                    a.bytes.extend_from_slice(&[0xff, 0x05]);
                    a.bytes.extend_from_slice(&data.to_le_bytes());
                    a.dec_ecx().jnz(top);
                    if entry == CODE {
                        a.push_imm(handle).call(join);
                    }
                    a.ret();
                });
            }
            prepare(&mut m);
            let start = Instant::now();
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
        println!(
            "THROUGHPUT round={round} policy={policy} iterations={} median_ns={} samples_ns={samples:?}",
            N * 2,
            samples[3]
        );
    }
}

#[test]
fn starting_a_trace_frame_does_not_handoff_before_its_callback_commits() {
    let mut r = Registry::default();
    r.add("test", "start_frame", Conv::Cdecl { argc: 0 }, |e| {
        e.sys.frame += 1;
        Outcome::call(PROC, [], |e, value| {
            assert_eq!(value.value(), 31);
            assert_eq!(
                e.cpu.read_u32(DATA).unwrap(),
                31,
                "task callback has not run"
            );
            e.sys.complete_frame();
            Outcome::defer(|e| {
                e.cpu.write_u32(DATA + 4, 77).unwrap();
                Outcome::Return(31)
            })
        })
    });
    let mut m = fixture(r, Clock::real());
    let start = m.resolve_import("test", "start_frame").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(start).mov_mem_eax(DATA + 8).ret();
    });
    emit(&mut m, PROC, |a| {
        a.bytes.extend_from_slice(&[0xb8, 31, 0, 0, 0]);
        a.mov_mem_eax(DATA).ret();
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert_eq!(m.sys.frame, 1);
    assert_eq!(m.sys.completed_frames, 1);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 31);
    assert_eq!(
        m.cpu.read_u32(DATA + 4).unwrap(),
        0,
        "started the following host work"
    );
    assert_eq!(
        m.cpu.read_u32(DATA + 8).unwrap(),
        0,
        "returned to the guest too early"
    );
    assert_eq!(m.sys.context(0).state_name(), "Ready");
    assert!(
        m.sys.context(0).blocked_stack().is_some(),
        "lost the deferred origin"
    );
    assert_eq!(
        m.sys.scheduling.wake_priority_dispatches, 0,
        "host deferral is not a wake"
    );
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Exited(31))
    ));
    assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 77);
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 31);
    assert_eq!(m.sys.host_calls, 1, "replayed the start-frame handler");
    assert_eq!(m.sys.scheduling.frame_host_yields, 1);
    assert!(m.sys.context(0).blocked_stack().is_none());
}

#[test]
fn deferred_frame_continuations_stay_inline_without_the_real_time_policy() {
    for mode in ["virtual", "disabled", "unsliced"] {
        let mut r = Registry::default();
        r.add("test", "present", Conv::Cdecl { argc: 0 }, |e| {
            e.sys.complete_frame();
            Outcome::defer(|e| {
                e.cpu.write_u32(DATA, 9).unwrap();
                Outcome::Return(9)
            })
        });
        let mut m = fixture(
            r,
            if mode == "virtual" {
                Clock::virtual_time()
            } else {
                Clock::real()
            },
        );
        if mode == "disabled" {
            m.set_latency_scheduling(false);
        }
        let present = m.resolve_import("test", "present").unwrap();
        emit(&mut m, CODE, |a| {
            a.call(present).ret();
        });
        m.spawn("renderer", CODE, &[], 65536).unwrap();
        prepare(&mut m);
        let result = if mode == "unsliced" {
            SliceResult::Complete(m.run())
        } else {
            step(&mut m)
        };
        assert!(
            matches!(result, SliceResult::Complete(RunResult::Exited(9))),
            "{mode}: {result:?}"
        );
        assert_eq!(m.sys.completed_frames, 1);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 9);
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
        assert_eq!(m.sys.scheduling.wake_dispatches, 0);
    }
}

#[test]
fn frame_inside_nested_callbacks_survives_a_peer_context_switch() {
    let mut r = Registry::default();
    r.add("test", "outer", Conv::Cdecl { argc: 0 }, |e| {
        e.ext.get_mut::<[u32; 6]>()[0] += 1;
        Outcome::call(PROC, [], |e, value| {
            e.ext.get_mut::<[u32; 6]>()[3] += 1;
            assert_ne!(e.cpu.read_u32(DATA + 20).unwrap(), 0, "peer never ran");
            Outcome::Return(value.value() + 3)
        })
    });
    r.add("test", "inner", Conv::Cdecl { argc: 0 }, |e| {
        e.ext.get_mut::<[u32; 6]>()[1] += 1;
        Outcome::call(PROC + 32, [], |e, value| {
            e.ext.get_mut::<[u32; 6]>()[4] += 1;
            Outcome::Return(value.value() + 2)
        })
    });
    r.add("test", "present", Conv::Stdcall { argc: 0 }, |e| {
        e.ext.get_mut::<[u32; 6]>()[2] += 1;
        e.sys.complete_frame();
        Outcome::defer(|e| {
            e.ext.get_mut::<[u32; 6]>()[5] += 1;
            Outcome::Return(8)
        })
    });
    let mut m = fixture(r, Clock::real());
    m.ext.insert([0u32; 6]);
    m.cpu.write_u32(DATA + 24, 1.25f32.to_bits()).unwrap();
    m.cpu.write_u32(DATA + 44, 9.5f32.to_bits()).unwrap();
    let outer = m.resolve_import("test", "outer").unwrap();
    let inner = m.resolve_import("test", "inner").unwrap();
    let present = m.resolve_import("test", "present").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(outer).mov_mem_eax(DATA).ret();
    });
    emit(&mut m, PROC, |a| {
        a.call(inner).ret();
    });
    // Keep this callback below the next entry at PROC+128.
    const PEER: u32 = PROC + 128;
    emit(&mut m, PROC + 32, |a| {
        a.bytes.extend_from_slice(&[0x64, 0xa1, 0x18, 0, 0, 0]); // mov eax, fs:[TEB.Self]
        a.mov_mem_eax(DATA + 16);
        a.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
        a.mov_mem_eax(DATA + 36);
        a.bytes.extend_from_slice(&[0xd9, 0x05]);
        a.bytes.extend_from_slice(&(DATA + 24).to_le_bytes());
        a.call(present).mov_mem_eax(DATA + 4);
        a.bytes.extend_from_slice(&[0x89, 0xe0]);
        a.mov_mem_eax(DATA + 40);
        a.bytes.extend_from_slice(&[0xd9, 0x1d]);
        a.bytes.extend_from_slice(&(DATA + 28).to_le_bytes());
        a.bytes.extend_from_slice(&[0xb8, 8, 0, 0, 0]);
        a.ret();
    });
    emit(&mut m, PEER, |a| {
        a.bytes.extend_from_slice(&[0x64, 0xa1, 0x18, 0, 0, 0]);
        a.mov_mem_eax(DATA + 20);
        a.bytes.extend_from_slice(&[0xd9, 0x05]);
        a.bytes.extend_from_slice(&(DATA + 44).to_le_bytes());
        a.bytes.extend_from_slice(&[0xd9, 0x1d]);
        a.bytes.extend_from_slice(&(DATA + 32).to_le_bytes());
        a.ret();
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    let peer = m.spawn("peer", PEER, &[], 65536).unwrap();
    prepare(&mut m);
    let initial_esp = m.sys.context(0).stack_hi - 0x14;
    assert!(matches!(step(&mut m), SliceResult::Yielded));
    assert_eq!(m.sys.completed_frames, 1);
    assert_eq!(
        m.cpu.read_u32(DATA + 28).unwrap(),
        0,
        "ran beyond presentation"
    );
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Exited(13))
    ));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 13);
    assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 8);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), m.sys.context(0).teb);
    assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), m.sys.context(peer).teb);
    assert_eq!(m.cpu.read_u32(DATA + 28).unwrap(), 1.25f32.to_bits());
    assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), 9.5f32.to_bits());
    assert_eq!(
        m.cpu.read_u32(DATA + 36).unwrap(),
        m.cpu.read_u32(DATA + 40).unwrap()
    );
    assert_eq!(m.cpu.reg(Reg::Esp), initial_esp + 4);
    assert_eq!(
        *m.ext.get::<[u32; 6]>(),
        [1; 6],
        "handler or continuation replayed"
    );
}

#[test]
fn granted_event_has_one_wake_turn_then_the_normal_peer_runs() {
    use adv_machine::{Object, Resume, WakeReason};
    let mut r = Registry::default();
    r.add("test", "wait", Conv::Stdcall { argc: 1 }, |e| {
        let handle = e.arg(0);
        assert!(!e.sys.try_acquire(handle));
        Outcome::block(
            Wait::Object {
                handle,
                deadline: None,
            },
            move |e, wake| {
                assert_eq!(wake, Resume::Woke(WakeReason::Signaled));
                e.ext.get_mut::<Vec<u32>>().push(0);
                Outcome::block(
                    Wait::Object {
                        handle,
                        deadline: None,
                    },
                    |_, _| Outcome::Fail("event woke twice".into()),
                )
            },
        )
    });
    r.add("test", "signal", Conv::Stdcall { argc: 1 }, |e| {
        let handle = e.arg(0);
        e.ext.get_mut::<Vec<u32>>().push(1);
        assert!(e.sys.set_event(handle));
        assert_eq!(e.sys.context(0).state_name(), "Ready");
        assert!(
            !e.sys.try_acquire(handle),
            "signaler stole the granted signal"
        );
        assert!(e.sys.reset_event(handle));
        assert_eq!(
            *e.ext.get::<Vec<u32>>(),
            [1],
            "SetEvent ran guest work inline"
        );
        Outcome::Return(0)
    });
    r.add("test", "normal", Conv::Stdcall { argc: 0 }, |e| {
        assert_eq!(
            e.sys.context(0).state_name(),
            "Blocked",
            "the granted turn must reblock before the normal peer runs"
        );
        assert_eq!(*e.ext.get::<Vec<u32>>(), [1, 0]);
        e.ext.get_mut::<Vec<u32>>().push(2);
        Outcome::ExitProcess(0)
    });
    let mut m = fixture(r, Clock::real());
    m.ext.insert(Vec::<u32>::new());
    let event = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let wait = m.resolve_import("test", "wait").unwrap();
    let signal = m.resolve_import("test", "signal").unwrap();
    let normal = m.resolve_import("test", "normal").unwrap();
    emit(&mut m, CODE, |a| {
        a.push_imm(event).call(wait).ret();
    });
    emit(&mut m, PROC, |a| {
        a.push_imm(event).call(signal);
        let top = a.here();
        a.bytes.extend_from_slice(&[0xff, 0x05]);
        a.bytes.extend_from_slice(&DATA.to_le_bytes());
        a.jmp(top);
    });
    emit(&mut m, PROC + 32, |a| {
        a.call(normal);
    });
    m.spawn("receiver", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    // Establish the wait before publishing the signaler. Native compilation can
    // consume a quantum before the receiver reaches its first host call.
    assert!(matches!(
        step(&mut m),
        SliceResult::Complete(RunResult::Idle { .. })
    ));
    assert_eq!(m.sys.context(0).state_name(), "Blocked");
    m.spawn("signaler", PROC, &[], 65536).unwrap();
    m.spawn("normal peer", PROC + 32, &[], 65536).unwrap();
    let started = Instant::now();
    loop {
        assert!(started.elapsed() < Duration::from_secs(3));
        match step(&mut m) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Exited(0)) => break,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(
        *m.ext.get::<Vec<u32>>(),
        [1, 0, 2],
        "wake rewound or bypassed the normal lane"
    );
    assert_eq!(m.sys.scheduling.wake_dispatches, 1);
    assert_eq!(m.sys.scheduling.wake_priority_dispatches, 1);
    assert_eq!(m.sys.scheduling.timed_dispatches, 0);
    assert!(
        m.sys
            .contexts
            .iter()
            .all(|context| context.exit_code() == Some(0)),
        "ExitProcess terminates both the blocked receiver and computing signaler"
    );
    assert!(!m.sys.try_acquire(event));
}

#[test]
fn completed_frames_do_not_mask_failure_or_a_deferred_watchdog_expiration() {
    for slow in [false, true] {
        let mut r = Registry::default();
        r.add("test", "fail_frame", Conv::Cdecl { argc: 0 }, |e| {
            e.sys.complete_frame();
            Outcome::Fail("draw failed".into())
        });
        r.add("test", "slow_frame", Conv::Cdecl { argc: 0 }, |e| {
            std::thread::sleep(Duration::from_millis(15));
            e.sys.complete_frame();
            Outcome::defer(|_| panic!("expired watchdog ran the deferred continuation"))
        });
        let mut m = fixture(r, Clock::real());
        let frame = m
            .resolve_import("test", if slow { "slow_frame" } else { "fail_frame" })
            .unwrap();
        emit(&mut m, CODE, |a| {
            a.call(frame).ret();
        });
        m.spawn("renderer", CODE, &[], 65536).unwrap();
        prepare(&mut m);
        if slow {
            m.set_execution_budget(Some(Duration::from_millis(5)));
        }
        let result = step(&mut m);
        assert!(
            matches!(result, SliceResult::Complete(RunResult::Failed(ref error))
            if error.message.contains(if slow { "execution budget exhausted" } else { "draw failed" })),
            "{result:?}"
        );
        assert_eq!(m.sys.completed_frames, 1);
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
    }
}

#[test]
fn hard_watchdog_wins_over_a_frame_handoff() {
    let mut r = Registry::default();
    r.add("test", "slow_present", Conv::Stdcall { argc: 0 }, |e| {
        std::thread::sleep(Duration::from_millis(15));
        e.sys.frame += 1;
        e.sys.complete_frame();
        Outcome::Return(0)
    });
    let mut m = fixture(r, Clock::real());
    let present = m.resolve_import("test", "slow_present").unwrap();
    emit(&mut m, CODE, |a| {
        a.call(present).ret();
    });
    m.spawn("renderer", CODE, &[], 65536).unwrap();
    prepare(&mut m);
    m.set_execution_budget(Some(Duration::from_millis(5)));
    assert!(
        matches!(step(&mut m), SliceResult::Complete(RunResult::Failed(e)) if e.message.contains("execution budget exhausted"))
    );
    assert_eq!(m.sys.scheduling.frame_host_yields, 0);
}
