use super::*;
use adv_abi::{ApiDiagnostics, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Conv, Machine, Object, RunResult,
    layout::{GRANULE, PARK_ADDR},
};
use adv_os::processes;
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const SENTINEL: u32 = 0x13572468;

fn machine<B: Backend>(cpu: B, pid: u32) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(registry.mode(), RunMode::Strict);
    for name in ["NtOpenThread", "ZwOpenThread"] {
        assert_eq!(
            registry.get("ntdll.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 4 }
        );
        assert!(registry.dynamic_available("ntdll.dll", name));
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine.ext.insert(ApiDiagnostics::default());
    machine.sys.set_process_identity(pid).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(
            "/nonexistent/thread-open",
            "fixture.exe",
            "/nonexistent/thread-open-data",
        ),
    )
    .unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.write_u32(DATA, 24).unwrap();
    machine.cpu.write_u32(DATA + 32, pid).unwrap();
    machine.cpu.write_u32(DATA + 36, pid + 4).unwrap();
    machine.cpu.write_u32(DATA + 64, SENTINEL).unwrap();
    machine
}

fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn returned(outcome: Outcome, expected: u32) {
    assert!(matches!(outcome, Outcome::Return(value) if value == expected));
}
fn nt<B: Backend>(machine: &mut Machine<B>) -> Outcome {
    open(
        &mut env(machine),
        GuestPtr::new(DATA + 64),
        0x40,
        GuestPtr::new(DATA),
        GuestPtr::new(DATA + 32),
    )
}

fn local_lifetime<B: Backend>(cpu: B) {
    let mut machine = machine(cpu, 0x1000);
    machine.spawn("thread", PARK_ADDR, &[], GRANULE).unwrap();
    let original = machine.sys.contexts[0].thread_handle;
    let duplicate = machine.sys.objects.duplicate(original).unwrap();
    machine.sys.objects.close(original);
    machine.sys.terminate_context_stopped(0, 21);
    crate::util::set_last_error(&mut env(&mut machine), SENTINEL);
    returned(nt(&mut machine), 0);
    let opened = machine.cpu.read_u32(DATA + 64).unwrap();
    assert!(
        matches!(machine.sys.objects.get(opened), Some(Object::RemoteThread { identity, access }) if identity.completion.exit_code() == Some(21) && *access == 0x840)
    );
    machine.sys.objects.close(duplicate);
    returned(nt(&mut machine), 0); // Retained opened handle is a real owner too.
    let second = machine.cpu.read_u32(DATA + 64).unwrap();
    machine.sys.objects.close(opened);
    machine.sys.objects.close(second);
    let count = machine.sys.objects.object_count();
    returned(nt(&mut machine), 0xc000000b);
    assert_eq!(machine.cpu.read_u32(DATA + 64).unwrap(), 0);
    assert_eq!(machine.sys.objects.object_count(), count);
    assert_eq!(crate::util::last_error(&env(&mut machine)), SENTINEL);
    // A bad capture preserves PHANDLE instead of leaking or clearing it.
    machine.cpu.write_u32(DATA + 64, SENTINEL).unwrap();
    machine.cpu.write_u32(DATA, 23).unwrap();
    returned(nt(&mut machine), INVALID_PARAMETER);
    assert_eq!(machine.cpu.read_u32(DATA + 64).unwrap(), SENTINEL);
    machine.cpu.write_u32(DATA, 24).unwrap();
    machine.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    returned(nt(&mut machine), ACCESS_VIOLATION);
    assert_eq!(machine.sys.objects.object_count(), count);
}

fn remote_lifetime<B: Backend>(make: impl Fn() -> B) {
    for unmap_publication in [false, true] {
        let mut parent = machine(make(), 0x1000);
        let mut child = machine(make(), 0x2000);
        parent.spawn("caller", PARK_ADDR, &[], GRANULE).unwrap();
        child.spawn("child", PARK_ADDR, &[], GRANULE).unwrap();
        child.sys.suspend_context(0);
        child.sys.objects.close(child.sys.contexts[0].thread_handle);
        let services = processes::Services::default();
        services.register(&parent.sys.process_identity);
        services.register_child(&child.sys.process_identity, parent.sys.pid);
        parent.ext.insert(services.clone());
        parent.cpu.write_u32(DATA + 32, 0x2000).unwrap();
        parent.cpu.write_u32(DATA + 36, 0x2004).unwrap();
        crate::util::set_last_error(&mut env(&mut parent), SENTINEL);
        let before = parent.sys.objects.object_count();
        let Outcome::HostRequest { then } = nt(&mut parent) else {
            panic!("requires real supervisor visit")
        };
        let processes::Request::Visit { pid, context, task } = services.pop().unwrap() else {
            panic!("visit")
        };
        assert_eq!(pid, 0x2000);
        assert_eq!(context, None);
        task(Ok(&mut env(&mut child)));
        if unmap_publication {
            parent.cpu.protect(DATA, 4096, Prot::READ).unwrap();
        }
        returned(
            then(&mut env(&mut parent)),
            if unmap_publication {
                ACCESS_VIOLATION
            } else {
                0
            },
        );
        assert_eq!(
            parent.sys.objects.object_count(),
            before + usize::from(!unmap_publication)
        );
        assert_eq!(crate::util::last_error(&env(&mut parent)), SENTINEL);
        if !unmap_publication {
            child.sys.terminate_process_stopped(37);
            assert!(
                services
                    .live_processes()
                    .iter()
                    .all(|&(pid, _)| pid != 0x2000)
            );
            returned(nt(&mut parent), 0); // Caller retains the actual exited child identity.
            assert!(!services.pending());
            let handle = parent.cpu.read_u32(DATA + 64).unwrap();
            assert!(
                matches!(parent.sys.objects.get(handle), Some(Object::RemoteThread { identity, .. }) if identity.completion.exit_code() == Some(37))
            );
        }
    }
}

#[test]
fn thread_identity_lifetime_requires_live_execution_or_retained_kernel_owner() {
    local_lifetime(UnicornCpu::new(PARK_ADDR).unwrap());
    local_lifetime(JitCpu::new(PARK_ADDR, false).unwrap());
    remote_lifetime(|| UnicornCpu::new(PARK_ADDR).unwrap());
    remote_lifetime(|| JitCpu::new(PARK_ADDR, false).unwrap());
}

fn guest<B: Backend>(cpu: B) {
    let mut machine = machine(cpu, 0x1000);
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    let nt = machine.resolve_import("ntdll.dll", "NtOpenThread").unwrap();
    let zw = machine.resolve_import("ntdll.dll", "ZwOpenThread").unwrap();
    let open = machine
        .resolve_import("kernel32.dll", "OpenThread")
        .unwrap();
    let close = machine
        .resolve_import("kernel32.dll", "CloseHandle")
        .unwrap();
    let resume = machine
        .resolve_import("kernel32.dll", "ResumeThread")
        .unwrap();
    let suspend = machine
        .resolve_import("kernel32.dll", "SuspendThread")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(SENTINEL).call(set_error);
    for (index, api) in [nt, zw].into_iter().enumerate() {
        code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
        code.mov_mem_eax(DATA + 128 + index as u32 * 20);
        code.push_imm(DATA + 32)
            .push_imm(DATA)
            .push_imm(0x40)
            .push_imm(DATA + 64)
            .call(api)
            .mov_mem_eax(DATA + 132 + index as u32 * 20);
        code.call(get_error)
            .mov_mem_eax(DATA + 136 + index as u32 * 20);
        code.bytes.extend_from_slice(&[0x89, 0xe0]);
        code.mov_mem_eax(DATA + 140 + index as u32 * 20);
        code.bytes.extend_from_slice(&[0xff, 0x35]); // push [PHANDLE]
        code.bytes.extend_from_slice(&(DATA + 64).to_le_bytes());
        code.call(close);
    }
    code.push_imm(0x1004).push_imm(1).push_imm(0x40).call(open);
    code.bytes.push(0x50); // push eax
    code.call(close).mov_mem_eax(DATA + 168);
    code.push_imm(0x1004)
        .push_imm(0)
        .push_imm(0x20000000)
        .call(open)
        .mov_mem_eax(DATA + 176);
    code.bytes.push(0x50);
    code.call(suspend).mov_mem_eax(DATA + 180);
    code.call(get_error).mov_mem_eax(DATA + 184);
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&(DATA + 176).to_le_bytes());
    code.call(resume).mov_mem_eax(DATA + 188);
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&(DATA + 176).to_le_bytes());
    code.call(close);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("open thread", CODE, &[], GRANULE).unwrap();
    let before = machine.sys.objects.object_count();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.sys.objects.object_count(), before);
    for index in 0..2 {
        let result = DATA + 128 + index * 20;
        assert_eq!(
            machine.cpu.read_u32(result).unwrap(),
            machine.cpu.read_u32(result + 12).unwrap(),
            "stdcall4 cleanup"
        );
        assert_eq!(machine.cpu.read_u32(result + 4).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(result + 8).unwrap(), SENTINEL);
    }
    assert_eq!(machine.cpu.read_u32(DATA + 168).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 180).unwrap(), u32::MAX);
    assert_eq!(machine.cpu.read_u32(DATA + 184).unwrap(), 5);
    assert_eq!(machine.cpu.read_u32(DATA + 188).unwrap(), 0);
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
    assert!(diagnostics.gaps.is_empty());
}

#[test]
fn typed_nt_zw_and_win32_guest_calls_preserve_error_and_balance_stack() {
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
    guest(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
    jit.set_async_compilation(false);
    guest(jit);
}
