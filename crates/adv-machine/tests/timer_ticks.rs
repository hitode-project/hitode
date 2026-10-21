//! Timer ticks (PROC-CLOCK-6): in real time, timed waits expire on whole
//! milliseconds, so deadlines inside one tick share a host wake.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, Wait};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;

fn target(now: u64) -> u64 {
    (now / 1000 + 5) * 1000 + 500
}

/// A context waiting until 5.5 ms past the current millisecond.
fn machine(exact: bool) -> Machine<UnicornCpu> {
    let mut registry = Registry::default();
    registry.add("test", "tick", Conv::Stdcall { argc: 0 }, |e| {
        Outcome::block_then_return(Wait::Until(target(e.sys.clock.now_us())), 0)
    });
    registry.add("test", "exact", Conv::Stdcall { argc: 0 }, |e| {
        e.sys.keep_exact_deadline();
        Outcome::block_then_return(Wait::Until(target(e.sys.clock.now_us())), 0)
    });
    let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::real()).unwrap();
    let wait = m
        .resolve_import("test", if exact { "exact" } else { "tick" })
        .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let mut code = Asm::new(CODE);
    code.call(wait).ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("waiter", CODE, &[], 65536).unwrap();
    m
}

fn idle_deadline(m: &mut Machine<UnicornCpu>) -> u64 {
    match m.run() {
        RunResult::Idle {
            next_deadline: Some(at),
        } => at,
        other => panic!("expected a timed idle, got {other:?}"),
    }
}

// spec: PROC-CLOCK-6
#[test]
fn timed_waits_expire_on_the_next_whole_millisecond() {
    let mut m = machine(false);
    let at = idle_deadline(&mut m);
    assert_eq!(at % 1000, 0, "{at}");
    assert!(at > m.sys.clock.now_us() + 4500, "{at}");
}

// spec: PROC-CLOCK-6
#[test]
fn display_paced_waits_keep_their_exact_deadline() {
    let mut m = machine(true);
    assert_eq!(idle_deadline(&mut m) % 1000, 500);
}
