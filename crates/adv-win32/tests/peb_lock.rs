//! Process PEB-lock storage and zero-argument ABI ownership checks.
use adv_abi::{ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot},
};
use adv_machine::{
    Clock, Conv, Env, Handler, Machine, Outcome,
    layout::{PARK_ADDR, PEB_ADDR, peb, teb},
};
use adv_os::Heaps;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const LAST_ERROR: u32 = 0x1357_2468;

fn machine<B: Backend>(cpu: B) -> (Machine<B>, [Handler; 2]) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let handlers = ["RtlAcquirePebLock", "RtlReleasePebLock"].map(|name| {
        let export = registry.get("ntdll.dll", name).unwrap();
        assert_eq!(export.conv, Conv::Stdcall { argc: 0 });
        assert_eq!(
            registry.level("ntdll.dll", name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available("ntdll.dll", name));
        export.handler
    });
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/peb-lock",
            "fixture.exe",
            "/nonexistent/peb-lock-data",
        ),
    )
    .unwrap();
    machine
        .spawn("PEB-lock owner", PARK_ADDR, &[], 65536)
        .unwrap();
    let section = section(&machine);
    assert_ne!(section, 0);
    assert!(machine.cpu.accessible(section, 24, Prot::RW));
    assert!(machine.sys.vm.region(section).is_some());
    assert_eq!(
        machine.ext.get::<Heaps>().owner(&machine.cpu, section),
        None
    );
    assert_eq!(state(&machine), (0, 0));
    machine
        .cpu
        .write_u32(machine.sys.contexts[0].teb + teb::LAST_ERROR, LAST_ERROR)
        .unwrap();
    (machine, handlers)
}

fn section<B: Backend>(machine: &Machine<B>) -> u32 {
    machine.cpu.read_u32(PEB_ADDR + peb::FAST_PEB_LOCK).unwrap()
}

fn state<B: Backend>(machine: &Machine<B>) -> (u32, u32) {
    let section = section(machine);
    (
        machine.cpu.read_u32(section + 8).unwrap(),
        machine.cpu.read_u32(section + 12).unwrap(),
    )
}

fn call<B: Backend>(machine: &mut Machine<B>, handler: Handler) {
    let result = handler(&mut Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    });
    assert!(matches!(result, Outcome::Return(0)));
    assert_eq!(
        machine
            .cpu
            .read_u32(machine.sys.contexts[0].teb + teb::LAST_ERROR)
            .unwrap(),
        LAST_ERROR
    );
}

fn isolated<B: Backend>(first: B, second: B) {
    let (mut first, [acquire, release]) = machine(first);
    let (mut second, [second_acquire, second_release]) = machine(second);
    call(&mut first, acquire);
    call(&mut first, acquire);
    assert_eq!(state(&first), (2, first.sys.contexts[0].tid));
    assert_eq!(state(&second), (0, 0));
    call(&mut second, second_acquire);
    assert_eq!(state(&second), (1, second.sys.contexts[0].tid));
    call(&mut first, release);
    assert_eq!(state(&first), (1, first.sys.contexts[0].tid));
    call(&mut first, release);
    assert_eq!(state(&first), (0, 0));
    assert_eq!(state(&second), (1, second.sys.contexts[0].tid));
    call(&mut second, second_release);
    assert_eq!(state(&second), (0, 0));
}

#[test]
fn peb_locks_have_process_vm_lifetime_and_independent_recursive_owners() {
    isolated(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        UnicornCpu::new(PARK_ADDR).unwrap(),
    );
    for jit in [false, true] {
        isolated(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            JitCpu::new(PARK_ADDR, jit).unwrap(),
        );
    }
}
