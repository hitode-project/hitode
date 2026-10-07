use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest},
};
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
