use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, Reg},
};
use adv_machine::{
    Clock, Machine,
    layout::{GRANULE, PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_os::processes;
use adv_unicorn::UnicornCpu;

const DATA: u32 = 0x410000;
const CODE: u32 = 0x81234567;

fn machine<B: Backend>(cpu: B, pid: u32) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine.sys.set_process_identity(pid).unwrap();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/nt-termination-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&path, "fixture.exe", &path),
    )
    .unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    for name in ["caller", "peer"] {
        machine.spawn(name, PARK_ADDR, &[], GRANULE).unwrap();
    }
    machine.cpu.write_u32(DATA, 0x24681357).unwrap();
    machine
}
fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn status(outcome: Outcome, expected: u32) {
    assert!(matches!(outcome, Outcome::Return(value) if value == expected));
}
fn exit(outcome: Outcome, expected: u32) {
    assert!(matches!(outcome, Outcome::ExitProcess(value) if value == expected));
}
fn last_error<B: Backend>(machine: &mut Machine<B>) {
    assert_eq!(crate::util::last_error(&env(machine)), 0x13572468);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0x24681357);
}
fn query<B: Backend>(machine: &mut Machine<B>) -> u32 {
    let entry = machine
        .resolve_import("kernel32.dll", "GetExitCodeProcess")
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA + 512);
    for (index, value) in [PARK_ADDR, u32::MAX, DATA + 256].into_iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 512 + index as u32 * 4, value)
            .unwrap();
    }
    let index = (entry - THUNK_BASE) / THUNK_STRIDE;
    let handler = machine.sys.thunks.get(index).f.handler;
    let mut env = env(machine);
    env.thunk = index;
    status(handler(&mut env), 1);
    machine.cpu.read_u32(DATA + 256).unwrap()
}

fn local<B: Backend>(make: impl Fn() -> B) {
    for current in [u32::MAX, 0] {
        let mut machine = machine(make(), 0x1000);
        let own = machine.sys.objects.insert(Object::Process {
            identity: machine.sys.process_identity.clone(),
            access: 1,
        });
        let denied = machine.sys.objects.insert(Object::Process {
            identity: machine.sys.process_identity.clone(),
            access: 0x1000,
        });
        crate::util::set_last_error(&mut env(&mut machine), 0x13572468);
        status(
            terminate(&mut env(&mut machine), Handle(denied), CODE as i32),
            0xc0000022,
        );
        assert!(
            machine
                .sys
                .contexts
                .iter()
                .all(|context| !context.is_done())
        );
        exit(
            terminate(
                &mut env(&mut machine),
                Handle(if current == 0 { own } else { current }),
                CODE as i32,
            ),
            CODE,
        );
        last_error(&mut machine);
    }
    for caller in [0, 1] {
        let mut machine = machine(make(), 0x1000);
        machine.sys.current = caller;
        assert_eq!(machine.sys.suspend_context(1 - caller), Some(0));
        let services = processes::Services::default();
        services.register(&machine.sys.process_identity);
        machine.ext.insert(services.clone());
        crate::util::set_last_error(&mut env(&mut machine), 0x13572468);
        status(terminate(&mut env(&mut machine), Handle(0), CODE as i32), 0);
        assert!(!machine.sys.contexts[caller as usize].is_done());
        assert_eq!(
            machine.sys.contexts[(1 - caller) as usize]
                .completion
                .exit_code(),
            Some(CODE)
        );
        assert_eq!(machine.sys.process_identity.completion.exit_code(), None);
        assert_eq!(
            machine.sys.process_identity.completion.exit_status(),
            Some(CODE)
        );
        assert_eq!(services.live_processes(), [(0x1000, 0)]);
        assert_eq!(query(&mut machine), CODE);
        let handle = machine.sys.objects.insert(Object::Process {
            identity: machine.sys.process_identity.clone(),
            access: 0x100000,
        });
        assert!(
            !machine.sys.try_acquire(handle),
            "pending exit status must not signal the process wait"
        );
        last_error(&mut machine);
        exit(terminate(&mut env(&mut machine), Handle(0), 73), 73);
        exit(
            crate::kernel32::terminate_process_owner(&mut env(&mut machine), 0x1000, 99, |_, _| {
                panic!("self termination must not return")
            }),
            99,
        );
        exit(
            crate::kernel32::exit_process(&mut env(&mut machine), 111),
            111,
        );
        machine.sys.terminate_process_stopped(111);
        assert_eq!(
            machine.sys.process_identity.completion.exit_code(),
            Some(CODE)
        );
        assert_eq!(
            machine.sys.contexts[caller as usize].completion.exit_code(),
            Some(111)
        );
        assert!(machine.sys.try_acquire(handle));
        last_error(&mut machine);
    }
    let mut single = machine(make(), 0x1000);
    single.sys.terminate_context_stopped(1, 99);
    status(terminate(&mut env(&mut single), Handle(0), 43), 0);
    assert!(!single.sys.contexts[0].is_done());
    assert_eq!(single.sys.process_identity.completion.exit_code(), None);
    assert_eq!(
        single.sys.process_identity.completion.exit_status(),
        Some(43)
    );
}

#[test]
fn null_publishes_status_without_signaling_and_current_handles_do_not_return() {
    for jit in [false, true] {
        local(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    local(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn remote<B: Backend>(make: impl Fn() -> B) {
    for race in [false, true] {
        let mut parent = machine(make(), 0x1000);
        let mut child = machine(make(), 0x2000);
        child.sys.suspend_context(0);
        let services = processes::Services::default();
        parent.ext.insert(services.clone());
        let allowed = parent.sys.objects.insert(Object::Process {
            identity: child.sys.process_identity.clone(),
            access: 1,
        });
        let denied = parent.sys.objects.insert(Object::Process {
            identity: child.sys.process_identity.clone(),
            access: 0x1000,
        });
        let thread = parent.sys.objects.insert(Object::Thread { ctx: 0 });
        crate::util::set_last_error(&mut env(&mut parent), 0x13572468);
        for (handle, expected) in [
            (0x12345678, 0xc0000008),
            (thread, 0xc0000024),
            (u32::MAX - 1, 0xc0000024),
            (denied, 0xc0000022),
        ] {
            status(
                terminate(&mut env(&mut parent), Handle(handle), CODE as i32),
                expected,
            );
            assert!(!services.pending());
            assert!(child.sys.contexts.iter().all(|context| !context.is_done()));
            last_error(&mut parent);
        }
        let Outcome::HostRequest { then } =
            terminate(&mut env(&mut parent), Handle(allowed), CODE as i32)
        else {
            panic!("remote termination must visit the real supervisor owner")
        };
        let processes::Request::Visit { pid, context, task } = services.pop().unwrap() else {
            panic!("expected a process visit")
        };
        assert_eq!(pid, 0x2000);
        assert_eq!(context, None);
        if race {
            assert!(child.sys.process_identity.completion.begin_termination(37));
        }
        task(Ok(&mut env(&mut child)));
        status(
            then(&mut env(&mut parent)),
            if race { 0xc000010a } else { 0 },
        );
        if race {
            assert_eq!(child.sys.process_identity.completion.exit_code(), None);
            assert_eq!(
                child.sys.process_identity.completion.exit_status(),
                Some(37)
            );
            assert!(child.sys.contexts.iter().all(|context| !context.is_done()));
        } else {
            assert_eq!(
                child.sys.process_identity.completion.exit_code(),
                Some(CODE)
            );
            assert!(child.sys.contexts.iter().all(|context| context.is_done()));
        }
        status(
            terminate(&mut env(&mut parent), Handle(allowed), 99),
            0xc000010a,
        );
        assert!(!services.pending());
        assert!(parent.sys.contexts.iter().all(|context| !context.is_done()));
        last_error(&mut parent);
    }
}

#[test]
fn remote_termination_checks_rights_and_uses_live_owner_without_mutating_failures() {
    for jit in [false, true] {
        remote(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    remote(|| UnicornCpu::new(PARK_ADDR).unwrap());
}
