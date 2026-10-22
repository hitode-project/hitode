//! Later SDK declarations must not turn optional Win7 probes into false capabilities.
use adv_abi::{Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Process, modules};
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

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry
}

#[test]
// spec: PERS-EXPORT-2
fn excluded_exports_have_no_registered_implementation_or_reviewed_failure() {
    let registry = registry();
    let mut previous = None;
    for row in include_str!("../../../data/abi/post-win7.tsv").lines() {
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let fields: Vec<_> = row.split('\t').collect();
        assert_eq!(fields.len(), 4, "{row}");
        assert!(fields.iter().all(|field| !field.is_empty()), "{row}");
        let key = (fields[0], fields[1]);
        assert!(previous.is_none_or(|previous| previous < key), "{row}");
        previous = Some(key);
        assert!(!registry.is_implemented(key.0, key.1), "{row}");
        assert!(!registry.dynamic_available(key.0, key.1), "{row}");
    }
    // Pinned WinMD selects the modern requirement from these multi-version
    // Learn pages. Their earlier desktop requirements keep them in Win7 scope.
    for (dll, name) in [
        ("ws2_32.dll", "WSAStartup"),
        ("ws2_32.dll", "htons"),
        ("msi.dll", "MsiGetFileHashW"),
        ("msi.dll", "MsiEnumProductsA"),
    ] {
        assert!(registry.dynamic_available(dll, name), "{dll}!{name}");
    }
}

fn probes<B: Backend>(cpu: B, mode: RunMode) {
    let mut registry = registry();
    registry.set_mode(mode);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/win7-exports",
            "probe.exe",
            "/nonexistent/win7-exports-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let kernel = machine.load(&Empty, "kernel32.dll").unwrap();
    let base = modules(&machine.ext).get(kernel).base;
    let get = machine
        .resolve_import("kernel32.dll", "GetProcAddress")
        .unwrap();
    let error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    for (index, name) in [
        "GetCurrentPackageId",
        "SetHandleCount",
        "GetSystemTimePreciseAsFileTime",
        "CreateFile2",
        "FlsGetValue2",
        "AppPolicyGetProcessTerminationMethod",
        "GetCurrentProcessId",
    ]
    .into_iter()
    .enumerate()
    {
        let text = DATA + 256 + index as u32 * 64;
        machine
            .cpu
            .write(text, format!("{name}\0").as_bytes())
            .unwrap();
        code.push_imm(text)
            .push_imm(base)
            .call(get)
            .mov_mem_eax(DATA + index as u32 * 8);
        code.call(error).mov_mem_eax(DATA + index as u32 * 8 + 4);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.start_main(CODE, CODE, 65536, "probe.exe").unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    for index in 0..6 {
        assert_eq!(machine.cpu.read_u32(DATA + index * 8).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(DATA + index * 8 + 4).unwrap(), 127);
    }
    assert_ne!(machine.cpu.read_u32(DATA + 48).unwrap(), 0);
}

#[test]
// spec: PERS-EXPORT-2
fn optional_probes_hide_later_exports_in_both_modes_on_three_backends() {
    for mode in [RunMode::Strict, RunMode::Discovery] {
        let mut interpreter = JitCpu::new(PARK_ADDR, false).unwrap();
        interpreter.set_async_compilation(false);
        probes(interpreter, mode);
        let mut jit = JitCpu::new(PARK_ADDR, true).unwrap();
        jit.set_async_compilation(false);
        probes(jit, mode);
        probes(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}
