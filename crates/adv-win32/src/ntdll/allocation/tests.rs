use super::*;
use adv_cpu::{JitCpu, backend::Backend};
use adv_machine::{
    Clock, Machine, Object,
    layout::{GRANULE, PAGE, PARK_ADDR},
};
use adv_unicorn::UnicornCpu;
const DATA: u32 = 0x410000;
const OUTPUT: u32 = DATA + PAGE;
const REMOTE: u32 = 0x22000000;
fn machine<B: Backend>(cpu: B, pid: u32) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    m.sys.set_process_identity(pid).unwrap();
    let path = std::env::temp_dir().join("adv-nt-allocation-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&path, "fixture.exe", &path)).unwrap();
    m.spawn("allocation test", PARK_ADDR, &[], GRANULE).unwrap();
    assert!(m.sys.vm.reserve_at(DATA, 2 * PAGE));
    m.cpu.map(DATA, 2 * PAGE, Prot::RW).unwrap();
    m
}
fn env<B: Backend>(m: &mut Machine<B>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn service<B: Backend>(s: &processes::Services, m: &mut Machine<B>) {
    let processes::Request::Visit { pid, context, task } = s.pop().unwrap() else {
        panic!("expected VM request")
    };
    assert_eq!(pid, m.sys.pid);
    assert_eq!(context, None);
    task(Ok(&mut env(m)));
}
fn lifecycle<B: Backend>(make: impl Fn() -> B) {
    for lose_output in [false, true] {
        let mut parent = machine(make(), 0x1000);
        let mut child = machine(make(), 0x2000);
        let s = processes::Services::default();
        s.register(&parent.sys.process_identity);
        s.register(&child.sys.process_identity);
        parent.ext.insert(s.clone());
        child.ext.insert(s.clone());
        let handle = parent.sys.objects.insert(Object::Process {
            identity: child.sys.process_identity.clone(),
            access: 8,
        });
        parent.cpu.write_u32(OUTPUT, REMOTE).unwrap();
        parent.cpu.write_u32(OUTPUT + 4, 5001).unwrap();
        crate::util::set_last_error(&mut env(&mut parent), 0x13572468);
        let operation = allocate(
            &mut env(&mut parent),
            Handle(handle),
            GuestPtr::new(OUTPUT),
            0,
            GuestPtr::new(OUTPUT + 4),
            0x3000,
            4,
        );
        let Outcome::HostRequest { then } = operation else {
            panic!("allocation must visit the child")
        };
        service(&s, &mut child);
        assert!(child.cpu.accessible(REMOTE, 2 * PAGE, Prot::RW));
        assert!(!parent.cpu.accessible(REMOTE, PAGE, Prot::READ));
        if lose_output {
            parent.cpu.protect(OUTPUT, PAGE, Prot::READ).unwrap();
        }
        let completion = then(&mut env(&mut parent));
        if lose_output {
            let Outcome::HostRequest { then } = completion else {
                panic!("failed publication must visit for rollback")
            };
            service(&s, &mut child);
            assert!(matches!(
                then(&mut env(&mut parent)),
                Outcome::Return(0xc0000005)
            ));
            assert!(child.sys.vm.region(REMOTE).is_none());
            assert!(!child.cpu.accessible(REMOTE, PAGE, Prot::READ));
            assert_eq!(parent.cpu.read_u32(OUTPUT + 4).unwrap(), 5001);
        } else {
            assert!(matches!(completion, Outcome::Return(0)));
            assert_eq!(parent.cpu.read_u32(OUTPUT).unwrap(), REMOTE);
            assert_eq!(parent.cpu.read_u32(OUTPUT + 4).unwrap(), 2 * PAGE);
            free_memory(&mut env(&mut child), REMOTE, 0, 0x8000).unwrap();
        }
        assert!(!s.pending());
        assert_eq!(crate::util::last_error(&env(&mut parent)), 0x13572468);
    }
}
#[test]
fn remote_allocation_isolates_vm_and_rolls_back_lost_outputs() {
    for jit in [false, true] {
        lifecycle(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    lifecycle(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn free_lifecycle<B: Backend>(make: impl Fn() -> B) {
    let mut parent = machine(make(), 0x1000);
    let mut child = machine(make(), 0x2000);
    assert_eq!(child.sys.suspend_context(0), Some(0));
    let s = processes::Services::default();
    s.register(&parent.sys.process_identity);
    s.register(&child.sys.process_identity);
    parent.ext.insert(s.clone());
    child.ext.insert(s.clone());
    let handle = parent.sys.objects.insert(Object::Process {
        identity: child.sys.process_identity.clone(),
        access: 8,
    });
    let denied = parent.sys.objects.insert(Object::Process {
        identity: child.sys.process_identity.clone(),
        access: 16,
    });
    for m in [&mut parent, &mut child] {
        allocate_memory(&mut env(m), REMOTE, 3 * PAGE, 0x3000, 4).unwrap();
        m.cpu.write_u32(REMOTE, 0x24681357).unwrap();
        crate::util::set_last_error(&mut env(m), 0x13572468);
    }

    let mut invoke = |process, address, count, flags, expected| {
        parent.cpu.write_u32(OUTPUT, address).unwrap();
        parent.cpu.write_u32(OUTPUT + 4, count).unwrap();
        let operation = free(
            &mut env(&mut parent),
            Handle(process),
            GuestPtr::new(OUTPUT),
            GuestPtr::new(OUTPUT + 4),
            flags,
        );
        let outcome = match operation {
            Outcome::HostRequest { then } => {
                service(&s, &mut child);
                then(&mut env(&mut parent))
            }
            other => other,
        };
        assert!(matches!(outcome, Outcome::Return(value) if value == expected));
        if expected != 0 {
            assert_eq!(parent.cpu.read_u32(OUTPUT).unwrap(), address);
            assert_eq!(parent.cpu.read_u32(OUTPUT + 4).unwrap(), count);
            assert!(child.cpu.accessible(REMOTE, 3 * PAGE, Prot::RW));
            assert_eq!(child.cpu.read_u32(REMOTE).unwrap(), 0x24681357);
        }
        assert!(!s.pending());
        assert_eq!(crate::util::last_error(&env(&mut parent)), 0x13572468);
        assert_eq!(crate::util::last_error(&env(&mut child)), 0x13572468);
    };
    invoke(0x12345678, REMOTE + 3, 7, 0x4000, 0xc0000008);
    invoke(denied, REMOTE + 3, 7, 0x4000, 0xc0000022);
    invoke(handle, REMOTE + 3, 7, 0, 0xc00000f2);
    invoke(handle, REMOTE + PAGE + 1, 0, 0x4000, 0xc000009f);
    invoke(handle, REMOTE + 2 * PAGE, 2 * PAGE, 0x4000, 0xc000001a);
    invoke(handle, DATA, PAGE, 0x4000, 0xc00000a0);
    invoke(handle, REMOTE + PAGE - 1, 2, 0x4000, 0);
    assert_eq!(parent.cpu.read_u32(OUTPUT).unwrap(), REMOTE);
    assert_eq!(parent.cpu.read_u32(OUTPUT + 4).unwrap(), 2 * PAGE);
    assert!(!child.cpu.accessible(REMOTE, 2 * PAGE, Prot::READ));
    assert!(child.cpu.accessible(REMOTE + 2 * PAGE, PAGE, Prot::RW));
    assert_eq!(child.sys.vm.region(REMOTE), Some(3 * PAGE));
    assert_eq!(parent.cpu.read_u32(REMOTE).unwrap(), 0x24681357);

    allocate_memory(&mut env(&mut child), REMOTE, 2 * PAGE, 0x1000, 4).unwrap();
    assert_eq!(child.cpu.read_u32(REMOTE).unwrap(), 0);
    for (flags, expected_reservation) in [(0x4000, Some(3 * PAGE)), (0x8000, None)] {
        parent.cpu.write_u32(OUTPUT, REMOTE + 1).unwrap();
        parent.cpu.write_u32(OUTPUT + 4, 0).unwrap();
        let Outcome::HostRequest { then } = free(
            &mut env(&mut parent),
            Handle(handle),
            GuestPtr::new(OUTPUT),
            GuestPtr::new(OUTPUT + 4),
            flags,
        ) else {
            panic!("free must visit the suspended child")
        };
        service(&s, &mut child);
        assert!(matches!(then(&mut env(&mut parent)), Outcome::Return(0)));
        assert_eq!(parent.cpu.read_u32(OUTPUT).unwrap(), REMOTE);
        assert_eq!(parent.cpu.read_u32(OUTPUT + 4).unwrap(), 3 * PAGE);
        assert_eq!(child.sys.vm.region(REMOTE), expected_reservation);
        assert!(!child.cpu.accessible(REMOTE, 3 * PAGE, Prot::READ));
        assert!(parent.cpu.accessible(REMOTE, 3 * PAGE, Prot::RW));
        assert_eq!(parent.cpu.read_u32(REMOTE).unwrap(), 0x24681357);
        assert_eq!(crate::util::last_error(&env(&mut parent)), 0x13572468);
        assert_eq!(crate::util::last_error(&env(&mut child)), 0x13572468);
    }
    assert_eq!(child.sys.contexts[0].suspend_count, 1);
    assert!(!s.pending());
}

#[test]
fn remote_free_shares_vm_ownership_and_preserves_caller_memory() {
    for jit in [false, true] {
        free_lifecycle(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    free_lifecycle(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn free_outputs<B: Backend>(make: impl Fn() -> B) {
    for lose_output in [false, true] {
        let mut parent = machine(make(), 0x1000);
        let mut child = machine(make(), 0x2000);
        let s = processes::Services::default();
        parent.ext.insert(s.clone());
        let handle = parent.sys.objects.insert(Object::Process {
            identity: child.sys.process_identity.clone(),
            access: 8,
        });
        allocate_memory(&mut env(&mut child), REMOTE, PAGE, 0x3000, 4).unwrap();
        parent.cpu.write_u32(OUTPUT, REMOTE).unwrap();
        parent.cpu.write_u32(OUTPUT + 4, 0).unwrap();
        crate::util::set_last_error(&mut env(&mut parent), 0x13572468);
        if !lose_output {
            parent.cpu.protect(OUTPUT, PAGE, Prot::READ).unwrap();
        }
        let operation = free(
            &mut env(&mut parent),
            Handle(handle),
            GuestPtr::new(OUTPUT),
            GuestPtr::new(OUTPUT + 4),
            0x8000,
        );
        let outcome = if lose_output {
            let Outcome::HostRequest { then } = operation else {
                panic!("valid outputs must permit the remote release")
            };
            service(&s, &mut child);
            parent.cpu.protect(OUTPUT, PAGE, Prot::READ).unwrap();
            then(&mut env(&mut parent))
        } else {
            operation
        };
        assert!(matches!(outcome, Outcome::Return(0xc0000005)));
        assert_eq!(child.sys.vm.region(REMOTE), (!lose_output).then_some(PAGE));
        assert_eq!(parent.cpu.read_u32(OUTPUT).unwrap(), REMOTE);
        assert_eq!(parent.cpu.read_u32(OUTPUT + 4).unwrap(), 0);
        assert_eq!(crate::util::last_error(&env(&mut parent)), 0x13572468);
        assert!(!s.pending());
    }

    let mut parent = machine(make(), 0x1000);
    let child = machine(make(), 0x2000);
    let s = processes::Services::default();
    parent.ext.insert(s.clone());
    let handle = parent.sys.objects.insert(Object::Process {
        identity: child.sys.process_identity.clone(),
        access: 8,
    });
    parent.cpu.write_u32(OUTPUT, REMOTE).unwrap();
    parent.cpu.write_u32(OUTPUT + 4, 0).unwrap();
    crate::util::set_last_error(&mut env(&mut parent), 0x13572468);
    let Outcome::HostRequest { then } = free(
        &mut env(&mut parent),
        Handle(handle),
        GuestPtr::new(OUTPUT),
        GuestPtr::new(OUTPUT + 4),
        0x8000,
    ) else {
        panic!("free must request the target's live owner")
    };
    let processes::Request::Visit { task, .. } = s.pop().unwrap() else {
        panic!("expected VM request")
    };
    task(Err(processes::Error::new(87, "process has exited")));
    assert!(matches!(
        then(&mut env(&mut parent)),
        Outcome::Return(0xc000010a)
    ));
    assert_eq!(parent.cpu.read_u32(OUTPUT).unwrap(), REMOTE);
    assert_eq!(parent.cpu.read_u32(OUTPUT + 4).unwrap(), 0);
    assert_eq!(crate::util::last_error(&env(&mut parent)), 0x13572468);
}

#[test]
fn remote_free_validates_outputs_and_does_not_restore_a_released_owner() {
    for jit in [false, true] {
        free_outputs(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    free_outputs(|| UnicornCpu::new(PARK_ADDR).unwrap());
}
