//! Wait-set acquisition happens when the set becomes ready, before its
//! continuation runs. These checks cover reset/reacquisition races and WaitAll.
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{
    Clock, Conv, Machine, Object, Outcome, Registry, Resume, RunResult, Wait, WakeReason,
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x500000;

fn fixture(
    all: bool,
    timeout: Option<u64>,
    notification: bool,
) -> (Machine<UnicornCpu>, Vec<u32>, Option<u32>) {
    let mut registry = Registry::default();
    registry.add("test", "wait", Conv::Stdcall { argc: 3 }, |env| {
        let handles = (0..9)
            .map(|i| env.cpu.read_u32(DATA + i * 4).unwrap())
            .collect();
        let all = env.arg(0) != 0;
        let deadline = (env.arg(1) != 0).then(|| env.sys.clock.now_us() + u64::from(env.arg(1)));
        let notification = (env.arg(2) != 0).then_some(env.arg(2));
        Outcome::block(
            Wait::Objects {
                handles,
                all,
                deadline,
                notification,
            },
            |_, wake| {
                Outcome::Return(match wake {
                    Resume::Woke(WakeReason::Objects { index, abandoned }) => {
                        index + if abandoned { 0x80 } else { 0 }
                    }
                    Resume::Woke(WakeReason::Notified) => 0x103,
                    Resume::Woke(WakeReason::Timeout) => 0x102,
                    _ => panic!("unexpected wait-set resume: {wake:?}"),
                })
            },
        )
    });
    let mut m = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let handles: Vec<_> = (0..9)
        .map(|_| {
            m.sys.objects.insert(Object::Event {
                manual_reset: false,
                signaled: false,
            })
        })
        .collect();
    let notification = notification.then(|| {
        m.sys.objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        })
    });
    for (i, &h) in handles.iter().enumerate() {
        m.cpu.write_u32(DATA + i as u32 * 4, h).unwrap();
    }
    let wait = m.resolve_import("test", "wait").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(notification.unwrap_or(0))
        .push_imm(timeout.unwrap_or(0) as u32)
        .push_imm(u32::from(all))
        .call(wait)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("waiter", CODE, &[], 65536).unwrap();
    assert!(
        matches!(m.run(), RunResult::Idle { next_deadline } if next_deadline == timeout.map(|t| m.sys.clock.now_us() + t))
    );
    (m, handles, notification)
}

#[test]
fn nine_object_wait_keeps_grant_after_reset_and_later_wait() {
    let (mut m, handles, _) = fixture(false, None, false);
    m.sys.set_event(handles[7]);
    assert_eq!(m.sys.context(0).state_name(), "Ready");
    assert!(!m.sys.try_acquire(handles[7]));
    m.sys.reset_event(handles[7]);
    m.sys.set_event(handles[2]);
    assert!(
        matches!(m.run(), RunResult::Exited(7)),
        "a later signal cannot replace the original grant"
    );
    assert!(
        m.sys.try_acquire(handles[2]),
        "unselected signals are retained"
    );
}

#[test]
fn wait_all_does_not_consume_partial_sets_then_grants_atomically() {
    let (mut m, handles, _) = fixture(true, None, false);
    for &h in &handles[..8] {
        m.sys.set_event(h);
    }
    assert_eq!(m.sys.context(0).state_name(), "Blocked");
    assert!(
        m.sys.try_acquire(handles[0]),
        "partial WaitAll cannot reserve an auto-reset signal"
    );
    m.sys.set_event(handles[8]);
    assert_eq!(m.sys.context(0).state_name(), "Blocked");
    m.sys.set_event(handles[0]);
    assert_eq!(m.sys.context(0).state_name(), "Ready");
    for &h in &handles {
        assert!(!m.sys.try_acquire(h));
        m.sys.reset_event(h);
    }
    assert!(matches!(m.run(), RunResult::Exited(0)));
}

#[test]
fn wait_any_uses_lowest_index_and_wait_all_reports_abandoned_mutex() {
    let (mut m, handles, _) = fixture(false, None, false);
    for i in [7, 2] {
        if let Some(Object::Event { signaled, .. }) = m.sys.objects.get_mut(handles[i]) {
            *signaled = true;
        }
    }
    assert!(matches!(m.run(), RunResult::Exited(2)));
    assert!(m.sys.try_acquire(handles[7]));

    let (mut m, handles, _) = fixture(true, None, false);
    let mut owner = Asm::new(CODE + 0x100);
    owner.ret();
    m.cpu.write(CODE + 0x100, &owner.bytes).unwrap();
    let owner = m.spawn("mutex owner", CODE + 0x100, &[], 65536).unwrap();
    assert!(matches!(
        m.run(),
        RunResult::Idle {
            next_deadline: None
        }
    ));
    assert!(m.sys.context(owner).is_done());
    *m.sys.objects.get_mut(handles[3]).unwrap() = Object::Mutex {
        owner: Some(owner),
        recursion: 1,
    };
    for (i, &h) in handles.iter().enumerate() {
        if i != 3 {
            m.sys.set_event(h);
        }
    }
    assert!(matches!(m.run(), RunResult::Exited(0x83)));
    assert!(matches!(
        m.sys.objects.get(handles[3]),
        Some(Object::Mutex {
            owner: Some(0),
            recursion: 1
        })
    ));
}

#[test]
fn wait_set_timeout_and_invalidation_do_not_consume_partial_objects() {
    let (mut m, handles, _) = fixture(true, Some(7_000), false);
    m.sys.set_event(handles[1]);
    let start = m.sys.clock.now_us();
    assert!(matches!(m.run_virtual(10_000), RunResult::Exited(0x102)));
    assert_eq!(m.sys.clock.now_us() - start, 7_000);
    assert!(m.sys.try_acquire(handles[1]));

    let (mut m, handles, notification) = fixture(true, None, true);
    m.sys.set_event(handles[1]);
    m.sys.set_event(notification.unwrap());
    assert!(matches!(m.run(), RunResult::Exited(0x103)));
    assert!(m.sys.try_acquire(handles[1]));
}

#[test]
fn wait_set_observes_thread_exit_and_semaphore_release() {
    let (mut m, handles, _) = fixture(false, None, false);
    let worker = m.spawn("worker", CODE + 0x100, &[], 65536).unwrap();
    m.cpu.write(CODE + 0x100, &[0xc3]).unwrap();
    *m.sys.objects.get_mut(handles[4]).unwrap() = Object::Thread { ctx: worker };
    assert!(matches!(m.run(), RunResult::Exited(4)));

    let (mut m, handles, _) = fixture(false, None, false);
    *m.sys.objects.get_mut(handles[5]).unwrap() = Object::Semaphore { count: 1, max: 3 };
    m.sys.signal_waiters(handles[5]);
    assert!(!m.sys.try_acquire(handles[5]));
    assert!(matches!(m.run(), RunResult::Exited(5)));
}

#[test]
fn one_yield_before_signal_does_not_jump_virtual_time_to_waiter_timeout() {
    let mut registry = Registry::default();
    registry.add("test", "yield", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::block_then_return(Wait::Yield, 0)
    });
    registry.add("test", "wait", Conv::Stdcall { argc: 1 }, |e| {
        Outcome::block(
            Wait::Object {
                handle: e.arg(0),
                deadline: Some(e.sys.clock.now_us() + 1_000_000),
            },
            |_, wake| {
                Outcome::Return(if matches!(wake, Resume::Woke(WakeReason::Signaled)) {
                    0
                } else {
                    0x102
                })
            },
        )
    });
    registry.add("test", "signal", Conv::Stdcall { argc: 1 }, |e| {
        e.sys.set_event(e.arg(0));
        Outcome::Return(0)
    });
    let mut m = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let event = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let wait = m.resolve_import("test", "wait").unwrap();
    let signal = m.resolve_import("test", "signal").unwrap();
    let yield_ = m.resolve_import("test", "yield").unwrap();
    let mut main = Asm::new(CODE);
    main.push_imm(event).call(wait).ret();
    let mut worker = Asm::new(CODE + 0x100);
    worker.call(yield_).push_imm(event).call(signal).ret();
    m.cpu.write(CODE, &main.bytes).unwrap();
    m.cpu.write(CODE + 0x100, &worker.bytes).unwrap();
    m.spawn("main", CODE, &[], 65536).unwrap();
    m.spawn("worker", CODE + 0x100, &[], 65536).unwrap();
    let start = m.sys.clock.now_us();
    assert!(matches!(m.run(), RunResult::Exited(0)));
    assert_eq!(
        m.sys.clock.now_us(),
        start,
        "yielding only hands off the CPU; the worker signals without a timed wait"
    );
}
