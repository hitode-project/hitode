//! Software frame cap (DEV-FRAME-9): pacing decisions and the held poll.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot, Reg},
};
use adv_machine::frame_spin::FramePacer;
use adv_machine::{Clock, Machine};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const VSYNC_US: u64 = 9_000;
const SCREEN: (u64, u64) = (921_600, 921_600);

#[derive(Default)]
struct Pacer {
    held: AtomicBool,
}
impl FramePacer for Pacer {
    fn until_vsync(&self) -> Option<Duration> {
        Some(Duration::from_micros(VSYNC_US))
    }
    fn input_held(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }
}

fn fixture(pacer: Option<Arc<Pacer>>) -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "frame-cap.exe", "target/frame-cap-contract-tests"),
    )
    .unwrap();
    for name in ["drawing", "other"] {
        machine
            .spawn(name, adv_machine::layout::PARK_ADDR, &[], 65536)
            .unwrap();
    }
    let memory = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, memory);
    machine.sys.current = 0;
    machine.sys.frame_pacer = pacer.map(|p| p as Arc<dyn FramePacer>);
    (machine, memory)
}
fn env(machine: &mut Machine<JitCpu>, context: u32) -> Env<'_> {
    Env {
        context,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn frames(machine: &mut Machine<JitCpu>, coverage: (u64, u64), count: usize) {
    for _ in 0..count {
        frame_cap::published(&mut env(machine, 0), coverage);
    }
}
const ALL: Filter = Filter {
    hwnd: 0,
    min: 0,
    max: 0,
};

// spec: DEV-FRAME-9
#[test]
fn a_large_frame_within_the_period_holds_the_next_empty_poll_once() {
    let (mut machine, out) = fixture(Some(Arc::default()));
    frames(&mut machine, SCREEN, 1);
    assert_eq!(
        frame_cap::take(&mut env(&mut machine, 0)),
        None,
        "first frame"
    );
    frames(&mut machine, SCREEN, 1);
    // Another context's poll neither takes nor drops the hold.
    assert_eq!(frame_cap::take(&mut env(&mut machine, 1)), None);
    let now = machine.sys.clock.now_us();
    match peek_message(&mut env(&mut machine, 0), out, ALL, true, false) {
        Outcome::Block {
            wait:
                Wait::Object {
                    handle,
                    deadline: Some(deadline),
                },
            ..
        } => {
            assert_eq!(deadline, now + VSYNC_US);
            assert_eq!(handle, user(&env(&mut machine, 0)).queues[&0].event);
        }
        _ => panic!("expected a hold on the message queue"),
    }
    // The hold is spent: the next empty poll yields as before.
    assert!(matches!(
        peek_message(&mut env(&mut machine, 0), out, ALL, true, false),
        Outcome::Block {
            wait: Wait::Yield,
            ..
        }
    ));
}

// spec: DEV-FRAME-9
#[test]
fn small_frames_held_input_and_no_frame_clock_never_hold() {
    let (mut machine, _) = fixture(Some(Arc::default()));
    frames(&mut machine, (921_600 / 4 - 1, 921_600), 3);
    assert_eq!(frame_cap::take(&mut env(&mut machine, 0)), None, "small");

    let pacer = Arc::new(Pacer::default());
    pacer.held.store(true, Ordering::Relaxed);
    let (mut machine, _) = fixture(Some(pacer));
    frames(&mut machine, SCREEN, 3);
    assert_eq!(frame_cap::take(&mut env(&mut machine, 0)), None, "held");

    let (mut machine, _) = fixture(None);
    frames(&mut machine, SCREEN, 3);
    assert_eq!(frame_cap::take(&mut env(&mut machine, 0)), None, "headless");
}
