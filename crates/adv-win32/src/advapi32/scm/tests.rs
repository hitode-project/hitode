use super::*;
use adv_abi::{ApiDiagnostics, ImplementationLevel, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const NAMES: u32 = 0x420000;

fn guest_calls<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(registry.mode(), RunMode::Strict);
    for name in ["OpenSCManagerA", "OpenSCManagerW"] {
        assert_eq!(
            registry.get("advapi32.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 3 }
        );
        assert_eq!(
            registry.level("advapi32.dll", name),
            Some(ImplementationLevel::SafeFail)
        );
        assert!(registry.dynamic_available("advapi32.dll", name));
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new("/nonexistent/scm", "fixture.exe", "/nonexistent/scm-data"),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(NAMES, 4096, Prot::READ).unwrap();
    let mut names = vec![0; 4096];
    for (offset, text) in [
        (0, "."),
        (64, "ServicesActive"),
        (128, "\\\\fixture-remote.invalid"),
        (192, "FixtureDatabase"),
    ] {
        names[offset..offset + text.len()].copy_from_slice(text.as_bytes());
        let wide: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        names[512 + offset * 2..512 + offset * 2 + wide.len()].copy_from_slice(&wide);
    }
    machine.cpu.write(NAMES, &names).unwrap();
    let open_a = machine
        .resolve_import("advapi32.dll", "OpenSCManagerA")
        .unwrap();
    let open_w = machine
        .resolve_import("advapi32.dll", "OpenSCManagerW")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    let mut cases = 0;
    for (open, offset, width) in [(open_a, 0, 1), (open_w, 512, 2)] {
        for (name, database, access) in [
            (0, 0, 4),
            (NAMES + offset, NAMES + offset + 64 * width, 4),
            (
                NAMES + offset + 128 * width,
                NAMES + offset + 192 * width,
                0xf003f,
            ),
        ] {
            let result = DATA + cases * 16;
            code.push_imm(0x13572468).call(set_error);
            code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
            code.mov_mem_eax(result + 8);
            code.push_imm(access)
                .push_imm(database)
                .push_imm(name)
                .call(open)
                .mov_mem_eax(result);
            code.call(get_error).mov_mem_eax(result + 4);
            code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
            code.mov_mem_eax(result + 12);
            cases += 1;
        }
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("SCM safe failure", CODE, &[], 65536).unwrap();
    let objects_before = machine.sys.objects.object_count();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.sys.objects.object_count(), objects_before);
    for case in 0..cases {
        let result = DATA + case * 16;
        assert_eq!(machine.cpu.read_u32(result).unwrap(), 0);
        assert_eq!(
            machine.cpu.read_u32(result + 4).unwrap(),
            ERROR_CALL_NOT_IMPLEMENTED
        );
        assert_eq!(
            machine.cpu.read_u32(result + 8).unwrap(),
            machine.cpu.read_u32(result + 12).unwrap(),
            "stdcall3 cleans its arguments"
        );
    }
    let mut unchanged = vec![0; names.len()];
    machine.cpu.read(NAMES, &mut unchanged).unwrap();
    assert_eq!(unchanged, names);
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.gaps.is_empty());
    assert_eq!(diagnostics.warnings.len(), 2);
    assert_eq!(diagnostics.unrecorded_warnings, 0);
    for name in ["OpenSCManagerA", "OpenSCManagerW"] {
        assert_eq!(
            diagnostics
                .warnings
                .get(&(format!("advapi32.dll!{name}"), UNAVAILABLE.into())),
            Some(&3)
        );
    }
}

#[test]
fn scm_open_is_a_typed_reported_failure_without_objects_for_local_or_remote_requests() {
    guest_calls(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_calls(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
    jit.set_async_compilation(false);
    guest_calls(jit);
}
