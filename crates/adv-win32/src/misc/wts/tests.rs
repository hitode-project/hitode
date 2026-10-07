use super::*;
use adv_abi::{ApiDiagnostics, ImplementationLevel};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const INPUT: u32 = 0x420000;

fn calls<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["WTSSendMessageA", "WTSSendMessageW"] {
        assert_eq!(
            registry.get("wtsapi32.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 10 }
        );
        assert_eq!(
            registry.level("wtsapi32.dll", name),
            Some(ImplementationLevel::SafeFail)
        );
        assert!(registry.dynamic_available("wtsapi32.dll", name));
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new("/nonexistent/wts", "fixture.exe", "/nonexistent/wts-data"),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(INPUT, 4096, Prot::READ).unwrap();
    let mut input = [0xa5; 4096];
    input[..6].copy_from_slice(b"title\0");
    machine.cpu.write(INPUT, &input).unwrap();
    let last_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    for (index, name) in ["WTSSendMessageA", "WTSSendMessageW"]
        .into_iter()
        .enumerate()
    {
        let call = machine.resolve_import("wtsapi32.dll", name).unwrap();
        for wait in [0, 1] {
            let output = DATA + (index as u32 * 2 + wait) * 16;
            code.bytes.extend([0x89, 0xe0]);
            code.mov_mem_eax(output + 8);
            code.push_imm(wait)
                .push_imm(INPUT + 128)
                .push_imm(0)
                .push_imm(0)
                .push_imm(6)
                .push_imm(INPUT)
                .push_imm(6)
                .push_imm(INPUT)
                .push_imm(u32::MAX)
                .push_imm(0)
                .call(call)
                .mov_mem_eax(output);
            code.call(last_error).mov_mem_eax(output + 4);
            code.bytes.extend([0x89, 0xe0]);
            code.mov_mem_eax(output + 12);
        }
    }
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("unavailable session provider", CODE, &[], 65536)
        .unwrap();
    let count = machine.sys.objects.object_count();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(machine.sys.objects.object_count(), count);
    for index in 0..4 {
        let output = DATA + index * 16;
        assert_eq!(machine.cpu.read_u32(output).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(output + 4).unwrap(), 120);
        assert_eq!(
            machine.cpu.read_u32(output + 8).unwrap(),
            machine.cpu.read_u32(output + 12).unwrap()
        );
    }
    let mut after = [0; 4096];
    machine.cpu.read(INPUT, &mut after).unwrap();
    assert_eq!(
        after, input,
        "failure leaves strings and response untouched"
    );
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.gaps.is_empty());
    assert!(diagnostics.message_boxes.is_empty());
    for name in ["WTSSendMessageA", "WTSSendMessageW"] {
        assert_eq!(
            diagnostics
                .warnings
                .get(&(format!("wtsapi32.dll!{name}"), UNAVAILABLE.into())),
            Some(&2)
        );
    }
}

#[test]
fn session_messages_fail_without_dispatch_or_response_writes_and_clean_stdcall10() {
    calls(UnicornCpu::new(PARK_ADDR).unwrap());
    calls(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
    jit.set_async_compilation(false);
    calls(jit);
}
