//! Authored x86 calls preserve diagnostics and outputs when text is unavailable.
use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};

#[test]
fn error_text_a_preserves_output_buffers_and_failure_context() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        register(&mut registry);
        assert_eq!(
            registry.get("quartz.dll", "AMGetErrorTextA").unwrap().conv,
            Conv::Stdcall { argc: 3 }
        );
        assert_eq!(
            registry.level("quartz.dll", "AMGetErrorTextA"),
            Some(adv_abi::ImplementationLevel::SafeFail)
        );
        assert!(registry.dynamic_available("quartz.dll", "AMGetErrorTextA"));
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/scratch/dshow-error-text-tests");
        adv_win32::Win32::install(
            &mut machine,
            adv_win32::Win32Config::new(root.join("game"), "fixture.exe", root.join("data")),
        )
        .unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let (code_address, data, readonly) = (0x400000, 0x410000, 0x420000);
        machine.cpu.map(code_address, 4096, Prot::RX).unwrap();
        machine.cpu.map(data, 4096, Prot::RW).unwrap();
        machine.cpu.map(readonly, 4096, Prot::READ).unwrap();
        machine.cpu.write(data + 128, &[0xcc; 32]).unwrap();
        machine.cpu.write(readonly, &[0xdd; 32]).unwrap();
        let lookup = machine
            .resolve_import("quartz.dll", "AMGetErrorTextA")
            .unwrap();
        let set_error = machine
            .resolve_import("kernel32.dll", "SetLastError")
            .unwrap();
        let get_error = machine
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap();
        let mut code = Asm::new(code_address);
        code.push_imm(0x12345678).call(set_error);
        code.bytes.extend([0x89, 0xe0]);
        code.mov_mem_eax(data + 64);
        for (index, (hr, output, capacity)) in [
            (0x80004005, data + 128, 32),
            (0xa1234567, data + 128, 0),
            (0xa1234567, 0, 32),
            (0xa1234567, readonly, 32),
        ]
        .into_iter()
        .enumerate()
        {
            code.push_imm(capacity)
                .push_imm(output)
                .push_imm(hr)
                .call(lookup)
                .mov_mem_eax(data + index as u32 * 8);
            code.call(get_error)
                .mov_mem_eax(data + index as u32 * 8 + 4);
        }
        code.bytes.extend([0x89, 0xe0]);
        code.mov_mem_eax(data + 68);
        code.bytes.extend([0x31, 0xc0]);
        code.ret();
        machine.cpu.write(code_address, &code.bytes).unwrap();
        let mut region = Region::new(code_address, code.bytes.len());
        region.reachable = true;
        machine.cpu.executor_mut().add_region(region).unwrap();
        machine
            .spawn("authored DirectShow error text", code_address, &[], 65536)
            .unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for index in 0..4 {
            assert_eq!(machine.cpu.read_u32(data + index * 8).unwrap(), 0);
            assert_eq!(
                machine.cpu.read_u32(data + index * 8 + 4).unwrap(),
                0x12345678
            );
        }
        assert_eq!(
            machine.cpu.read_u32(data + 64).unwrap(),
            machine.cpu.read_u32(data + 68).unwrap()
        );
        for (address, marker) in [(data + 128, 0xcc), (readonly, 0xdd)] {
            let mut bytes = [0; 32];
            machine.cpu.read(address, &mut bytes).unwrap();
            assert_eq!(bytes, [marker; 32]);
        }
        assert!(machine.ext.try_get::<State>().is_none());
        let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
        assert_eq!(diagnostics.stub_calls, 0);
        assert!(
            diagnostics
                .warnings
                .keys()
                .any(|key| key.0 == "quartz.dll!AMGetErrorTextA")
        );
    }
}
