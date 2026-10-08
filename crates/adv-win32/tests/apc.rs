//! Authored guest APCs verify FIFO, alertability, target ownership and live NT calls.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, Object, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_os::processes::{Request, Services};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const CALLBACK: u32 = CODE + 0x2000;
const WORKER: u32 = CODE + 0x3000;
const DATA: u32 = 0x410000;
const ORDER: u32 = DATA + 0x200;
const OWNER: u32 = DATA + 0x204;
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};

fn fixture(jit: bool, pid: u32, services: &Services) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.sys.set_process_identity(pid).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new("/nonexistent/apc", "apc.exe", "/nonexistent/apc-data"),
    )
    .unwrap();
    Win32::prepare_process_infrastructure(&mut machine).unwrap();
    machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x2000, Prot::RW).unwrap();
    services.register(&machine.sys.process_identity);
    machine.ext.insert(services.clone());
    let thread_id = machine
        .resolve_import("kernel32.dll", "GetCurrentThreadId")
        .unwrap();
    let mut callback = Asm::new(CALLBACK);
    callback.call(thread_id).mov_mem_eax(OWNER);
    callback.mov_eax_mem(ORDER);
    callback
        .bytes
        .extend_from_slice(&[0x6b, 0xc0, 10, 0x03, 0x44, 0x24, 4]); // eax = order*10 + argument
    callback.mov_mem_eax(ORDER).ret_n(4);
    machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
    machine
}
fn call(machine: &mut Machine<JitCpu>, code: &mut Asm, api: &str, args: &[Arg], out: u32) {
    let (module, name) = api.split_once('!').unwrap_or(("kernel32.dll", api));
    let entry = machine.resolve_import(module, name).unwrap();
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            M(pointer) => {
                code.mov_eax_mem(*pointer).push_eax();
            }
        }
    }
    code.call(entry);
    if out != 0 {
        code.mov_mem_eax(out);
    }
}
fn drive(
    machine: &mut Machine<JitCpu>,
    mut child: Option<&mut Machine<JitCpu>>,
    services: &Services,
) {
    for _ in 0..100 {
        match machine.run_virtual(1_000_000) {
            RunResult::HostRequest => {
                while let Some(request) = services.pop() {
                    match request {
                        Request::Visit { pid, context, task } if pid == machine.sys.pid => {
                            machine.visit_context(context, |env| task(Ok(env))).unwrap();
                        }
                        Request::Visit { pid, context, task } => {
                            let target = child
                                .as_deref_mut()
                                .filter(|target| target.sys.pid == pid)
                                .expect("known APC target");
                            target.visit_context(context, |env| task(Ok(env))).unwrap();
                        }
                        _ => panic!("APC fixture never creates a process"),
                    }
                }
            }
            RunResult::Idle { .. } if child.is_some() => {
                let result = child.as_deref_mut().unwrap().run_virtual(1_000_000);
                assert!(matches!(result, RunResult::Idle { .. }), "{result:?}");
            }
            RunResult::Exited(0) => return,
            other => panic!("APC fixture failed: {other:?}"),
        }
    }
    panic!("APC fixture did not finish");
}
fn start(machine: &mut Machine<JitCpu>, code: Asm) {
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("APC caller", CODE, &[], 0x10000).unwrap();
}
fn word(machine: &Machine<JitCpu>, offset: u32) -> u32 {
    machine.cpu.read_u32(DATA + offset).unwrap()
}

#[test]
fn fifo_queues_remain_pending_for_nonalertable_calls_and_native_test_alert_runs_three_arguments() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = fixture(jit, 0x1000, &services);
        let native = CALLBACK + 0x100;
        let mut callback = Asm::new(native);
        for i in 0..3 {
            callback
                .mov_eax_esp(4 + i * 4)
                .mov_mem_eax(DATA + 0x210 + u32::from(i) * 4);
        }
        callback.ret_n(12);
        machine.cpu.write(native, &callback.bytes).unwrap();
        let mut code = Asm::new(CODE);
        for parameter in [1, 2] {
            call(
                &mut machine,
                &mut code,
                "QueueUserAPC",
                &[I(CALLBACK), I(u32::MAX - 1), I(parameter)],
                DATA + parameter * 4,
            );
        }
        call(&mut machine, &mut code, "Sleep", &[I(0)], 0);
        call(&mut machine, &mut code, "SleepEx", &[I(0), I(0)], DATA + 12);
        code.mov_eax_mem(ORDER).mov_mem_eax(DATA + 16);
        call(&mut machine, &mut code, "SleepEx", &[I(0), I(1)], DATA + 20);
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtQueueApcThread",
            &[I(u32::MAX - 1), I(native), I(3), I(4), I(5)],
            DATA + 24,
        );
        call(
            &mut machine,
            &mut code,
            "ntdll.dll!NtTestAlert",
            &[],
            DATA + 28,
        );
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        start(&mut machine, code);
        drive(&mut machine, None, &services);
        assert_eq!((word(&machine, 4), word(&machine, 8)), (1, 1));
        assert_eq!((word(&machine, 12), word(&machine, 16)), (0, 0));
        assert_eq!((word(&machine, 20), word(&machine, 0x200)), (0xc0, 12));
        assert_eq!(word(&machine, 0x204), machine.sys.contexts[0].tid);
        assert_eq!((word(&machine, 24), word(&machine, 28)), (0, 0));
        assert_eq!(
            (
                word(&machine, 0x210),
                word(&machine, 0x214),
                word(&machine, 0x218)
            ),
            (3, 4, 5)
        );
    }
}

#[test]
fn pending_before_start_and_alertable_blocked_thread_callbacks_execute_on_the_target() {
    for jit in [false, true] {
        for suspended in [false, true] {
            let services = Services::default();
            let mut machine = fixture(jit, 0x1000, &services);
            let mut worker = Asm::new(WORKER);
            if suspended {
                worker.mov_eax_mem(ORDER).mov_mem_eax(DATA + 0x220);
            } else {
                call(
                    &mut machine,
                    &mut worker,
                    "SleepEx",
                    &[I(u32::MAX), I(1)],
                    DATA + 0x220,
                );
            }
            worker.bytes.extend_from_slice(&[0xb8, 37, 0, 0, 0]);
            worker.ret_n(4);
            machine.cpu.write(WORKER, &worker.bytes).unwrap();
            let mut code = Asm::new(CODE);
            call(
                &mut machine,
                &mut code,
                "CreateThread",
                &[
                    I(0),
                    I(0),
                    I(WORKER),
                    I(0),
                    I(if suspended { 4 } else { 0 }),
                    I(DATA + 4),
                ],
                DATA,
            );
            if !suspended {
                call(&mut machine, &mut code, "Sleep", &[I(0)], 0);
            }
            call(
                &mut machine,
                &mut code,
                "QueueUserAPC",
                &[I(CALLBACK), M(DATA), I(7)],
                DATA + 8,
            );
            if suspended {
                call(
                    &mut machine,
                    &mut code,
                    "ResumeThread",
                    &[M(DATA)],
                    DATA + 12,
                );
            }
            call(
                &mut machine,
                &mut code,
                "WaitForSingleObject",
                &[M(DATA), I(u32::MAX)],
                DATA + 16,
            );
            call(
                &mut machine,
                &mut code,
                "GetExitCodeThread",
                &[M(DATA), I(DATA + 20)],
                DATA + 24,
            );
            call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
            start(&mut machine, code);
            drive(&mut machine, None, &services);
            assert_eq!(
                (
                    word(&machine, 8),
                    word(&machine, 16),
                    word(&machine, 20),
                    word(&machine, 24)
                ),
                (1, 0, 37, 1)
            );
            assert_eq!(word(&machine, 0x200), 7);
            assert_eq!(word(&machine, 0x204), word(&machine, 4));
            assert_eq!(word(&machine, 0x220), if suspended { 7 } else { 0xc0 });
        }
    }
}

fn hook(machine: &mut Machine<JitCpu>, name: &str, at: u32, counter: u32, alter: bool) {
    let binding = machine.resolve_import("ntdll.dll", name).unwrap();
    let original = machine
        .sys
        .thunks
        .pristine_entry((binding - THUNK_BASE) / THUNK_STRIDE);
    machine.cpu.write(at + 0x100, &original).unwrap();
    let mut code = Asm::new(at);
    code.mov_eax_mem(counter)
        .add_eax_imm(1)
        .mov_mem_eax(counter);
    if alter {
        code.bytes
            .extend_from_slice(&[0xc7, 0x44, 0x24, 16, 7, 0, 0, 0]);
    }
    code.jmp(at + 0x100);
    machine.cpu.write(at, &code.bytes).unwrap();
    let modules = adv_os::modules(&machine.ext);
    let ntdll = modules.get(modules.by_name("ntdll.dll").unwrap());
    let entry = match ntdll.exports_by_name.get(name).unwrap() {
        adv_pe::ExportTarget::Rva(rva) => ntdll.base + rva,
        _ => panic!("code entry"),
    };
    let mut patch = Asm::new(entry);
    patch.jmp(at);
    machine.cpu.write(entry, &patch.bytes).unwrap();
}
#[test]
fn win32_queue_sleep_and_wait_execute_live_native_hooks_and_keep_nonalertable_apcs_pending() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = fixture(jit, 0x1000, &services);
        hook(
            &mut machine,
            "NtQueueApcThread",
            CODE + 0x4000,
            DATA + 0x240,
            true,
        );
        hook(
            &mut machine,
            "NtDelayExecution",
            CODE + 0x5000,
            DATA + 0x244,
            false,
        );
        hook(
            &mut machine,
            "NtWaitForSingleObject",
            CODE + 0x6000,
            DATA + 0x248,
            false,
        );
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateEventA",
            &[I(0), I(1), I(0), I(0)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "QueueUserAPC",
            &[I(CALLBACK), I(u32::MAX - 1), I(9)],
            DATA + 4,
        );
        call(&mut machine, &mut code, "SleepEx", &[I(0), I(1)], DATA + 8);
        call(
            &mut machine,
            &mut code,
            "QueueUserAPC",
            &[I(CALLBACK), I(u32::MAX - 1), I(9)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObjectEx",
            &[M(DATA), I(0), I(0)],
            DATA + 16,
        );
        code.mov_eax_mem(ORDER).mov_mem_eax(DATA + 20);
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObjectEx",
            &[M(DATA), I(0), I(1)],
            DATA + 24,
        );
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        start(&mut machine, code);
        drive(&mut machine, None, &services);
        assert_eq!(
            (word(&machine, 4), word(&machine, 8), word(&machine, 12)),
            (1, 0xc0, 1)
        );
        assert_eq!(
            (word(&machine, 16), word(&machine, 20), word(&machine, 24)),
            (0x102, 7, 0xc0)
        );
        assert_eq!(word(&machine, 0x200), 77);
        assert_eq!(
            (
                word(&machine, 0x240),
                word(&machine, 0x244),
                word(&machine, 0x248)
            ),
            (2, 1, 2)
        );
    }
}

#[test]
fn apcs_queued_to_a_family_thread_execute_in_the_remote_owner() {
    for jit in [false, true] {
        let services = Services::default();
        let mut parent = fixture(jit, 0x1000, &services);
        let mut child = fixture(jit, 0x2000, &services);
        let keeper = child
            .spawn("suspended child main", PARK_ADDR, &[], 0x10000)
            .unwrap();
        child.sys.suspend_context(keeper);
        let mut worker = Asm::new(WORKER);
        call(
            &mut child,
            &mut worker,
            "SleepEx",
            &[I(u32::MAX), I(1)],
            DATA + 0x220,
        );
        call(&mut child, &mut worker, "ExitThread", &[I(37)], 0);
        child.cpu.write(WORKER, &worker.bytes).unwrap();
        let target = child
            .spawn("remote APC target", WORKER, &[], 0x10000)
            .unwrap();
        assert!(matches!(child.run_virtual(1000), RunResult::Idle { .. }));
        let identity = child.sys.thread_identity(target).unwrap();
        let tid = identity.tid;
        let handle = parent.sys.objects.insert(Object::RemoteThread {
            identity,
            access: 0x1fffff,
        });
        let mut code = Asm::new(CODE);
        call(
            &mut parent,
            &mut code,
            "QueueUserAPC",
            &[I(CALLBACK), I(handle), I(5)],
            DATA,
        );
        call(
            &mut parent,
            &mut code,
            "WaitForSingleObject",
            &[I(handle), I(u32::MAX)],
            DATA + 4,
        );
        call(
            &mut parent,
            &mut code,
            "GetExitCodeThread",
            &[I(handle), I(DATA + 8)],
            DATA + 12,
        );
        call(&mut parent, &mut code, "ExitProcess", &[I(0)], 0);
        start(&mut parent, code);
        drive(&mut parent, Some(&mut child), &services);
        assert_eq!(
            (
                word(&parent, 0),
                word(&parent, 4),
                word(&parent, 8),
                word(&parent, 12)
            ),
            (1, 0, 37, 1)
        );
        assert_eq!(
            (
                word(&child, 0x200),
                word(&child, 0x204),
                word(&child, 0x220)
            ),
            (5, tid, 0xc0)
        );
        assert_eq!(word(&parent, 0x200), 0);
    }
}

#[test]
fn terminating_an_alertable_target_discards_queued_callbacks_and_notification_storage() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = fixture(jit, 0x1000, &services);
        let event_count = machine
            .sys
            .objects
            .kernel_objects()
            .filter(|object| matches!(object, Object::Event { .. }))
            .count();
        let mut worker = Asm::new(WORKER);
        call(
            &mut machine,
            &mut worker,
            "SleepEx",
            &[I(u32::MAX), I(1)],
            DATA + 0x220,
        );
        worker.ret_n(4);
        machine.cpu.write(WORKER, &worker.bytes).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(WORKER), I(0), I(0), I(0)],
            DATA,
        );
        call(&mut machine, &mut code, "Sleep", &[I(0)], 0);
        call(
            &mut machine,
            &mut code,
            "QueueUserAPC",
            &[I(CALLBACK), M(DATA), I(7)],
            DATA + 4,
        );
        call(
            &mut machine,
            &mut code,
            "TerminateThread",
            &[M(DATA), I(37)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA), I(0)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA), I(DATA + 16)],
            DATA + 20,
        );
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        start(&mut machine, code);
        drive(&mut machine, None, &services);
        assert_eq!(
            (
                word(&machine, 4),
                word(&machine, 8),
                word(&machine, 12),
                word(&machine, 16),
                word(&machine, 20)
            ),
            (1, 1, 0, 37, 1)
        );
        assert_eq!(
            word(&machine, 0x200),
            0,
            "a canceled APC never runs on the terminated thread"
        );
        assert_eq!(
            machine
                .sys
                .objects
                .kernel_objects()
                .filter(|object| matches!(object, Object::Event { .. }))
                .count(),
            event_count
        );
    }
}

fn event_count(machine: &Machine<JitCpu>) -> usize {
    machine
        .sys
        .objects
        .kernel_objects()
        .filter(|object| matches!(object, Object::Event { .. }))
        .count()
}

#[test]
fn native_self_thread_termination_discards_its_pending_apc_and_notification() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = fixture(jit, 0x1000, &services);
        let baseline = event_count(&machine);
        let mut worker = Asm::new(WORKER);
        call(
            &mut machine,
            &mut worker,
            "SleepEx",
            &[I(1), I(1)],
            DATA + 0x220,
        );
        call(
            &mut machine,
            &mut worker,
            "QueueUserAPC",
            &[I(CALLBACK), I(u32::MAX - 1), I(7)],
            DATA + 4,
        );
        call(
            &mut machine,
            &mut worker,
            "ntdll.dll!NtTerminateThread",
            &[I(u32::MAX - 1), I(37)],
            0,
        );
        worker.ret_n(4);
        machine.cpu.write(WORKER, &worker.bytes).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(WORKER), I(0), I(0), I(0)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA), I(u32::MAX)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA), I(DATA + 12)],
            DATA + 16,
        );
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        start(&mut machine, code);
        drive(&mut machine, None, &services);
        assert_eq!(
            (
                word(&machine, 4),
                word(&machine, 8),
                word(&machine, 12),
                word(&machine, 16)
            ),
            (1, 0, 37, 1)
        );
        assert_eq!(word(&machine, 0x200), 0);
        assert_eq!(event_count(&machine), baseline);
    }
}

#[test]
fn native_current_and_null_process_termination_retire_pending_apcs_and_notifications() {
    for jit in [false, true] {
        for null in [false, true] {
            let services = Services::default();
            let mut machine = fixture(jit, 0x1000, &services);
            let baseline = event_count(&machine);
            let peer = if null {
                let mut worker = Asm::new(WORKER);
                call(
                    &mut machine,
                    &mut worker,
                    "SleepEx",
                    &[I(u32::MAX), I(1)],
                    0,
                );
                worker.ret_n(4);
                machine.cpu.write(WORKER, &worker.bytes).unwrap();
                Some(
                    machine
                        .spawn("NULL termination peer", WORKER, &[], 0x10000)
                        .unwrap(),
                )
            } else {
                None
            };
            let mut code = Asm::new(CODE);
            // A timed alertable call creates the current thread's private wake event.
            call(&mut machine, &mut code, "SleepEx", &[I(1), I(1)], DATA);
            call(
                &mut machine,
                &mut code,
                "QueueUserAPC",
                &[I(CALLBACK), I(u32::MAX - 1), I(7)],
                DATA + 4,
            );
            if let Some(peer) = peer {
                let handle = machine.sys.contexts[peer as usize].thread_handle;
                call(
                    &mut machine,
                    &mut code,
                    "QueueUserAPC",
                    &[I(CALLBACK), I(handle), I(8)],
                    DATA + 16,
                );
            }
            call(
                &mut machine,
                &mut code,
                "ntdll.dll!NtTerminateProcess",
                &[I(if null { 0 } else { u32::MAX }), I(37)],
                DATA + 8,
            );
            if null {
                // The first NULL call preserves the caller; its APC stays pending.
                code.mov_eax_mem(ORDER).mov_mem_eax(DATA + 12);
                call(
                    &mut machine,
                    &mut code,
                    "ntdll.dll!NtTerminateProcess",
                    &[I(0), I(43)],
                    0,
                );
            }
            start(&mut machine, code);
            assert!(matches!(
                machine.run_virtual(1_000_000),
                RunResult::Exited(37)
            ));
            assert_eq!((word(&machine, 0), word(&machine, 4)), (0, 1));
            assert_eq!(word(&machine, 0x200), 0);
            assert_eq!(word(&machine, 12), 0);
            assert_eq!(event_count(&machine), baseline);
            if let Some(peer) = peer {
                assert_eq!(word(&machine, 16), 1);
                assert_eq!(
                    machine.sys.contexts[peer as usize].completion.exit_code(),
                    Some(37)
                );
                assert_eq!(machine.sys.contexts[1].completion.exit_code(), Some(43));
            }
        }
    }
}

#[test]
fn native_family_process_termination_clears_target_thread_apcs_without_delivering_them() {
    for jit in [false, true] {
        let services = Services::default();
        let mut parent = fixture(jit, 0x1000, &services);
        let mut child = fixture(jit, 0x2000, &services);
        let baseline = event_count(&child);
        let mut worker = Asm::new(WORKER);
        call(&mut child, &mut worker, "SleepEx", &[I(u32::MAX), I(1)], 0);
        worker.ret_n(4);
        child.cpu.write(WORKER, &worker.bytes).unwrap();
        let target = child
            .spawn("remote termination target", WORKER, &[], 0x10000)
            .unwrap();
        assert!(matches!(child.run_virtual(1000), RunResult::Idle { .. }));
        let thread = parent.sys.objects.insert(Object::RemoteThread {
            identity: child.sys.thread_identity(target).unwrap(),
            access: 0x1fffff,
        });
        let process = parent.sys.objects.insert(Object::Process {
            identity: child.sys.process_identity.clone(),
            access: 0x1fffff,
        });
        let mut code = Asm::new(CODE);
        call(
            &mut parent,
            &mut code,
            "QueueUserAPC",
            &[I(CALLBACK), I(thread), I(7)],
            DATA,
        );
        call(
            &mut parent,
            &mut code,
            "ntdll.dll!NtTerminateProcess",
            &[I(process), I(37)],
            DATA + 4,
        );
        call(
            &mut parent,
            &mut code,
            "GetExitCodeThread",
            &[I(thread), I(DATA + 8)],
            DATA + 12,
        );
        call(&mut parent, &mut code, "ExitProcess", &[I(0)], 0);
        start(&mut parent, code);
        drive(&mut parent, Some(&mut child), &services);
        assert_eq!(
            (
                word(&parent, 0),
                word(&parent, 4),
                word(&parent, 8),
                word(&parent, 12)
            ),
            (1, 0, 37, 1)
        );
        assert_eq!(word(&child, 0x200), 0);
        assert_eq!(event_count(&child), baseline);
        assert_eq!(child.sys.process_identity.completion.exit_code(), Some(37));
    }
}
