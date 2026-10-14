use super::*;
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot},
};
use adv_machine::{
    Clock, Machine, Resume, Wait, WakeReason,
    layout::{PARK_ADDR, teb},
};
use std::{cell::Cell, rc::Rc};

fn fixture() -> Machine<JitCpu> {
    fixture_with(false)
}

fn fixture_with(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(
            "/nonexistent/heap-lock",
            "fixture.exe",
            "/nonexistent/heap-lock-data",
        ),
    )
    .unwrap();
    for name in ["owner", "waiter"] {
        machine.spawn(name, PARK_ADDR, &[], 65536).unwrap();
    }
    machine
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

fn returned(result: Outcome, value: u32) {
    assert!(matches!(result, Outcome::Return(actual) if actual == value));
}

fn recursion(machine: &Machine<JitCpu>, heap: u32) -> u32 {
    let lock = machine
        .ext
        .get::<Heaps>()
        .synchronization(heap)
        .unwrap()
        .lock
        .unwrap();
    machine.cpu.read_u32(lock + 8).unwrap()
}

#[test]
fn heap_gates_wait_recurse_release_before_callbacks_and_keep_failed_destroy_balanced() {
    let mut machine = fixture();
    let heap = machine.ext.get::<Heaps>().process_heap;
    let marker = 0x13572468;
    machine
        .cpu
        .write_u32(machine.sys.contexts[1].teb + teb::LAST_ERROR, marker)
        .unwrap();
    returned(lock(&mut env(&mut machine, 0), heap), 1);
    returned(lock(&mut env(&mut machine, 0), heap), 1);
    assert_eq!(recursion(&machine, heap), 2);
    let entered = Rc::new(Cell::new(false));
    let observed = entered.clone();
    let result = with_heap(&mut env(&mut machine, 1), heap, 0, move |env| {
        observed.set(true);
        let lock = env
            .ext
            .get::<Heaps>()
            .synchronization(heap)
            .unwrap()
            .lock
            .unwrap();
        assert_eq!(env.cpu.read_u32(lock + 12).unwrap(), env.ctx().tid);
        Outcome::call(PARK_ADDR, vec![], |_, _| Outcome::Return(0))
    });
    let Outcome::Block {
        wait: Wait::Object { handle: event, .. },
        then,
        ..
    } = result
    else {
        panic!("another owner must park heap work")
    };
    assert!(!entered.get());
    assert!(matches!(
        destroy(&mut env(&mut machine, 0), heap),
        Outcome::Fail(_)
    ));
    assert_eq!(
        recursion(&machine, heap),
        2,
        "failed destroy releases its temporary hold"
    );
    assert!(machine.cpu.accessible(heap, 4096, Prot::RW));
    returned(unlock(&mut env(&mut machine, 0), heap), 1);
    assert_eq!(recursion(&machine, heap), 1);
    assert!(!machine.sys.try_acquire(event));
    returned(unlock(&mut env(&mut machine, 0), heap), 1);
    assert!(machine.sys.try_acquire(event));
    let result = then(
        &mut env(&mut machine, 1),
        Resume::Woke(WakeReason::Signaled),
    );
    assert!(entered.get());
    assert!(matches!(result, Outcome::Call { .. }));
    assert_eq!(
        recursion(&machine, heap),
        0,
        "temporary hold does not span a guest callback"
    );
    assert_eq!(
        machine
            .cpu
            .read_u32(machine.sys.contexts[1].teb + teb::LAST_ERROR)
            .unwrap(),
        marker
    );
    returned(destroy(&mut env(&mut machine, 0), heap), 1);
    assert!(machine.ext.get::<Heaps>().synchronization(heap).is_none());
    assert!(!machine.cpu.accessible(heap, 4096, Prot::RW));
    assert!(machine.sys.objects.get(event).is_none());
}

#[test]
fn heap_owners_are_independent_and_no_serialize_bypasses_only_the_automatic_gate() {
    let mut machine = fixture();
    let heap = machine.ext.get::<Heaps>().process_heap;
    let other = machine
        .ext
        .get_mut::<Heaps>()
        .create(&mut machine.cpu, &mut machine.sys.vm)
        .unwrap();
    let unprotected = machine
        .ext
        .get_mut::<Heaps>()
        .create_with_serialization(&mut machine.cpu, &mut machine.sys.vm, false)
        .unwrap();
    returned(lock(&mut env(&mut machine, 0), heap), 1);
    returned(lock(&mut env(&mut machine, 1), other), 1);
    returned(
        with_heap(&mut env(&mut machine, 1), heap, 1, |_| Outcome::Return(17)),
        17,
    );
    assert_eq!(recursion(&machine, heap), 1);
    returned(lock(&mut env(&mut machine, 0), unprotected), 1);
    returned(
        with_heap(&mut env(&mut machine, 1), unprotected, 0, |_| {
            Outcome::Return(23)
        }),
        23,
    );
    assert_eq!(recursion(&machine, unprotected), 1);
    returned(unlock(&mut env(&mut machine, 1), heap), 0);
    assert_eq!(
        recursion(&machine, heap),
        1,
        "another thread cannot release ownership"
    );
    returned(unlock(&mut env(&mut machine, 0), heap), 1);
    returned(unlock(&mut env(&mut machine, 1), other), 1);
    returned(unlock(&mut env(&mut machine, 0), unprotected), 1);
    returned(destroy(&mut env(&mut machine, 0), other), 1);
    returned(destroy(&mut env(&mut machine, 0), unprotected), 1);
}

#[test]
// spec: WIN32-OBJECT-9
fn heap_lock_waits_for_preempted_guest_operations_before_returning() {
    for jit in [false, true] {
        let mut machine = fixture_with(jit);
        let heap = machine.ext.get::<Heaps>().process_heap;
        machine.cpu.write_u32(heap + 0x108, 1).unwrap();
        let Outcome::Block {
            wait: Wait::Yield,
            then,
            ..
        } = lock(&mut env(&mut machine, 0), heap)
        else {
            panic!("must drain the earlier operation");
        };
        assert_eq!(machine.cpu.read_u32(heap + 0x104).unwrap(), 1);
        assert_eq!(recursion(&machine, heap), 0);
        machine.cpu.write_u32(heap + 0x108, 0).unwrap();
        returned(
            then(
                &mut env(&mut machine, 0),
                Resume::Woke(WakeReason::Signaled),
            ),
            1,
        );
        assert_eq!(recursion(&machine, heap), 1);
        returned(unlock(&mut env(&mut machine, 0), heap), 1);
        assert_eq!(machine.cpu.read_u32(heap + 0x104).unwrap(), 1);
    }
}

#[test]
// spec: WIN32-OBJECT-9
fn private_heap_destruction_drains_lookup_pins_before_unregistering() {
    for jit in [false, true] {
        let mut machine = fixture_with(jit);
        let root = machine.ext.get::<Heaps>().process_heap;
        let heaps = machine.ext.get_mut::<Heaps>();
        let heap = heaps.create(&mut machine.cpu, &mut machine.sys.vm).unwrap();
        assert!(heaps.enable_small_heap(&mut machine.cpu, heap).unwrap());
        machine.cpu.write_u32(root + 0xb14, 1).unwrap();
        let Outcome::Block {
            wait: Wait::Yield,
            then,
            ..
        } = destroy(&mut env(&mut machine, 0), heap)
        else {
            panic!("lookup pin must keep heap mapped");
        };
        assert!(machine.cpu.accessible(heap, 4096, Prot::RW));
        assert_eq!(machine.cpu.read_u32(root + 0xb24).unwrap(), heap);
        assert_eq!(machine.cpu.read_u32(root + 0xb10).unwrap(), 1);
        machine.cpu.write_u32(root + 0xb14, 0).unwrap();
        returned(
            then(
                &mut env(&mut machine, 0),
                Resume::Woke(WakeReason::Signaled),
            ),
            1,
        );
        assert!(!machine.cpu.accessible(heap, 4096, Prot::RW));
        assert_eq!(machine.cpu.read_u32(root + 0xb24).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(root + 0xb10).unwrap(), 0);
    }
}
