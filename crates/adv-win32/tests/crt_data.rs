//! Data IATs, genuine GetProcAddress calls and guest writes on three CPU backends.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Process};
use adv_pe::{ExportTarget, ImportName, Pe};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
#[path = "support/data_fixture.rs"]
mod fixture;
struct File(Vec<u8>);
impl FileSource for File {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        (path == "data.exe").then(|| self.0.clone())
    }
}
fn case<B: Backend>(cpu: B, module: &str) {
    let names = if module == "msvcrt.dll" {
        vec!["_acmdln", "_wcmdln", "_adjust_fdiv"]
    } else {
        vec!["_acmdln", "_wcmdln"]
    };
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/crt-data",
            "表 data.exe",
            "/nonexistent/crt-data-profile",
        ),
    )
    .unwrap();
    let id = machine
        .load(&File(fixture::image(module, &names)), "data.exe")
        .unwrap();
    let host = adv_os::modules(&machine.ext).by_name(module).unwrap();
    let base = adv_os::modules(&machine.ext).get(host).base;
    let get_proc = machine
        .resolve_import("kernel32.dll", "GetProcAddress")
        .unwrap();
    let mut code = Asm::new(fixture::ENTRY);
    for index in 0..names.len() as u32 {
        code.mov_eax_mem(fixture::IAT + index * 4)
            .mov_mem_eax(fixture::OUTPUT + index * 12);
        code.bytes.extend_from_slice(&[0x8b, 0x00]); // mov eax,[eax]
        code.mov_mem_eax(fixture::OUTPUT + index * 12 + 4);
        code.push_imm(fixture::name(index))
            .push_imm(base)
            .call(get_proc)
            .mov_mem_eax(fixture::OUTPUT + index * 12 + 8);
    }
    for (index, name) in ["GetCommandLineA", "GetCommandLineW"]
        .into_iter()
        .enumerate()
    {
        code.call(machine.resolve_import("kernel32.dll", name).unwrap())
            .mov_mem_eax(fixture::OUTPUT + 64 + index as u32 * 4);
    }
    code.mov_eax_mem(fixture::IAT)
        .bytes
        .extend_from_slice(&[0xc7, 0x00, 0x34, 0x12, 0, 0]); // guest owns the cell
    code.push_imm(fixture::name(0))
        .push_imm(base)
        .call(get_proc)
        .mov_mem_eax(fixture::OUTPUT + 72);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(fixture::ENTRY, &code.bytes).unwrap();
    machine.start_process(id).unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    let statics = machine.ext.get::<Win32>().statics.clone();
    for (index, &name) in names.iter().enumerate() {
        let offset = fixture::OUTPUT + index as u32 * 12;
        let cell = machine.cpu.read_u32(offset).unwrap();
        assert_eq!(
            cell,
            machine
                .cpu
                .read_u32(fixture::IAT + index as u32 * 4)
                .unwrap()
        );
        assert_eq!(cell, machine.cpu.read_u32(offset + 8).unwrap());
        assert!(machine.cpu.accessible(cell, 4, Prot::READ | Prot::WRITE));
        assert!(!machine.cpu.accessible(cell, 4, Prot::EXEC));
        assert_eq!(cell % 8, 0);
        if index < 2 {
            assert_eq!(
                machine.cpu.read_u32(offset + 4).unwrap(),
                statics[if index == 0 { "cmdline_a" } else { "cmdline_w" }]
            );
            assert_eq!(
                machine.cpu.read_u32(offset + 4).unwrap(),
                machine
                    .cpu
                    .read_u32(fixture::OUTPUT + 64 + index as u32 * 4)
                    .unwrap()
            );
        } else {
            assert_eq!(machine.cpu.read_u32(offset + 4).unwrap(), 0);
        }
        assert!(
            !machine
                .sys
                .thunks
                .iter()
                .any(|(_, thunk)| thunk.module == module && thunk.name == name)
        );
    }
    let cell = machine.cpu.read_u32(fixture::IAT).unwrap();
    assert_eq!(machine.cpu.read_u32(cell).unwrap(), 0x1234);
    assert_eq!(machine.cpu.read_u32(fixture::OUTPUT + 72).unwrap(), cell);
    // A local EAT ordinal is discoverable dynamically, without promising a native static ordinal.
    let target = adv_os::modules(&machine.ext).get(host).exports_by_name["_acmdln"].clone();
    let ordinal = *adv_os::modules(&machine.ext)
        .get(host)
        .exports_by_ordinal
        .iter()
        .find(|(_, value)| **value == target)
        .unwrap()
        .0;
    let address = adv_os::Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<adv_os::Modules>(),
        &File(Vec::new()),
    )
    .resolve_dynamic(host, &ImportName::Ordinal(ordinal))
    .unwrap()
    .0;
    assert_eq!(address, cell);
    // The immutable System32 file and mapped image use the same export directory and initial cell value.
    let file = adv_os::system_files::SystemFiles::default()
        .open(
            &mut machine.cpu,
            &mut machine.sys,
            &format!("C:/Windows/System32/{module}"),
        )
        .unwrap()
        .unwrap();
    let pe = Pe::parse(&file).unwrap();
    let data_rva = match target {
        ExportTarget::Rva(rva) => rva,
        _ => panic!("data"),
    };
    assert_eq!(
        pe.exports()
            .unwrap()
            .iter()
            .find(|entry| entry.name.as_deref() == Some("_acmdln"))
            .unwrap()
            .target,
        ExportTarget::Rva(data_rva)
    );
    assert_eq!(
        u32::from_le_bytes(
            file[data_rva as usize..data_rva as usize + 4]
                .try_into()
                .unwrap()
        ),
        statics["cmdline_a"]
    );
}
#[test]
fn static_and_dynamic_crt_data_are_writable_cells_not_function_thunks() {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for jit in [false, true] {
            case(JitCpu::new(PARK_ADDR, jit).unwrap(), module);
        }
        case(UnicornCpu::new(PARK_ADDR).unwrap(), module);
    }
}
#[test]
fn win32_only_install_does_not_register_or_seed_crt_exports() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/crt-data",
            "data.exe",
            "/nonexistent/crt-data-profile",
        ),
    )
    .unwrap();
    // Win32 owns the Unicode command line and PEB parameters even without CRT.
    // Installing that infrastructure must not eagerly seed the ANSI CRT string.
    let statics = &machine.ext.get::<Win32>().statics;
    assert!(statics.contains_key("cmdline_w"));
    assert!(!statics.contains_key("cmdline_a"));
    let registry = machine.sys.thunks.resolver_mut::<Registry>().unwrap();
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for name in ["_acmdln", "_wcmdln"] {
            assert!(!registry.is_implemented(module, name));
            assert!(registry.data(module, name).is_none());
        }
    }
    assert!(!registry.is_implemented("msvcrt.dll", "_adjust_fdiv"));
}
