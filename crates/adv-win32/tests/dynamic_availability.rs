//! Authored CRT-style feature probe. No game, native executable, device or window.
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Loader, Modules, Process};
use adv_pe::ImportName;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}
fn machine<B: Backend>(cpu: B, mode: RunMode) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.set_mode(mode);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/dynamic-probe",
            "probe.exe",
            "/nonexistent/dynamic-probe-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn fallback<B: Backend>(cpu: B, mode: RunMode) {
    let mut machine = machine(cpu, mode);
    let kernel = machine.load(&Empty, "kernel32.dll").unwrap();
    let base = adv_os::modules(&machine.ext).get(kernel).base;
    let get_proc = machine
        .resolve_import("kernel32.dll", "GetProcAddress")
        .unwrap();
    let mut code = Asm::new(CODE);
    for (i, name) in [
        "FlsAlloc",
        "FlsFree",
        "FlsGetValue",
        "FlsSetValue",
        "TlsAlloc",
    ]
    .iter()
    .enumerate()
    {
        let text = DATA + 0x100 + i as u32 * 32;
        machine
            .cpu
            .write(text, format!("{name}\0").as_bytes())
            .unwrap();
        code.push_imm(text)
            .push_imm(base)
            .call(get_proc)
            .mov_mem_eax(DATA + i as u32 * 4);
    }
    code.mov_eax_mem(DATA);
    code.bytes.extend_from_slice(&[0x85, 0xc0]); // test eax,eax
    code.jz(CODE + 0x300).push_imm(0);
    code.bytes.extend_from_slice(&[0xff, 0xd0]); // FlsAlloc(NULL) if falsely advertised.
    code.bytes
        .extend_from_slice(&[0xb8, 0xfe, 0xca, 0xad, 0x0b, 0xc3]);
    let mut tls = Asm::new(CODE + 0x300);
    for (name, arguments, result) in [
        ("TlsAlloc", vec![], DATA + 32),
        ("TlsSetValue", vec![0xfeedcafe], DATA + 36),
        ("TlsGetValue", vec![], DATA + 40),
        ("TlsFree", vec![], DATA + 44),
    ] {
        for value in arguments.iter().rev() {
            tls.push_imm(*value);
        }
        if name != "TlsAlloc" {
            tls.mov_eax_mem(DATA + 32).push_eax();
        }
        tls.call(machine.resolve_import("kernel32.dll", name).unwrap())
            .mov_mem_eax(result);
    }
    tls.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.write(CODE + 0x300, &tls.bytes).unwrap();
    machine.start_main(CODE, CODE, 65536, "probe.exe").unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    for i in 0..4 {
        assert_eq!(machine.cpu.read_u32(DATA + i * 4).unwrap(), 0);
    }
    assert_ne!(machine.cpu.read_u32(DATA + 16).unwrap(), 0);
    assert_ne!(machine.cpu.read_u32(DATA + 32).unwrap(), u32::MAX);
    assert_eq!(machine.cpu.read_u32(DATA + 36).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 40).unwrap(), 0xfeedcafe);
    assert_eq!(machine.cpu.read_u32(DATA + 44).unwrap(), 1);
    assert_eq!(
        machine
            .ext
            .try_get::<ApiDiagnostics>()
            .map_or(0, |d| d.stub_calls),
        0
    );
    assert!(
        machine
            .sys
            .thunks
            .iter()
            .filter(|(_, t)| t.name.starts_with("Fls"))
            .all(|(_, t)| !t.requested)
    );
}
#[test]
fn crt_probe_reaches_real_tls_fallback_on_all_backends_and_modes() {
    for mode in [RunMode::Strict, RunMode::Discovery] {
        fallback(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        fallback(JitCpu::new(PARK_ADDR, true).unwrap(), mode);
        fallback(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}
#[test]
fn static_unimplemented_import_keeps_strict_named_failure() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, true).unwrap(), RunMode::Strict);
    let kernel = machine.load(&Empty, "kernel32.dll").unwrap();
    let entry = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    )
    .resolve(
        kernel,
        &ImportName::Name {
            hint: 0,
            name: "FlsAlloc".into(),
        },
    )
    .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0).call(entry).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .start_main(CODE, CODE, 65536, "static-probe.exe")
        .unwrap();
    let RunResult::Failed(failure) = machine.run_virtual(1_000_000) else {
        panic!("static stub must fail")
    };
    assert!(
        failure
            .message
            .contains("kernel32.dll!FlsAlloc (strict ABI stub)")
    );
    assert_eq!(machine.ext.get::<ApiDiagnostics>().stub_calls, 1);
}
