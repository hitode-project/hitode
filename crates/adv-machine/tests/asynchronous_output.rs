//! A separate output consumer must not disable context scheduling or fault/IRQ
//! boundaries merely because completed frames no longer force host handoff.
use adv_abi::Registry;
use adv_cpu::backend::{Backend, Guest, GuestExt, Prot, asm::Asm};
use adv_cpu::{JitCpu, Region};
use adv_machine::{Clock, Conv, Env, Machine, Object, Outcome, RunResult, SliceResult, Wait};
use adv_unicorn::UnicornCpu;
use std::time::{Duration, Instant};

const CODE: u32 = 0x400000;
const PEER: u32 = CODE + 0x100;
const DATA: u32 = 0x410000;
const PARK: u32 = adv_machine::layout::PARK_ADDR;

fn setup<B: Backend>(cpu: B, registry: Registry) -> Machine<B> {
    let mut m = Machine::new(cpu, registry, Clock::real()).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.set_execution_budget(Some(Duration::from_secs(3)));
    m
}
fn compile(cpu: &mut JitCpu, start: u32, len: usize) {
    let mut region = Region::new(start, len);
    region.reachable = true;
    cpu.executor_mut().add_region(region).unwrap();
}
fn burst<B: Backend>(
    cpu: B,
    prepare: impl FnOnce(&mut B, u32, usize),
    handoff: bool,
    stats: bool,
    count: u32,
    deferred: bool,
) -> (Machine<B>, Duration) {
    let mut registry = Registry::default();
    fn returned(env: &mut Env<'_>) -> Outcome {
        let calls = env.cpu.read_u32(DATA).unwrap() + 1;
        env.cpu.write_u32(DATA, calls).unwrap();
        Outcome::Return(calls)
    }
    let handler: fn(&mut Env<'_>) -> Outcome = if deferred {
        |env| {
            env.sys.frame += 1;
            env.sys.complete_frame();
            Outcome::defer(returned)
        }
    } else {
        |env| {
            env.sys.frame += 1;
            env.sys.complete_frame();
            returned(env)
        }
    };
    registry.add("test", "present", Conv::Stdcall { argc: 0 }, handler);
    let mut m = setup(cpu, registry);
    m.set_frame_handoff(handoff);
    m.sys.stats.enabled = stats;
    let present = m.resolve_import("test", "present").unwrap();
    let mut a = Asm::new(CODE);
    a.mov_ecx_imm(count);
    let top = a.here();
    a.call(present).dec_ecx().jnz(top).ret();
    m.cpu.write(CODE, &a.bytes).unwrap();
    prepare(&mut m.cpu, CODE, a.bytes.len());
    m.spawn("producer", CODE, &[], 65536).unwrap();
    let start = Instant::now();
    loop {
        match m.run_slice(Duration::from_millis(200)) {
            SliceResult::Yielded => assert!(start.elapsed() < Duration::from_secs(3)),
            SliceResult::Complete(RunResult::Exited(value)) => {
                assert_eq!(value, count);
                break;
            }
            other => panic!("unexpected burst outcome: {other:?}"),
        }
    }
    let elapsed = start.elapsed();
    assert_eq!(
        m.cpu.read_u32(DATA).unwrap(),
        count,
        "every deferred host continuation runs once"
    );
    assert_eq!(m.sys.frame, count);
    assert_eq!(m.sys.completed_frames, count);
    assert_eq!(m.sys.host_calls, u64::from(count));
    assert_eq!(
        m.sys.scheduling.frame_host_yields,
        if handoff || stats {
            u64::from(count)
        } else {
            0
        }
    );
    if stats {
        assert_eq!(m.sys.stats.frames.len(), count as usize);
    }
    (m, elapsed)
}

#[test]
fn asynchronous_frames_keep_callbacks_and_diagnostic_rows_without_forced_handoff() {
    for (handoff, stats) in [(true, false), (false, false), (false, true)] {
        burst(
            UnicornCpu::new(PARK).unwrap(),
            |_, _, _| {},
            handoff,
            stats,
            64,
            true,
        );
        burst(
            JitCpu::new(PARK, false).unwrap(),
            |_, _, _| {},
            handoff,
            stats,
            64,
            true,
        );
        let (m, _) = burst(
            JitCpu::new(PARK, true).unwrap(),
            compile,
            handoff,
            stats,
            64,
            true,
        );
        assert!(m.cpu.stats().native_entries > 0);
    }
}

fn peer_progress<B: Backend>(cpu: B, mut prepare: impl FnMut(&mut B, u32, usize)) {
    let mut registry = Registry::default();
    registry.add("test", "present", Conv::Stdcall { argc: 0 }, |env| {
        env.sys.complete_frame();
        env.sys.set_event(*env.ext.get::<u32>());
        Outcome::Return(0)
    });
    registry.add("test", "wait", Conv::Stdcall { argc: 0 }, |env| {
        Outcome::block_then_return(
            Wait::Object {
                handle: *env.ext.get::<u32>(),
                deadline: None,
            },
            0,
        )
    });
    registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitProcess(7)
    });
    let mut m = setup(cpu, registry);
    m.set_frame_handoff(false);
    let event = m.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    m.ext.insert(event);
    let present = m.resolve_import("test", "present").unwrap();
    let wait = m.resolve_import("test", "wait").unwrap();
    let done = m.resolve_import("test", "done").unwrap();
    let mut producer = Asm::new(CODE);
    producer.call(present);
    let spin = producer.here();
    producer.jmp(spin);
    let mut peer = Asm::new(PEER);
    peer.call(wait).mov_mem_imm(DATA, 99).call(done);
    m.cpu.write(CODE, &producer.bytes).unwrap();
    m.cpu.write(PEER, &peer.bytes).unwrap();
    prepare(&mut m.cpu, CODE, producer.bytes.len());
    prepare(&mut m.cpu, PEER, peer.bytes.len());
    m.spawn("producer", CODE, &[], 65536).unwrap();
    m.spawn("waiter", PEER, &[], 65536).unwrap();
    assert!(matches!(
        m.run_slice(Duration::from_millis(200)),
        SliceResult::Complete(RunResult::Exited(7))
    ));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 99);
    assert_eq!(m.sys.completed_frames, 1);
    assert_eq!(m.sys.scheduling.frame_host_yields, 0);
    assert!(
        m.sys.scheduling.internal_preemptions > 0,
        "context quantum remained active"
    );
}

#[test]
fn independent_consumer_does_not_disable_runnable_peer_or_object_wake_scheduling() {
    peer_progress(UnicornCpu::new(PARK).unwrap(), |_, _, _| {});
    peer_progress(JitCpu::new(PARK, false).unwrap(), |_, _, _| {});
    peer_progress(JitCpu::new(PARK, true).unwrap(), compile);
}

#[test]
fn output_completion_never_masks_a_handler_failure() {
    for jit in [false, true] {
        let mut r = Registry::default();
        r.add("test", "fail", Conv::Stdcall { argc: 0 }, |env| {
            env.sys.complete_frame();
            Outcome::Fail("authored output failure".into())
        });
        let mut m = setup(JitCpu::new(PARK, jit).unwrap(), r);
        m.set_frame_handoff(false);
        let fail = m.resolve_import("test", "fail").unwrap();
        let mut a = Asm::new(CODE);
        a.call(fail).ret();
        m.cpu.write(CODE, &a.bytes).unwrap();
        if jit {
            compile(&mut m.cpu, CODE, a.bytes.len());
        }
        m.spawn("producer", CODE, &[], 65536).unwrap();
        let SliceResult::Complete(RunResult::Failed(error)) =
            m.run_slice(Duration::from_millis(200))
        else {
            panic!("output masked failure");
        };
        assert!(error.message.contains("authored output failure"));
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
    }
}

#[test]
fn expired_watchdog_stops_before_explicit_deferred_work_without_frame_handoff() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    for jit in [false, true] {
        let mut r = Registry::default();
        r.add("test", "present", Conv::Stdcall { argc: 0 }, |env| {
            env.sys.complete_frame();
            env.ext
                .get::<Arc<AtomicBool>>()
                .store(true, Ordering::Release);
            Outcome::defer(|_| panic!("expired watchdog ran subsequent host work"))
        });
        let mut m = setup(JitCpu::new(PARK, jit).unwrap(), r);
        m.set_frame_handoff(false);
        let flag = Arc::new(AtomicBool::new(false));
        m.cpu.executor_mut().set_host_interrupt_flag(flag.clone());
        m.ext.insert(flag.clone());
        let present = m.resolve_import("test", "present").unwrap();
        let mut a = Asm::new(CODE);
        a.call(present).ret();
        m.cpu.write(CODE, &a.bytes).unwrap();
        if jit {
            compile(&mut m.cpu, CODE, a.bytes.len());
        }
        m.spawn("producer", CODE, &[], 65536).unwrap();

        // A short wall budget can expire during cold execution before the frame
        // handler runs. Pause explicitly after it commits the frame and queues
        // Defer, then expire the watchdog at that retained driver boundary.
        while !flag.load(Ordering::Acquire) {
            let result = m.run_slice(Duration::from_millis(200));
            assert!(matches!(result, SliceResult::Yielded), "{result:?}");
        }
        assert_eq!(m.sys.completed_frames, 1);
        assert_eq!(m.sys.host_calls, 1);
        assert_eq!(m.sys.scheduling.external_interrupts, 1);
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
        assert!(m.sys.context(0).blocked_thunk().is_some());
        let cpu_runs = m.sys.stats.total.cpu_runs;

        // Hard expiration must win over the queued continuation and any frame
        // handoff; no real-time sleep or startup-speed assumption is required.
        m.set_execution_budget(Some(Duration::ZERO));
        flag.store(false, Ordering::Release);
        let SliceResult::Complete(RunResult::Failed(error)) =
            m.run_slice(Duration::from_millis(200))
        else {
            panic!("frame hid expired budget");
        };
        assert!(error.message.contains("execution budget exhausted"));
        assert_eq!(m.sys.completed_frames, 1);
        assert_eq!(m.sys.host_calls, 1);
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
        assert_eq!(m.sys.stats.total.cpu_runs, cpu_runs);
    }
}

#[test]
fn an_external_interrupt_still_returns_after_output_and_resumes_without_replay() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for jit in [false, true] {
        let mut r = Registry::default();
        r.add("test", "present", Conv::Stdcall { argc: 0 }, |env| {
            env.sys.complete_frame();
            env.ext
                .get::<Arc<AtomicBool>>()
                .store(true, Ordering::Release);
            Outcome::Return(0)
        });
        let mut m = setup(JitCpu::new(PARK, jit).unwrap(), r);
        m.set_frame_handoff(false);
        let flag = Arc::new(AtomicBool::new(false));
        m.cpu.executor_mut().set_host_interrupt_flag(flag.clone());
        m.ext.insert(flag.clone());
        let present = m.resolve_import("test", "present").unwrap();
        let mut a = Asm::new(CODE);
        a.call(present).mov_mem_imm(DATA, 99).ret();
        m.cpu.write(CODE, &a.bytes).unwrap();
        if jit {
            compile(&mut m.cpu, CODE, a.bytes.len());
        }
        m.spawn("producer", CODE, &[], 65536).unwrap();
        assert!(matches!(
            m.run_slice(Duration::from_millis(200)),
            SliceResult::Yielded
        ));
        assert_eq!(m.sys.scheduling.external_interrupts, 1);
        assert_eq!(m.sys.scheduling.frame_host_yields, 0);
        flag.store(false, Ordering::Release);
        assert!(matches!(
            m.run_slice(Duration::from_millis(200)),
            SliceResult::Complete(RunResult::Exited(0))
        ));
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 99);
        assert_eq!(m.sys.completed_frames, 1);
        assert_eq!(m.sys.host_calls, 1);
    }
}

#[test]
#[ignore = "diagnostic: identical completed-frame work, compile/setup excluded"]
fn report_equal_frame_work() {
    for round in 0..3 {
        for handoff in [true, false, true] {
            let mut times = Vec::new();
            for _ in 0..9 {
                let (_, time) = burst(
                    JitCpu::new(PARK, true).unwrap(),
                    compile,
                    handoff,
                    false,
                    2000,
                    false,
                );
                times.push(time.as_nanos());
            }
            times.sort_unstable();
            println!(
                "FRAME_HANDOFF round={round} required={handoff} frames=2000 median_ns={} samples={times:?}",
                times[4]
            );
        }
    }
}
