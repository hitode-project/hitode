//! Frame spins (PROC-CLOCK-4): synthetic polling loops around a frame.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, Prot};
use adv_machine::frame_spin::FramePacer;
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, Wait};
use adv_unicorn::UnicornCpu;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const CODE: u32 = 0x0040_0000;
const VSYNC: Duration = Duration::from_millis(12);

#[derive(Default)]
struct Pacer {
    held: AtomicBool,
}

impl FramePacer for Pacer {
    fn until_vsync(&self) -> Option<Duration> {
        Some(VSYNC)
    }
    fn input_held(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }
}

enum Loop {
    /// A frame, then nothing but polls.
    Spin,
    /// A frame before every poll.
    DrawEach,
    /// A frame, then polls with 600 µs of work between them.
    Work,
}

fn machine(shape: Loop, pacer: Option<Arc<Pacer>>) -> Machine<UnicornCpu> {
    let mut registry = Registry::default();
    registry.add("test", "frame", Conv::Stdcall { argc: 0 }, |e| {
        e.sys.complete_frame();
        Outcome::Return(0)
    });
    registry.add("test", "peek", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::block_then_return(Wait::Yield, 0)
    });
    registry.add("test", "work", Conv::Stdcall { argc: 0 }, |_| {
        std::thread::sleep(Duration::from_micros(600));
        Outcome::Return(0)
    });
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::real()).unwrap();
    m.set_execution_budget(Some(Duration::from_secs(2)));
    m.sys.frame_pacer = pacer.map(|p| p as Arc<dyn FramePacer>);
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let frame = m.resolve_import("test", "frame").unwrap();
    let peek = m.resolve_import("test", "peek").unwrap();
    let work = m.resolve_import("test", "work").unwrap();
    let mut code = Asm::new(CODE);
    match shape {
        Loop::Spin => {
            code.call(frame);
            let poll = code.here();
            code.call(peek).jmp(poll);
        }
        Loop::DrawEach => {
            code.call(frame).call(peek).jmp(CODE);
        }
        Loop::Work => {
            code.call(frame);
            let poll = code.here();
            code.call(peek).call(work).jmp(poll);
        }
    }
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("main", CODE, &[], 65536).unwrap();
    m
}

/// How far ahead of the clock each of `rounds` polling rounds waits.
fn waits(m: &mut Machine<UnicornCpu>, rounds: usize) -> Vec<Duration> {
    (0..rounds)
        .map(|_| match m.run() {
            RunResult::Idle {
                next_deadline: Some(deadline),
            } => Duration::from_micros(deadline.saturating_sub(m.sys.clock.now_us())),
            other => panic!("expected a polling round, got {other:?}"),
        })
        .collect()
}

fn long(wait: &Duration) -> bool {
    *wait > Duration::from_millis(5)
}

// spec: PROC-CLOCK-4
#[test]
fn a_spin_after_a_frame_waits_once_for_the_vsync() {
    let mut m = machine(Loop::Spin, Some(Arc::default()));
    let waits = waits(&mut m, 4);
    // The round right after the frame is ordinary; the next spins.
    let long: Vec<_> = waits.iter().map(long).collect();
    assert_eq!(long, [false, true, false, false], "{waits:?}");
    assert!(waits[1] <= VSYNC, "{waits:?}");
    assert_eq!(m.sys.scheduling.frame_spin_waits, 1);
}

// spec: PROC-CLOCK-4
#[test]
fn loops_that_draw_or_work_between_polls_keep_millisecond_rounds() {
    for shape in [Loop::DrawEach, Loop::Work] {
        let mut m = machine(shape, Some(Arc::default()));
        let waits = waits(&mut m, 5);
        assert!(!waits.iter().any(long), "{waits:?}");
        assert_eq!(m.sys.scheduling.frame_spin_waits, 0);
    }
}

// spec: PROC-CLOCK-4
#[test]
fn held_input_or_no_host_frame_clock_keeps_millisecond_rounds() {
    let held = Arc::new(Pacer::default());
    held.held.store(true, Ordering::Relaxed);
    for pacer in [Some(held), None] {
        let mut m = machine(Loop::Spin, pacer);
        let waits = waits(&mut m, 4);
        assert!(!waits.iter().any(long), "{waits:?}");
        assert_eq!(m.sys.scheduling.frame_spin_waits, 0);
    }
}
