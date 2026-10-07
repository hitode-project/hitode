//! Authored x86 ABI calls verify the explicit path-search failure contract.
use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};

#[test]
fn search_path_a_reports_unavailable_without_changing_outputs() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let api = registry.get("kernel32.dll", "SearchPathA").unwrap();
        assert_eq!(api.conv, adv_abi::Conv::Stdcall { argc: 6 });
        assert_eq!(
            registry.level("kernel32.dll", "SearchPathA"),
            Some(adv_abi::ImplementationLevel::SafeFail)
        );
        assert!(registry.dynamic_available("kernel32.dll", "SearchPathA"));
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/search-path-tests");
        Win32::install(
            &mut machine,
            crate::Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
        )
        .unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let (code_address, data) = (0x400000, 0x410000);
        machine.cpu.map(code_address, 4096, Prot::RX).unwrap();
        machine.cpu.map(data, 4096, Prot::RW).unwrap();
        machine.cpu.write(data + 64, b"authored.dll\0").unwrap();
        machine.cpu.write(data + 256, &[0xcc; 128]).unwrap();
        machine.cpu.write_u32(data + 4, 0x12345678).unwrap();
        let search = machine
            .resolve_import("kernel32.dll", "SearchPathA")
            .unwrap();
        let get_error = machine
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap();
        let mut code = Asm::new(code_address);
        for arg in [data + 4, data + 256, 128, 0, data + 64, 0] {
            code.push_imm(arg);
        }
        code.call(search).mov_mem_eax(data);
        code.call(get_error).mov_mem_eax(data + 8);
        code.bytes.extend_from_slice(&[0x31, 0xc0]);
        code.ret();
        machine.cpu.write(code_address, &code.bytes).unwrap();
        machine
            .cpu
            .executor_mut()
            .add_region(Region::new(code_address, code.bytes.len()))
            .unwrap();
        machine
            .spawn("authored path search", code_address, &[], 65536)
            .unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_eq!(machine.cpu.read_u32(data).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(data + 8).unwrap(), 50);
        assert_eq!(machine.cpu.read_u32(data + 4).unwrap(), 0x12345678);
        let mut output = [0; 128];
        machine.cpu.read(data + 256, &mut output).unwrap();
        assert_eq!(output, [0xcc; 128]);
        let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|key| key.0 == "kernel32.dll!SearchPathA")
        );
    }
}
