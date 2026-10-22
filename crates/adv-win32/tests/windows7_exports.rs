//! Guest GetProcAddress hides verified later-client exports in both API modes.
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
use std::collections::BTreeMap;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const RESULTS: u32 = DATA + 8192;
#[path = "support/test_cpu.rs"]
mod test_cpu;
use test_cpu::TestCpu;

fn check<B: TestCpu>(cpu: B, mode: RunMode, expose: bool) {
    let mut registry = Registry::default();
    registry.set_mode(mode);
    registry.set_expose_fixed_exports(expose);
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/windows7-export-tests");
    Win32::install(&mut machine, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    machine.ext.get_or_default::<ApiDiagnostics>();
    machine.cpu.map(CODE, 8192, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    let load = machine
        .resolve_import("kernel32.dll", "LoadLibraryA")
        .unwrap();
    let lookup = machine
        .resolve_import("kernel32.dll", "GetProcAddress")
        .unwrap();
    let last_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let scope = [
        "user32.dll",
        "comctl32.dll",
        "comdlg32.dll",
        "imm32.dll",
        "gdi32.dll",
        "msimg32.dll",
        "gdiplus.dll",
        "ddraw.dll",
        "d3d9.dll",
        "winmm.dll",
        "avifil32.dll",
        "msacm32.dll",
        "msvfw32.dll",
        "dsound.dll",
    ];
    let mut strings = DATA + 128;
    let mut modules = BTreeMap::new();
    let mut cases = Vec::new();
    let mut code = Asm::new(CODE);
    let later: Vec<_> = include_str!("../../../data/abi/post-win7.tsv")
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let fields: Vec<_> = line.split('\t').collect();
            (fields[0].to_owned(), fields[1])
        })
        .collect();
    for (dll, name) in later
        .iter()
        .filter(|(dll, _)| scope.contains(&dll.as_str()))
    {
        let module = *modules.entry(dll.clone()).or_insert_with(|| {
            let slot = DATA + 4 * (scope.iter().position(|module| *module == dll).unwrap() as u32);
            let bytes = format!("{dll}\0");
            machine.cpu.write(strings, bytes.as_bytes()).unwrap();
            code.push_imm(strings).call(load).mov_mem_eax(slot);
            strings += bytes.len() as u32;
            slot
        });
        let name_at = strings;
        machine
            .cpu
            .write(name_at, format!("{name}\0").as_bytes())
            .unwrap();
        strings += name.len() as u32 + 1;
        let out = RESULTS + cases.len() as u32 * 8;
        code.push_imm(name_at)
            .mov_eax_mem(module)
            .push_eax()
            .call(lookup)
            .mov_mem_eax(out);
        code.call(last_error).mov_mem_eax(out + 4);
        cases.push((dll, name, out));
    }
    assert!(!cases.is_empty());
    assert!(strings < RESULTS);
    code.ret();
    assert!(code.bytes.len() <= 8192);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.code_region(CODE, code.bytes.len());
    machine
        .spawn("Windows 7 export guest", CODE, &[], 65536)
        .unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(_)
    ));
    for (dll, slot) in modules {
        assert_ne!(machine.cpu.read_u32(slot).unwrap(), 0, "load {dll}");
    }
    for (dll, name, out) in &cases {
        assert_eq!(machine.cpu.read_u32(*out).unwrap(), 0, "{dll}!{name}");
        assert_eq!(
            machine.cpu.read_u32(*out + 4).unwrap(),
            127,
            "{dll}!{name} error"
        );
    }
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.dynamic_lookup_failure_count, cases.len() as u64);
    assert!(diagnostics.warnings.is_empty());
}
#[test]
// spec: PERS-EXPORT-2
fn windows7_later_apis_are_absent_in_guest_lookup() {
    for mode in [RunMode::Strict, RunMode::Discovery] {
        for expose in [false, true] {
            check(JitCpu::new(PARK_ADDR, false).unwrap(), mode, expose);
            check(JitCpu::new(PARK_ADDR, true).unwrap(), mode, expose);
            check(UnicornCpu::new(PARK_ADDR).unwrap(), mode, expose);
        }
    }
}
