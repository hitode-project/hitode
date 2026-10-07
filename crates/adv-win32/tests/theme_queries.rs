//! The authored guest dynamically resolves the two classic UI capability queries.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn store_esp(code: &mut Asm, address: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}

fn classic<B: Backend>(cpu: B) {
    let names = ["IsAppThemed", "IsThemeActive"];
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for name in names {
        assert_eq!(
            registry.get("uxtheme.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 0 }
        );
        assert!(registry.dynamic_available("uxtheme.dll", name));
    }
    assert!(registry.get("uxtheme.dll", "EnableTheming").is_none());
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/theme-query-tests");
    Win32::install(&mut machine, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.write(DATA, b"uxtheme.dll\0").unwrap();
    let load = machine
        .resolve_import("kernel32.dll", "LoadLibraryA")
        .unwrap();
    let gpa = machine
        .resolve_import("kernel32.dll", "GetProcAddress")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(DATA).call(load).mov_mem_eax(DATA + 128);
    for (index, name) in names.into_iter().enumerate() {
        let text = DATA + 32 + index as u32 * 32;
        let result = DATA + 160 + index as u32 * 32;
        machine
            .cpu
            .write(text, &[name.as_bytes(), &[0]].concat())
            .unwrap();
        code.push_imm(text)
            .mov_eax_mem(DATA + 128)
            .push_eax()
            .call(gpa);
        code.mov_mem_eax(result);
        code.push_imm(0x12345678 + index as u32).call(set_error);
        store_esp(&mut code, result + 12);
        code.mov_eax_mem(result);
        code.bytes.extend_from_slice(&[0xff, 0xd0]); // Actual GPA result, stdcall zero args.
        code.mov_mem_eax(result + 4);
        store_esp(&mut code, result + 16);
        code.call(get_error).mov_mem_eax(result + 8);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("classic theme queries guest", CODE, &[], 65536)
        .unwrap();
    let outcome = machine.run_virtual(1_000_000);
    assert!(matches!(outcome, RunResult::Exited(0)), "{outcome:?}");
    assert_ne!(machine.cpu.read_u32(DATA + 128).unwrap(), 0);
    for index in 0..2 {
        let result = DATA + 160 + index * 32;
        assert_ne!(machine.cpu.read_u32(result).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(result + 4).unwrap(), 0);
        assert_eq!(
            machine.cpu.read_u32(result + 8).unwrap(),
            0x12345678 + index
        );
        assert_eq!(
            machine.cpu.read_u32(result + 12).unwrap(),
            machine.cpu.read_u32(result + 16).unwrap()
        );
    }
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.dynamic_lookup_failures.is_empty());
    for name in names {
        let matching: Vec<_> = diagnostics
            .warnings
            .iter()
            .filter(|((api, _), _)| api == &format!("uxtheme.dll!{name}"))
            .collect();
        assert_eq!(matching.len(), 1);
        let ((_, reason), count) = matching[0];
        assert!(reason.contains("classic") && reason.contains("visual styles"));
        assert_eq!(*count, 1);
    }
}

#[test]
fn dynamically_resolved_classic_theme_queries_preserve_last_error_and_warn() {
    classic(UnicornCpu::new(PARK_ADDR).unwrap());
    classic(JitCpu::new(PARK_ADDR, false).unwrap());
}
