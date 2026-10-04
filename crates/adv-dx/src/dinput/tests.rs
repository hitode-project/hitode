use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};
use adv_machine::{Clock, Machine, ObjectLimits, Objects, layout::PARK_ADDR};

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    adv_win32::Win32::install(
        &mut machine,
        adv_win32::Win32Config::new(".", "input-fixture.exe", "target/input-fixture-tests"),
    )
    .unwrap();
    machine.spawn("authored", PARK_ADDR, &[], 65536).unwrap();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write(data, &IID_A).unwrap();
    machine.cpu.write(data + 16, &IID_W).unwrap();
    machine
        .cpu
        .write(data + 32, &adv_os::com::IUNKNOWN)
        .unwrap();
    (machine, data)
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn factory(machine: &mut Machine<JitCpu>, data: u32, wide: bool) -> u32 {
    assert_eq!(
        create(
            &mut env(machine),
            Handle(0x400000),
            0x800,
            GuestPtr::new(data + u32::from(wide) * 16),
            GuestPtr::new(data + 128),
            GuestPtr::new(0)
        ),
        0
    );
    machine.cpu.read_u32(data + 128).unwrap()
}

#[test]
fn a_w_share_one_iunknown_count_and_release_both_guest_aliases() {
    for wide in [false, true] {
        let (mut machine, data) = fixture();
        let before = machine.sys.objects.object_count();
        let pointer = factory(&mut machine, data, wide);
        assert_ne!(pointer, 0);
        assert_eq!(machine.sys.objects.object_count(), before + 1);
        assert_eq!(
            query(
                &mut env(&mut machine),
                pointer,
                GuestPtr::new(data + u32::from(!wide) * 16),
                GuestPtr::new(data + 132)
            ),
            0
        );
        let other = machine.cpu.read_u32(data + 132).unwrap();
        assert_ne!(other, pointer);
        assert_ne!(
            machine.cpu.read_u32(other).unwrap(),
            machine.cpu.read_u32(pointer).unwrap()
        );
        assert_eq!(
            query(
                &mut env(&mut machine),
                other,
                GuestPtr::new(data + 32),
                GuestPtr::new(data + 136)
            ),
            0
        );
        assert_eq!(machine.cpu.read_u32(data + 136).unwrap(), pointer);
        assert_eq!(add_ref(&mut env(&mut machine), other), 4);
        assert_eq!(release(&mut env(&mut machine), other), 3);
        assert_eq!(release(&mut env(&mut machine), pointer), 2);
        assert_eq!(release(&mut env(&mut machine), other), 1);
        assert_eq!(release(&mut env(&mut machine), pointer), 0);
        assert_eq!(machine.sys.objects.object_count(), before);
        let com = machine.ext.get::<State>().com;
        assert!(com.get::<Factory>(&machine.sys.objects, pointer).is_none());
        assert!(com.get::<Factory>(&machine.sys.objects, other).is_none());
        let heap = machine.ext.get_mut::<Heaps>();
        assert_eq!(heap.size(heap.process_heap, pointer), None);
        assert_eq!(heap.size(heap.process_heap, other), None);
    }
}

#[test]
fn only_empty_controller_enumeration_succeeds_and_requires_a_valid_callback() {
    let (mut machine, data) = fixture();
    let pointer = factory(&mut machine, data, false);
    let callback = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RX)
        .unwrap();
    machine
        .cpu
        .write(callback, &[0x31, 0xc0, 0xc2, 8, 0])
        .unwrap();
    for flags in [0, 1, 0x101, 0x70101] {
        assert_eq!(
            enumerate(
                &mut env(&mut machine),
                pointer,
                4,
                GuestPtr::new(callback),
                flags
            ),
            0
        );
    }
    for class in [0, 1, 2, 3, 0x12, 0x13] {
        assert_eq!(
            enumerate(
                &mut env(&mut machine),
                pointer,
                class,
                GuestPtr::new(callback),
                1
            ),
            UNSUPPORTED
        );
    }
    assert_eq!(
        enumerate(&mut env(&mut machine), pointer, 4, GuestPtr::new(0), 1),
        INVALID
    );
    assert_eq!(
        enumerate(&mut env(&mut machine), pointer, 4, GuestPtr::new(data), 1),
        INVALID
    );
    assert_eq!(
        enumerate(
            &mut env(&mut machine),
            pointer,
            4,
            GuestPtr::new(callback),
            2
        ),
        INVALID
    );
    assert_eq!(release(&mut env(&mut machine), pointer), 0);
    assert_eq!(
        enumerate(
            &mut env(&mut machine),
            pointer,
            4,
            GuestPtr::new(callback),
            1
        ),
        INVALID
    );
}

#[test]
fn unknown_iids_readonly_outputs_and_second_alias_exhaustion_publish_nothing() {
    let (mut machine, data) = fixture();
    machine.cpu.write(data + 64, &[0x42; 16]).unwrap();
    machine.cpu.write_u32(data + 128, 0xa5a5a5a5).unwrap();
    assert_eq!(
        create(
            &mut env(&mut machine),
            Handle(0x400000),
            0x800,
            GuestPtr::new(data + 64),
            GuestPtr::new(data + 128),
            GuestPtr::new(0)
        ),
        NO_INTERFACE
    );
    assert_eq!(machine.cpu.read_u32(data + 128).unwrap(), 0);
    assert!(machine.ext.try_get::<State>().is_none());
    machine.cpu.write_u32(data + 4096, 0xa5a5a5a5).unwrap();
    machine.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        create(
            &mut env(&mut machine),
            Handle(0x400000),
            0x800,
            GuestPtr::new(data),
            GuestPtr::new(data + 4096),
            GuestPtr::new(0)
        ),
        INVALID
    );
    assert_eq!(machine.cpu.read_u32(data + 4096).unwrap(), 0xa5a5a5a5);
    machine.sys.objects = Objects::with_limits(ObjectLimits {
        aliases: 1,
        ..Default::default()
    });
    assert!(initialize(&mut env(&mut machine)));
    let probe = alloc(&mut env(&mut machine), 4).unwrap();
    free(&mut env(&mut machine), probe);
    assert_eq!(
        create(
            &mut env(&mut machine),
            Handle(0x400000),
            0x800,
            GuestPtr::new(data),
            GuestPtr::new(data + 128),
            GuestPtr::new(0)
        ),
        OUT_OF_MEMORY
    );
    assert_eq!(machine.cpu.read_u32(data + 128).unwrap(), 0);
    assert_eq!(machine.sys.objects.object_count(), 0);
    assert_eq!(
        machine
            .sys
            .objects
            .alias_count(machine.ext.get::<State>().com.namespace()),
        0
    );
    let after = alloc(&mut env(&mut machine), 4).unwrap();
    assert_eq!(
        after, probe,
        "both unexposed interface allocations were freed"
    );
    free(&mut env(&mut machine), after);
}
