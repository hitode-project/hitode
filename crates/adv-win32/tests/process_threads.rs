//! Authored x86 process/thread calls exercise suspension, safe context visits,
//! handle rights, cross-process identity duplication and terminal wait signals.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg, asm::Asm},
};
use adv_machine::{Clock, Machine, Object, RunResult, layout::PARK_ADDR};
use adv_os::processes::{Error, Request, Services};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const WORKER: u32 = CODE + 0x8000;
const DATA: u32 = 0x420000;
const CONTEXT: u32 = DATA + 0x1000;
const CURRENT_PROCESS: u32 = u32::MAX;

#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};

fn machine(jit: bool, pid: u32, services: &Services) -> Machine<JitCpu> {
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
        Win32Config::new(
            "/nonexistent/process-thread-probe",
            "probe.exe",
            "/nonexistent/process-thread-probe-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x3000, Prot::RW).unwrap();
    services.register(&machine.sys.process_identity);
    machine.ext.insert(services.clone());
    machine
}

fn call(machine: &mut Machine<JitCpu>, code: &mut Asm, name: &str, args: &[Arg], out: u32) {
    let entry = machine.resolve_import("kernel32.dll", name).unwrap();
    for argument in args.iter().rev() {
        match argument {
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

fn run(
    machine: &mut Machine<JitCpu>,
    mut child: Option<&mut Machine<JitCpu>>,
    services: &Services,
    code: Asm,
) {
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("main", CODE, &[], 0x10000).unwrap();
    for _ in 0..100 {
        match machine.run_virtual(1_000_000) {
            RunResult::HostRequest => {
                while let Some(request) = services.pop() {
                    match request {
                        Request::Visit { pid, context, task } => {
                            if pid == machine.sys.pid {
                                machine.visit_context(context, |env| task(Ok(env))).unwrap();
                            } else if let Some(target) =
                                child.as_deref_mut().filter(|target| target.sys.pid == pid)
                            {
                                target.visit_context(context, |env| task(Ok(env))).unwrap();
                            } else {
                                task(Err(Error::new(6, "unknown test process")));
                            }
                        }
                        Request::Spawn { .. } => panic!("fixture never creates a process"),
                    }
                }
            }
            RunResult::Exited(0) => return,
            other => panic!("unexpected fixture result: {other:?}"),
        }
    }
    panic!("too many supervisor requests");
}

fn word(machine: &Machine<JitCpu>, offset: u32) -> u32 {
    machine.cpu.read_u32(DATA + offset).unwrap()
}

#[test]
fn create_suspended_thread_counts_gate_execution_and_partial_context_changes_are_retained() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = machine(jit, 0x1000, &services);
        let mut worker = Asm::new(WORKER);
        worker.mov_mem_imm(DATA + 0x100, 1).push_imm(37);
        worker.bytes.push(0x58); // pop eax
        worker.ret_n(4);
        machine.cpu.write(WORKER, &worker.bytes).unwrap();
        let mut context = [0xa5; 716];
        context[..4].copy_from_slice(&0x10002u32.to_le_bytes());
        machine.cpu.write(CONTEXT, &context).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(WORKER), I(0), I(4), I(DATA + 0x80)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA), I(DATA + 4)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "SuspendThread",
            &[M(DATA)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "ResumeThread",
            &[M(DATA)],
            DATA + 16,
        );
        call(&mut machine, &mut code, "Sleep", &[I(0)], 0);
        code.mov_eax_mem(DATA + 0x100).mov_mem_eax(DATA + 20);
        call(
            &mut machine,
            &mut code,
            "GetThreadContext",
            &[M(DATA), I(CONTEXT)],
            DATA + 24,
        );
        code.mov_mem_imm(CONTEXT + 0x9c, 0x1234abcd);
        call(
            &mut machine,
            &mut code,
            "SetThreadContext",
            &[M(DATA), I(CONTEXT)],
            DATA + 28,
        );
        code.mov_mem_imm(CONTEXT + 0x9c, 0);
        call(
            &mut machine,
            &mut code,
            "GetThreadContext",
            &[M(DATA), I(CONTEXT)],
            DATA + 32,
        );
        code.mov_eax_mem(CONTEXT + 0x9c).mov_mem_eax(DATA + 36);
        call(
            &mut machine,
            &mut code,
            "ResumeThread",
            &[M(DATA)],
            DATA + 40,
        );
        // A live thread's read visits its quiesced architecture without adding
        // an external suspend count or consuming the pending worker execution.
        call(
            &mut machine,
            &mut code,
            "GetThreadContext",
            &[M(DATA), I(CONTEXT)],
            DATA + 44,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 48);
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA), I(u32::MAX)],
            DATA + 52,
        );
        call(
            &mut machine,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA), I(DATA + 56)],
            DATA + 60,
        );
        call(
            &mut machine,
            &mut code,
            "ResumeThread",
            &[M(DATA)],
            DATA + 64,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 68);
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        run(&mut machine, None, &services, code);
        assert_ne!(word(&machine, 0), 0);
        assert_eq!((word(&machine, 4), word(&machine, 8)), (259, 1));
        assert_eq!((word(&machine, 12), word(&machine, 16)), (1, 2));
        assert_eq!(
            word(&machine, 20),
            0,
            "suspended thread never ran during a yield"
        );
        assert_eq!(
            (word(&machine, 24), word(&machine, 28), word(&machine, 32)),
            (1, 1, 1)
        );
        assert_eq!(word(&machine, 36), 0x1234abcd);
        assert_eq!(
            machine.cpu.read_u32(CONTEXT + 0xb8).unwrap(),
            0xa5a5a5a5,
            "integer capture preserves unrequested control fields"
        );
        assert_eq!(word(&machine, 40), 1);
        assert_eq!((word(&machine, 44), word(&machine, 48)), (1, 0));
        assert_eq!(
            (word(&machine, 52), word(&machine, 56), word(&machine, 60)),
            (0, 37, 1)
        );
        assert_eq!((word(&machine, 64), word(&machine, 68)), (u32::MAX, 6));
        assert_eq!(word(&machine, 0x100), 1);
    }
}

#[test]
fn initial_thread_image_entry_consumes_modified_routine_and_parameter() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = machine(jit, 0x1000, &services);
        let replacement = WORKER + 0x100;
        let mut worker = Asm::new(WORKER);
        worker.mov_mem_imm(DATA + 0x100, 1).ret_n(4);
        machine.cpu.write(WORKER, &worker.bytes).unwrap();
        let mut alternate = Asm::new(replacement);
        alternate.mov_eax_esp(0).mov_mem_eax(DATA + 0x104);
        alternate.mov_eax_fs(0).mov_mem_eax(DATA + 0x108);
        alternate.mov_eax_ptr_eax().mov_mem_eax(DATA + 0x10c);
        alternate
            .mov_eax_mem(DATA + 0x108)
            .add_eax_imm(4)
            .mov_eax_ptr_eax()
            .mov_mem_eax(DATA + 0x110);
        alternate.mov_eax_esp(4).add_eax_imm(37).ret_n(4);
        machine.cpu.write(replacement, &alternate.bytes).unwrap();
        machine.cpu.write_u32(CONTEXT, 0x10007).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(WORKER), I(0x1234), I(4), I(0)],
            DATA,
        );
        call(
            &mut machine,
            &mut code,
            "GetThreadContext",
            &[M(DATA), I(CONTEXT)],
            DATA + 4,
        );
        for (offset, output) in [(0xb8, 8), (0xb0, 12), (0xa4, 16)] {
            code.mov_eax_mem(CONTEXT + offset)
                .mov_mem_eax(DATA + output);
        }
        code.mov_mem_imm(CONTEXT + 0xb0, replacement);
        code.mov_mem_imm(CONTEXT + 0xa4, 41);
        call(
            &mut machine,
            &mut code,
            "SetThreadContext",
            &[M(DATA), I(CONTEXT)],
            DATA + 20,
        );
        call(
            &mut machine,
            &mut code,
            "ResumeThread",
            &[M(DATA)],
            DATA + 24,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA), I(u32::MAX)],
            DATA + 28,
        );
        call(
            &mut machine,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA), I(DATA + 32)],
            DATA + 36,
        );
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        run(&mut machine, None, &services, code);
        assert_eq!(word(&machine, 4), 1);
        assert_eq!((word(&machine, 12), word(&machine, 16)), (WORKER, 0x1234));
        assert_eq!(
            (word(&machine, 20), word(&machine, 24), word(&machine, 28)),
            (1, 1, 0)
        );
        assert_eq!((word(&machine, 32), word(&machine, 36)), (78, 1));
        assert_eq!(word(&machine, 0x100), 0, "original routine never executes");
        let modules = adv_os::modules(&machine.ext);
        let ntdll = modules.by_name("ntdll.dll").unwrap();
        assert_eq!(modules.containing(word(&machine, 8)), Some(ntdll));
        assert_eq!(modules.containing(word(&machine, 0x104)), Some(ntdll));
        let registration = word(&machine, 0x108);
        let context = &machine.sys.contexts[1];
        assert!((context.stack_lo..context.stack_hi - 7).contains(&registration));
        assert_eq!(word(&machine, 0x10c), u32::MAX);
        assert_eq!(modules.containing(word(&machine, 0x110)), Some(ntdll));
    }
}

#[test]
fn two_hundred_and_one_thousand_sequential_thread_calls_preserve_old_exit_codes() {
    for jit in [false, true] {
        for count in [200u32, 1000] {
            let services = Services::default();
            let mut machine = machine(jit, 0x1000, &services);
            // This is a lifetime test, not a per-process throughput deadline.
            // Keep a bounded allowance for 1000 interpreter mapping lifetimes.
            machine.set_execution_budget(Some(std::time::Duration::from_secs(20)));
            let mut worker = Asm::new(WORKER);
            worker.mov_eax_esp(4).ret_n(4);
            machine.cpu.write(WORKER, &worker.bytes).unwrap();
            let failure = CODE + 0x7000;
            let mut failed = Asm::new(failure);
            call(&mut machine, &mut failed, "ExitProcess", &[I(99)], 0);
            machine.cpu.write(failure, &failed.bytes).unwrap();
            let mut code = Asm::new(CODE);
            code.mov_mem_imm(DATA + 0x100, 0);
            let start = code.here();
            code.mov_eax_mem(DATA + 0x100)
                .add_eax_imm(1)
                .mov_mem_eax(DATA + 0x104);
            call(
                &mut machine,
                &mut code,
                "CreateThread",
                &[I(0), I(0x10000), I(WORKER), M(DATA + 0x104), I(0), I(0)],
                DATA,
            );
            code.bytes.extend_from_slice(&[0x85, 0xc0]); // test eax,eax
            code.jz(failure);
            call(
                &mut machine,
                &mut code,
                "WaitForSingleObject",
                &[M(DATA), I(u32::MAX)],
                0,
            );
            code.bytes.extend_from_slice(&[0x85, 0xc0]);
            code.jnz(failure);
            call(
                &mut machine,
                &mut code,
                "GetExitCodeThread",
                &[M(DATA), I(DATA + 4)],
                0,
            );
            code.bytes.extend_from_slice(&[0x83, 0xf8, 1]); // cmp eax,1
            code.jnz(failure);
            code.mov_eax_mem(DATA + 4);
            code.bytes.extend_from_slice(&[0x3b, 0x05]); // cmp eax,[sequence]
            code.bytes.extend_from_slice(&(DATA + 0x104).to_le_bytes());
            code.jnz(failure);
            code.mov_mem_eax(DATA + 0x100);
            code.bytes.push(0x3d); // cmp eax,count
            code.bytes.extend_from_slice(&count.to_le_bytes());
            code.jnz(start);
            call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
            run(&mut machine, None, &services, code);
            assert_eq!(word(&machine, 0x100), count);
            for sequence in 1..=count {
                let context = &machine.sys.contexts[sequence as usize];
                assert_eq!(context.exit_code(), Some(sequence));
                assert!(
                    matches!(machine.sys.objects.get(context.thread_handle), Some(Object::Thread { ctx }) if *ctx == context.id)
                );
                assert_eq!(context.completion.exit_code(), Some(sequence));
            }
        }
    }
}

#[test]
fn main_entry_and_callback_returns_belong_to_the_resident_system_image() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = machine(jit, 0x1000, &services);
        let mut code = Asm::new(CODE);
        code.mov_eax_esp(0).mov_mem_eax(DATA);
        code.mov_eax_fs(0).mov_mem_eax(DATA + 4);
        code.mov_eax_ptr_eax().mov_mem_eax(DATA + 8);
        code.mov_eax_mem(DATA + 4)
            .add_eax_imm(4)
            .mov_eax_ptr_eax()
            .mov_mem_eax(DATA + 12);
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        machine.cpu.write(CODE, &code.bytes).unwrap();
        let main = machine
            .start_main(CODE, CODE, 0x10000, "authored main")
            .unwrap();
        let ntdll = adv_os::modules(&machine.ext).by_name("ntdll.dll").unwrap();
        machine
            .visit_context(Some(main), |env| {
                assert_eq!(env.cpu.reg(Reg::Eax), CODE);
                assert_eq!(env.cpu.reg(Reg::Ebx), 0);
                assert_eq!(
                    adv_os::modules(env.ext).containing(env.cpu.reg(Reg::Eip)),
                    Some(ntdll)
                );
            })
            .unwrap();
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        let modules = adv_os::modules(&machine.ext);
        assert_eq!(modules.containing(word(&machine, 0)), Some(ntdll));
        assert_ne!(word(&machine, 4), u32::MAX);
        assert_eq!(word(&machine, 8), u32::MAX);
        assert_eq!(modules.containing(word(&machine, 12)), Some(ntdll));
    }
}

#[test]
fn terminal_registration_declines_unhandled_exceptions_and_unwind_notifications() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = machine(jit, 0x1000, &services);
        Win32::prepare_process_infrastructure(&mut machine).unwrap();
        let handler = machine.sys.thread_exception_entry().unwrap();
        let mut code = Asm::new(CODE);
        // Both ordinary dispatch and unwind flags keep searching; this frame
        // must never claim it handled an exception or fabricate a recovery PC.
        for flags in [0, 2] {
            code.mov_mem_imm(DATA + 0x204, flags);
            for value in [0, 0, 0, DATA + 0x200] {
                code.push_imm(value);
            }
            code.call(handler).mov_mem_eax(DATA + flags * 4);
        }
        call(
            &mut machine,
            &mut code,
            "RaiseException",
            &[I(0xe1234567), I(0), I(0), I(0)],
            0,
        );
        machine.cpu.write(CODE, &code.bytes).unwrap();
        machine
            .start_main(CODE, CODE, 0x10000, "uncaught authored main")
            .unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(
            matches!(result, RunResult::Failed(ref error) if error.message.contains("unhandled Windows exception")),
            "{result:?}"
        );
        assert_eq!(word(&machine, 0), 1);
        assert_eq!(word(&machine, 8), 1);
        assert_eq!(machine.sys.guest_failure_code, Some(0xe1234567));
    }
}

#[test]
// spec: PERS-BRIDGE-3
fn create_thread_observes_live_native_hooks_parameter_changes_and_failure_status() {
    for jit in [false, true] {
        for deny in [false, true] {
            let services = Services::default();
            let mut machine = machine(jit, 0x1000, &services);
            Win32::prepare_process_infrastructure(&mut machine).unwrap();
            let binding = machine
                .resolve_import("ntdll.dll", "NtCreateThreadEx")
                .unwrap();
            let index =
                (binding - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
            let original = machine.sys.thunks.pristine_entry(index);
            let trampoline = CODE + 0x3000;
            machine.cpu.write(trampoline, &original).unwrap();
            let modules = adv_os::modules(&machine.ext);
            let ntdll = modules.get(modules.by_name("ntdll.dll").unwrap());
            let entry = match ntdll.exports_by_name.get("NtCreateThreadEx").unwrap() {
                adv_pe::ExportTarget::Rva(rva) => ntdll.base + rva,
                _ => panic!("native create entry must be owned code"),
            };
            let hook = CODE + 0x2000;
            let mut detour = Asm::new(hook);
            detour
                .mov_eax_mem(DATA + 0x200)
                .add_eax_imm(1)
                .mov_mem_eax(DATA + 0x200);
            if deny {
                detour.bytes.push(0xb8); // mov eax,STATUS_ACCESS_DENIED
                detour.bytes.extend_from_slice(&0xc0000022u32.to_le_bytes());
                detour.ret_n(44);
            } else {
                // NtCreateThreadEx's sixth argument is the real start parameter.
                detour.bytes.extend_from_slice(&[0xc7, 0x44, 0x24, 24]);
                detour.bytes.extend_from_slice(&77u32.to_le_bytes());
                detour.jmp(trampoline);
            }
            machine.cpu.write(hook, &detour.bytes).unwrap();
            let mut patch = Asm::new(entry);
            patch.jmp(hook);
            machine.cpu.write(entry, &patch.bytes).unwrap();
            let mut worker = Asm::new(WORKER);
            worker.mov_eax_esp(4).ret_n(4);
            machine.cpu.write(WORKER, &worker.bytes).unwrap();
            let mut code = Asm::new(CODE);
            call(&mut machine, &mut code, "SetLastError", &[I(0x12345678)], 0);
            call(
                &mut machine,
                &mut code,
                "CreateThread",
                &[I(0), I(0), I(WORKER), I(11), I(0), I(0)],
                DATA,
            );
            call(&mut machine, &mut code, "GetLastError", &[], DATA + 4);
            if !deny {
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
            }
            call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
            run(&mut machine, None, &services, code);
            assert_eq!(
                word(&machine, 0x200),
                1,
                "the live native export is called once"
            );
            if deny {
                assert_eq!((word(&machine, 0), word(&machine, 4)), (0, 5));
                assert_eq!(
                    machine.sys.contexts.len(),
                    1,
                    "denied native create has no thread side effect"
                );
            } else {
                assert_ne!(word(&machine, 0), 0);
                assert_eq!(word(&machine, 4), 0x12345678);
                assert_eq!(
                    (word(&machine, 8), word(&machine, 12), word(&machine, 16)),
                    (0, 77, 1)
                );
            }
        }
    }
}

#[test]
fn remote_identities_survive_duplicate_close_and_publish_thread_and_process_exit_codes() {
    for jit in [false, true] {
        let services = Services::default();
        let mut parent = machine(jit, 0x1000, &services);
        let mut child = machine(jit, 0x2000, &services);
        child.spawn("child-main", CODE, &[], 0x10000).unwrap();
        let worker = child.spawn("child-worker", WORKER, &[], 0x10000).unwrap();
        child.sys.suspend_context(worker).unwrap();
        let identity = child.sys.thread_identity(worker).unwrap();
        let handle = parent.sys.objects.insert(Object::RemoteThread {
            identity: identity.clone(),
            access: 0x1fffff,
        });
        parent.cpu.write_u32(DATA, handle).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut parent,
            &mut code,
            "OpenProcess",
            &[I(0x1fffff), I(0), I(0x2000)],
            DATA + 4,
        );
        call(
            &mut parent,
            &mut code,
            "GetExitCodeProcess",
            &[M(DATA + 4), I(DATA + 8)],
            DATA + 12,
        );
        call(
            &mut parent,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 4), I(0)],
            DATA + 16,
        );
        call(
            &mut parent,
            &mut code,
            "ResumeThread",
            &[M(DATA)],
            DATA + 20,
        );
        call(
            &mut parent,
            &mut code,
            "SuspendThread",
            &[M(DATA)],
            DATA + 24,
        );
        // Give the child another handle to its own worker, then duplicate that
        // foreign alias back into the caller. Neither alias is a context index.
        call(
            &mut parent,
            &mut code,
            "DuplicateHandle",
            &[
                I(CURRENT_PROCESS),
                M(DATA),
                M(DATA + 4),
                I(DATA + 28),
                I(0),
                I(0),
                I(2),
            ],
            DATA + 32,
        );
        call(
            &mut parent,
            &mut code,
            "DuplicateHandle",
            &[
                M(DATA + 4),
                M(DATA + 28),
                I(CURRENT_PROCESS),
                I(DATA + 36),
                I(0),
                I(0),
                I(3),
            ],
            DATA + 40,
        );
        call(&mut parent, &mut code, "CloseHandle", &[M(DATA)], DATA + 44);
        call(
            &mut parent,
            &mut code,
            "DuplicateHandle",
            &[
                I(CURRENT_PROCESS),
                M(DATA + 36),
                I(CURRENT_PROCESS),
                I(DATA + 48),
                I(2),
                I(0),
                I(0),
            ],
            DATA + 52,
        );
        call(
            &mut parent,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA + 48), I(DATA + 56)],
            DATA + 60,
        );
        call(&mut parent, &mut code, "GetLastError", &[], DATA + 64);
        call(
            &mut parent,
            &mut code,
            "TerminateThread",
            &[M(DATA + 36), I(77)],
            DATA + 68,
        );
        call(
            &mut parent,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA + 36), I(DATA + 72)],
            DATA + 76,
        );
        call(
            &mut parent,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 36), I(0)],
            DATA + 80,
        );
        call(
            &mut parent,
            &mut code,
            "TerminateProcess",
            &[M(DATA + 4), I(88)],
            DATA + 84,
        );
        call(
            &mut parent,
            &mut code,
            "GetExitCodeProcess",
            &[M(DATA + 4), I(DATA + 88)],
            DATA + 92,
        );
        call(
            &mut parent,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 4), I(0)],
            DATA + 96,
        );
        call(
            &mut parent,
            &mut code,
            "GetExitCodeThread",
            &[M(DATA + 36), I(DATA + 100)],
            DATA + 104,
        );
        call(
            &mut parent,
            &mut code,
            "OpenProcess",
            &[I(0), I(0), I(0)],
            DATA + 108,
        );
        call(&mut parent, &mut code, "GetLastError", &[], DATA + 112);
        call(&mut parent, &mut code, "ExitProcess", &[I(0)], 0);
        run(&mut parent, Some(&mut child), &services, code);
        assert_ne!(word(&parent, 4), CURRENT_PROCESS);
        assert_eq!(
            (word(&parent, 8), word(&parent, 12), word(&parent, 16)),
            (259, 1, 258)
        );
        assert_eq!((word(&parent, 20), word(&parent, 24)), (1, 0));
        assert_eq!(
            (
                word(&parent, 32),
                word(&parent, 40),
                word(&parent, 44),
                word(&parent, 52)
            ),
            (1, 1, 1, 1)
        );
        assert!(
            child.sys.objects.get(word(&parent, 28)).is_none(),
            "DUPLICATE_CLOSE_SOURCE closes the source process alias"
        );
        assert_eq!((word(&parent, 60), word(&parent, 64)), (0, 5));
        assert_eq!(
            (
                word(&parent, 68),
                word(&parent, 72),
                word(&parent, 76),
                word(&parent, 80)
            ),
            (1, 77, 1, 0)
        );
        assert_eq!(
            (
                word(&parent, 84),
                word(&parent, 88),
                word(&parent, 92),
                word(&parent, 96)
            ),
            (1, 88, 1, 0)
        );
        assert_eq!((word(&parent, 100), word(&parent, 104)), (77, 1));
        assert_eq!((word(&parent, 108), word(&parent, 112)), (0, 87));
        assert_eq!(identity.completion.exit_code(), Some(77));
        assert_eq!(child.sys.process_identity.completion.exit_code(), Some(88));
        assert!(child.sys.contexts.iter().all(|context| context.is_done()));
    }
}

#[test]
fn suspending_the_calling_thread_parks_until_a_peer_resumes_it() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = machine(jit, 0x1000, &services);
        let mut worker = Asm::new(WORKER);
        call(
            &mut machine,
            &mut worker,
            "ResumeThread",
            &[M(DATA)],
            DATA + 12,
        );
        worker.push_imm(0);
        worker.bytes.push(0x58);
        worker.ret_n(4);
        machine.cpu.write(WORKER, &worker.bytes).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "DuplicateHandle",
            &[
                I(CURRENT_PROCESS),
                I(u32::MAX - 1),
                I(CURRENT_PROCESS),
                I(DATA),
                I(0),
                I(0),
                I(2),
            ],
            DATA + 4,
        );
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(WORKER), I(0), I(0), I(0)],
            DATA + 8,
        );
        call(
            &mut machine,
            &mut code,
            "SuspendThread",
            &[M(DATA)],
            DATA + 16,
        );
        call(
            &mut machine,
            &mut code,
            "WaitForSingleObject",
            &[M(DATA + 8), I(u32::MAX)],
            DATA + 20,
        );
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        run(&mut machine, None, &services, code);
        assert_eq!(
            (
                word(&machine, 4),
                word(&machine, 12),
                word(&machine, 16),
                word(&machine, 20)
            ),
            (1, 1, 0, 0)
        );
    }
}

#[test]
fn maximum_suspend_count_and_invalid_creation_outputs_leave_thread_state_unchanged() {
    for jit in [false, true] {
        let services = Services::default();
        let mut machine = machine(jit, 0x1000, &services);
        let mut code = Asm::new(CODE);
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(WORKER), I(0), I(4), I(0)],
            DATA,
        );
        for _ in 0..126 {
            call(&mut machine, &mut code, "SuspendThread", &[M(DATA)], 0);
        }
        call(
            &mut machine,
            &mut code,
            "SuspendThread",
            &[M(DATA)],
            DATA + 4,
        );
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 8);
        call(
            &mut machine,
            &mut code,
            "ResumeThread",
            &[M(DATA)],
            DATA + 12,
        );
        call(
            &mut machine,
            &mut code,
            "TerminateThread",
            &[M(DATA), I(9)],
            DATA + 16,
        );
        call(
            &mut machine,
            &mut code,
            "CreateThread",
            &[I(0), I(0), I(WORKER), I(0), I(4), I(0xfffffff0)],
            DATA + 20,
        );
        // Extra Win32 creation flags are accepted, so only the unwritable
        // thread id output remains an invalid creation here.
        call(&mut machine, &mut code, "GetLastError", &[], DATA + 24);
        call(&mut machine, &mut code, "ExitProcess", &[I(0)], 0);
        run(&mut machine, None, &services, code);
        assert_eq!(
            (
                word(&machine, 4),
                word(&machine, 8),
                word(&machine, 12),
                word(&machine, 16)
            ),
            (u32::MAX, 156, 127, 1)
        );
        assert_eq!((word(&machine, 20), word(&machine, 24)), (0, 998));
        assert_eq!(
            machine.sys.contexts.len(),
            2,
            "failed creation adds no context"
        );
        assert_eq!(machine.sys.contexts[1].exit_code(), Some(9));
    }
}
