//! Event grants belong to blocked waits, not to a later scheduling poll.
//! https://learn.microsoft.com/en-us/windows/win32/api/synchapi/nf-synchapi-setevent
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Object, Outcome, Resume, RunResult, Wait, WakeReason};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;

fn blocked_waiters(manual: bool, count: usize) -> (Machine<UnicornCpu>, u32) {
    let mut registry = Registry::default();
    registry.add("test", "wait", Conv::Stdcall { argc: 1 }, |env| {
        let handle = env.arg(0);
        if env.sys.try_acquire(handle) {
            return Outcome::Return(0);
        }
        Outcome::block(
            Wait::Object {
                handle,
                deadline: None,
            },
            |_, wake| {
                assert!(matches!(wake, Resume::Woke(WakeReason::Signaled)));
                Outcome::Return(0)
            },
        )
    });
    let mut machine = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let event = machine.sys.objects.insert(Object::Event {
        manual_reset: manual,
        signaled: false,
    });
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let wait = machine.resolve_import("test", "wait").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(event).call(wait).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    for _ in 0..count {
        machine.spawn("waiter", CODE, &[], 65536).unwrap();
    }
    if count != 0 {
        assert!(matches!(
            machine.run(),
            RunResult::Idle {
                next_deadline: None
            }
        ));
        assert!(
            machine
                .sys
                .contexts
                .iter()
                .all(|context| context.state_name() == "Blocked")
        );
    }
    (machine, event)
}

#[test]
fn auto_reset_grant_cannot_be_stolen_or_revoked_before_waiter_resumes() {
    let (mut machine, event) = blocked_waiters(false, 2);
    assert!(machine.sys.set_event(event));
    let released = machine
        .sys
        .contexts
        .iter()
        .filter(|context| context.state_name() == "Ready")
        .count();
    assert_eq!(released, 1, "one already blocked wait must own the signal");
    assert!(
        !machine.sys.try_acquire(event),
        "a later wait cannot take the existing grant"
    );
    assert!(machine.sys.reset_event(event));
    assert_eq!(machine.sys.context(0).state_name(), "Ready");
    assert!(matches!(machine.run(), RunResult::Exited(0)));
}

#[test]
fn manual_reset_releases_all_existing_waiters_even_if_reset_before_they_run() {
    let (mut machine, event) = blocked_waiters(true, 3);
    assert!(machine.sys.set_event(event));
    assert!(
        machine
            .sys
            .contexts
            .iter()
            .all(|context| context.state_name() == "Ready")
    );
    assert!(machine.sys.reset_event(event));
    assert!(
        !machine.sys.try_acquire(event),
        "a later wait observes the reset state"
    );
    assert!(
        machine
            .sys
            .contexts
            .iter()
            .all(|context| context.state_name() == "Ready")
    );
    assert!(matches!(machine.run(), RunResult::Exited(0)));
}

#[test]
fn auto_reset_without_waiters_stores_one_signal_and_does_not_count_repeated_sets() {
    let (mut machine, event) = blocked_waiters(false, 0);
    assert!(machine.sys.set_event(event));
    assert!(machine.sys.set_event(event));
    assert!(machine.sys.try_acquire(event));
    assert!(!machine.sys.try_acquire(event));
}

#[test]
fn two_auto_reset_signals_can_release_two_waiters_before_either_is_scheduled() {
    let (mut machine, event) = blocked_waiters(false, 3);
    assert!(machine.sys.set_event(event));
    assert!(machine.sys.set_event(event));
    assert_eq!(
        machine
            .sys
            .contexts
            .iter()
            .filter(|context| context.state_name() == "Ready")
            .count(),
        2
    );
    assert!(!machine.sys.try_acquire(event));
}
