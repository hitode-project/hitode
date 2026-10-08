//! Live reads use real quiesced register owners without changing suspension.
use super::*;
use adv_cpu::{JitCpu, backend::Backend};
use adv_machine::{
    Clock, Machine,
    layout::{PAGE, PARK_ADDR},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const FLAGS: u32 = 0x10007;
const LAST_ERROR: u32 = 0x13572468;

fn machine<B: Backend>(cpu: B, pid: u32) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine.sys.set_process_identity(pid).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/live-context-tests");
    Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    machine.spawn("caller", PARK_ADDR, &[], 65536).unwrap();
    machine.spawn("live target", PARK_ADDR, &[], 65536).unwrap();
    machine
        .visit_context(Some(0), |env| {
            env.cpu.set_reg(Reg::Eip, CODE + 16);
            env.cpu.set_reg(Reg::Eax, 0x11112222);
        })
        .unwrap();
    machine
        .visit_context(Some(1), |env| {
            env.cpu.set_reg(Reg::Eip, CODE + 64);
            env.cpu.set_reg(Reg::Eax, 0x33334444);
        })
        .unwrap();
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
fn reset<B: Backend>(machine: &mut Machine<B>) {
    let mut bytes = [0xa5; thread_context::SIZE];
    bytes[..4].copy_from_slice(&FLAGS.to_le_bytes());
    machine.cpu.write(DATA, &bytes).unwrap();
    set_last_error(&mut env(machine), LAST_ERROR);
}
fn output_unchanged<B: Backend>(machine: &Machine<B>) {
    let mut bytes = [0; thread_context::SIZE];
    machine.cpu.read(DATA, &mut bytes).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), FLAGS);
    assert_eq!(&bytes[4..], &[0xa5; thread_context::SIZE - 4]);
}
fn value(outcome: Outcome, expected: u32) {
    assert!(matches!(outcome, Outcome::Return(actual) if actual == expected));
}
fn service<B: Backend>(services: &processes::Services, owner: &mut Machine<B>) {
    let processes::Request::Visit { pid, context, task } = services.pop().unwrap() else {
        panic!("expected actual context owner visit");
    };
    assert_eq!(pid, owner.sys.pid);
    assert_eq!(context, Some(1));
    let mut task = Some(task);
    if let Err(error) = owner.visit_context(context, |env| task.take().unwrap()(Ok(env))) {
        task.take().unwrap()(Err(processes::Error::new(87, error)));
    }
}
fn front(win32: bool) -> ContextFront {
    if win32 {
        ContextFront::Win32
    } else {
        ContextFront::Native("ntdll.dll!NtGetContextThread")
    }
}
fn live<B: Backend>(make: impl Fn() -> B) {
    for remote in [false, true] {
        for win32 in [false, true] {
            let mut caller = machine(make(), 0x1000);
            let mut child = machine(make(), 0x2000);
            let identity = if remote {
                child.sys.thread_identity(1)
            } else {
                caller.sys.thread_identity(1)
            }
            .unwrap();
            let handle = caller.sys.objects.insert(Object::RemoteThread {
                identity,
                access: 0x1fffff,
            });
            let services = processes::Services::default();
            caller.ext.insert(services.clone());
            reset(&mut caller);
            let before = caller.cpu.capture_architecture().unwrap();
            let Outcome::HostRequest { then } =
                thread_context_front(&mut env(&mut caller), handle, DATA, false, front(win32))
            else {
                panic!("live read must visit its architecture owner");
            };
            if remote {
                service(&services, &mut child);
            } else {
                service(&services, &mut caller);
            }
            assert_eq!(
                caller.cpu.capture_architecture().unwrap(),
                before,
                "restore caller registers and FS after visiting another thread"
            );
            value(then(&mut env(&mut caller)), u32::from(win32));
            assert_eq!(caller.cpu.read_u32(DATA + 0xb0).unwrap(), 0x33334444);
            assert_eq!(caller.cpu.read_u32(DATA + 0xb8).unwrap(), CODE + 64);
            let mut bytes = [0; thread_context::SIZE];
            caller.cpu.read(DATA, &mut bytes).unwrap();
            assert_eq!(&bytes[4..0x8c], &[0xa5; 0x88]);
            assert_eq!(&bytes[0xcc..], &[0xa5; thread_context::SIZE - 0xcc]);
            assert_eq!(crate::util::last_error(&env(&mut caller)), LAST_ERROR);
            let owner = if remote { &mut child } else { &mut caller };
            assert_eq!(owner.sys.contexts[1].suspend_count, 0);
            assert_eq!(owner.sys.contexts[1].state_name(), "Ready");
            assert!(owner.sys.contexts[1].is_ready());
            assert!(!services.pending());
        }
    }
}
#[test]
fn live_get_visits_local_and_remote_architecture_and_restores_caller_state() {
    for jit in [false, true] {
        live(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    live(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn failures<B: Backend>(make: impl Fn() -> B) {
    for race in [0, 1, 2] {
        for win32 in [false, true] {
            let mut caller = machine(make(), 0x1000);
            let mut child = machine(make(), 0x2000);
            let handle = caller.sys.objects.insert(Object::RemoteThread {
                identity: child.sys.thread_identity(1).unwrap(),
                access: if race == 2 { 0x40 } else { 8 },
            });
            let services = processes::Services::default();
            caller.ext.insert(services.clone());
            reset(&mut caller);
            let outcome =
                thread_context_front(&mut env(&mut caller), handle, DATA, false, front(win32));
            let outcome = if race == 2 {
                assert!(!services.pending());
                outcome
            } else {
                let Outcome::HostRequest { then } = outcome else {
                    panic!("expected queued live read");
                };
                if race == 1 {
                    child.sys.terminate_context_stopped(1, 37);
                }
                service(&services, &mut child);
                if race == 0 {
                    caller.cpu.protect(DATA, PAGE, Prot::READ).unwrap();
                }
                then(&mut env(&mut caller))
            };
            let (native, error) = match race {
                0 => (0xc0000005, 998),
                1 => (0xc000000d, 87),
                _ => (0xc0000022, 5),
            };
            value(outcome, if win32 { 0 } else { native });
            output_unchanged(&caller);
            assert_eq!(
                crate::util::last_error(&env(&mut caller)),
                if win32 { error } else { LAST_ERROR }
            );
            assert_eq!(child.sys.contexts[1].suspend_count, 0);
        }
    }
}
#[test]
fn live_get_checks_rights_done_owner_and_output_publication() {
    for jit in [false, true] {
        failures(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    failures(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

fn writes<B: Backend>(make: impl Fn() -> B) {
    for remote in [false, true] {
        for win32 in [false, true] {
            let mut caller = machine(make(), 0x1000);
            let mut child = machine(make(), 0x2000);
            let identity = if remote {
                child.sys.thread_identity(1)
            } else {
                caller.sys.thread_identity(1)
            }
            .unwrap();
            let handle = caller.sys.objects.insert(Object::RemoteThread {
                identity,
                access: 0x1fffff,
            });
            let services = processes::Services::default();
            caller.ext.insert(services.clone());
            reset(&mut caller);
            let owner = if remote { &mut child } else { &mut caller };
            let target_before = owner
                .visit_context(Some(1), |env| env.cpu.capture_architecture())
                .unwrap()
                .unwrap();
            let mut input = [0xa5; thread_context::SIZE];
            input[..4].copy_from_slice(&FLAGS.to_le_bytes());
            let mut input = thread_context::capture(&target_before, input).unwrap();
            input[0xb0..0xb4].copy_from_slice(&0x55556666u32.to_le_bytes());
            caller.cpu.write(DATA, &input).unwrap();
            let before = caller.cpu.capture_architecture().unwrap();
            let api = if win32 {
                ContextFront::Win32
            } else {
                ContextFront::Native("ntdll.dll!NtSetContextThread")
            };
            let outcome = thread_context_front(&mut env(&mut caller), handle, DATA, true, api);
            let outcome = if remote {
                let Outcome::HostRequest { then } = outcome else {
                    panic!("remote SET checks the actual owner");
                };
                service(&services, &mut child);
                then(&mut env(&mut caller))
            } else {
                assert!(!services.pending());
                outcome
            };
            value(outcome, if win32 { 0 } else { 0xc000000d });
            let mut actual = [0; thread_context::SIZE];
            caller.cpu.read(DATA, &mut actual).unwrap();
            assert_eq!(actual, input);
            assert_eq!(caller.cpu.capture_architecture().unwrap(), before);
            assert_eq!(
                crate::util::last_error(&env(&mut caller)),
                if win32 { 87 } else { LAST_ERROR }
            );
            let owner = if remote { &mut child } else { &mut caller };
            assert_eq!(owner.sys.contexts[1].suspend_count, 0);
            assert_eq!(owner.sys.contexts[1].state_name(), "Ready");
            assert_eq!(
                owner
                    .visit_context(Some(1), |env| env.cpu.capture_architecture())
                    .unwrap()
                    .unwrap(),
                target_before
            );
        }
    }
}
#[test]
fn live_set_still_requires_an_external_suspend() {
    for jit in [false, true] {
        writes(|| JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    writes(|| UnicornCpu::new(PARK_ADDR).unwrap());
}
