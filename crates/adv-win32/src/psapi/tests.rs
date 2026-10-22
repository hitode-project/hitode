use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, Object, RunResult,
    layout::{PARK_ADDR, PEB_ADDR, peb},
};
use adv_os::{Module, ModuleKind, module::Lifetime};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const MAIN: u32 = 0x500000;
const DLL: u32 = 0x510000;
const MAIN_PATH: &str = "C:\\before\\Main.Exe";
const DLL_PATH: &str = "C:\\before\\資材\\MiXeD日本.Dll";
const MARKER: u32 = 0x12345678;
fn module(cpu: &mut dyn Guest, ext: &mut adv_machine::Ext, base: u32, path: &str, is_dll: bool) {
    let ldr = base + 0x10000;
    cpu.map(base, 4096, Prot::RX).unwrap();
    cpu.map(ldr, 4096, Prot::RW).unwrap();
    let text: Vec<_> = path.encode_utf16().collect();
    cpu.write(ldr + 0x24, &(text.len() as u16 * 2).to_le_bytes())
        .unwrap();
    cpu.write(ldr + 0x26, &(text.len() as u16 * 2 + 2).to_le_bytes())
        .unwrap();
    cpu.write_u32(ldr + 0x28, ldr + 0x80).unwrap();
    cpu.write(
        ldr + 0x80,
        &text
            .into_iter()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    adv_os::modules_mut(ext).add(Module {
        name: path.rsplit('\\').next().unwrap().to_ascii_lowercase(),
        path: "relative.dll".into(),
        kind: ModuleKind::Image,
        base,
        size: 4096,
        is_dll,
        entry: base + 0x100,
        stack_reserve: 65536,
        exports_by_name: Default::default(),
        exports_by_ordinal: Default::default(),
        resources: None,
        data_file: None,
        load_count: 1,
        ldr_entry: ldr,
        ldr_size: 4096,
        lifetime: Lifetime {
            mapped: true,
            ..Lifetime::default()
        },
    });
}
fn queries<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", MAIN_PATH, "target/psapi-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.write(DATA + 0x1000, &[0xcd; 4096]).unwrap();
    // LDR blocks are separate from the next image's mapping.
    module(&mut m.cpu, &mut m.ext, MAIN, MAIN_PATH, false);
    module(&mut m.cpu, &mut m.ext, DLL + 0x20000, DLL_PATH, true);
    let dll = DLL + 0x20000;
    m.cpu.write_u32(PEB_ADDR + peb::IMAGE_BASE, MAIN).unwrap();
    m.ext.get_mut::<Win32>().vfs.cwd = "C:\\after".into();
    m.spawn("process queries", CODE, &[], 65536).unwrap();
    let file = m.sys.objects.insert_host(crate::FileObj {
        file: adv_os::vfs::FileStream::read_only(std::sync::Arc::from(vec![0x41u8; 4096])),
        guest_path: "C:\\before\\mapped.bin".into(),
        access: 0x80000000,
        share: 7,
    });
    let mapped = {
        let mut env = adv_machine::Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0,
        };
        let adv_machine::Outcome::Return(section) =
            crate::kernel32::file_mapping::create_section(&mut env, file, 0, 2, 0, None)
        else {
            panic!("section creation");
        };
        assert_ne!(section, 0);
        let adv_machine::Outcome::Return(base) =
            crate::kernel32::file_mapping::map_section(&mut env, section, 4, 0, 0, 0)
        else {
            panic!("section mapping");
        };
        assert_ne!(base, 0);
        env.sys.objects.close(file);
        env.sys.objects.close(section);
        base
    };
    let limited = m.sys.objects.insert(Object::Process {
        identity: m.sys.process_identity.clone(),
        access: 0x1000,
    });
    let denied = m.sys.objects.insert(Object::Process {
        identity: m.sys.process_identity.clone(),
        access: 0x400,
    });
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    let mut call = |module: &str, name: &str, args: &[u32], value, error| {
        code.push_imm(MARKER).call(set_error);
        for arg in args.iter().rev() {
            code.push_imm(*arg);
        }
        let function = m.resolve_import(module, name).unwrap();
        let row = DATA + expected.len() as u32 * 8;
        code.call(function)
            .mov_mem_eax(row)
            .call(get_error)
            .mov_mem_eax(row + 4);
        expected.push((value, error));
    };
    for (index, dll_name) in ["psapi.dll", "kernel32.dll"].into_iter().enumerate() {
        let base = DATA + 0x1000 + index as u32 * 0x800;
        let name = |name: &str| {
            if dll_name == "kernel32.dll" {
                format!("K32{name}")
            } else {
                name.into()
            }
        };
        call(
            dll_name,
            &name("EnumProcesses"),
            &[base, 4, base + 4],
            1,
            MARKER,
        );
        call(
            dll_name,
            &name("EnumProcesses"),
            &[0, 0, base + 8],
            1,
            MARKER,
        );
        call(dll_name, &name("EnumProcesses"), &[base, 4, 0], 0, 998);
        call(
            dll_name,
            &name("EnumProcessModules"),
            &[u32::MAX, base + 0x20, 8, base + 12],
            1,
            MARKER,
        );
        call(
            dll_name,
            &name("EnumProcessModulesEx"),
            &[u32::MAX, 0, 0, base + 16, 3],
            1,
            MARKER,
        );
        call(
            dll_name,
            &name("EnumProcessModules"),
            &[denied, base + 0x20, 8, base + 12],
            0,
            5,
        );
        call(
            dll_name,
            &name("GetModuleInformation"),
            &[u32::MAX, 0, base + 0x40, 12],
            1,
            MARKER,
        );
        call(
            dll_name,
            &name("GetModuleInformation"),
            &[u32::MAX, dll, base + 0x50, 12],
            1,
            MARKER,
        );
        call(
            dll_name,
            &name("GetModuleInformation"),
            &[u32::MAX, dll, base + 0x60, 8],
            0,
            122,
        );
        call(
            dll_name,
            &name("GetModuleInformation"),
            &[u32::MAX, dll, 0, 12],
            0,
            998,
        );
        let leaf = DLL_PATH.rsplit('\\').next().unwrap();
        let ansi = encoding_rs::SHIFT_JIS.encode(leaf).0.len() as u32;
        call(
            dll_name,
            &name("GetModuleBaseNameA"),
            &[u32::MAX, dll, base + 0x80, 64],
            ansi,
            MARKER,
        );
        call(
            dll_name,
            &name("GetModuleBaseNameW"),
            &[u32::MAX, dll, base + 0x100, 64],
            leaf.encode_utf16().count() as u32,
            MARKER,
        );
        call(
            dll_name,
            &name("GetModuleBaseNameW"),
            &[denied, dll, base + 0x100, 64],
            0,
            5,
        );
        call(
            dll_name,
            &name("GetModuleFileNameExW"),
            &[u32::MAX, dll, base + 0x180, 128],
            DLL_PATH.encode_utf16().count() as u32,
            MARKER,
        );
        call(
            dll_name,
            &name("GetModuleFileNameExW"),
            &[denied, dll, base + 0x180, 128],
            0,
            5,
        );
        let native = r"\Device\HarddiskVolume1\before\Main.Exe";
        call(
            dll_name,
            &name("GetProcessImageFileNameA"),
            &[limited, base + 0x280, 128],
            native.len() as u32,
            MARKER,
        );
        call(
            dll_name,
            &name("GetProcessImageFileNameW"),
            &[u32::MAX, base + 0x300, 128],
            native.len() as u32,
            MARKER,
        );
        call(
            dll_name,
            &name("GetProcessImageFileNameW"),
            &[u32::MAX, base + 0x400, 1],
            0,
            122,
        );
        let native_dll = format!(r"\Device\HarddiskVolume1{}", &DLL_PATH[2..]);
        call(
            dll_name,
            &name("GetMappedFileNameW"),
            &[u32::MAX, dll + 0x200, base + 0x500, 128],
            native_dll.encode_utf16().count() as u32,
            MARKER,
        );
        call(
            dll_name,
            &name("GetModuleBaseNameW"),
            &[u32::MAX, dll, base + 0x680, 4],
            4,
            MARKER,
        );
        call(
            dll_name,
            &name("GetMappedFileNameW"),
            &[u32::MAX, dll, base + 0x690, 4],
            4,
            122,
        );
        call(
            dll_name,
            &name("GetMappedFileNameA"),
            &[u32::MAX, DATA, base + 0x700, 128],
            0,
            1006,
        );
        call(
            dll_name,
            &name("GetMappedFileNameW"),
            &[u32::MAX, mapped + 8, base + 0x740, 64],
            r"\Device\HarddiskVolume1\before\mapped.bin".len() as u32,
            MARKER,
        );
        call(
            dll_name,
            &name("GetModuleBaseNameW"),
            &[0, dll, base + 0x100, 64],
            0,
            6,
        );
    }
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();

    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (value, error)) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 8).unwrap(),
            value,
            "call {i}"
        );
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 8 + 4).unwrap(),
            error,
            "error {i}"
        );
    }
    let read = |pointer, length| {
        let mut bytes = vec![0; length];
        m.cpu.read(pointer, &mut bytes).unwrap();
        bytes
    };
    for index in 0..2 {
        let base = DATA + 0x1000 + index * 0x800;
        assert_eq!(m.cpu.read_u32(base).unwrap(), m.sys.pid);
        assert_eq!(m.cpu.read_u32(base + 4).unwrap(), 4);
        assert_eq!(m.cpu.read_u32(base + 8).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 12).unwrap(), 8);
        assert_eq!(m.cpu.read_u32(base + 16).unwrap(), 8);
        assert_eq!(
            read(base + 0x20, 8),
            [MAIN, dll]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            read(base + 0x40, 12),
            [MAIN, 4096, MAIN + 0x100]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>()
        );
        assert_eq!(read(base + 0x60, 12), [0xcd; 12]);
        let leaf = DLL_PATH.rsplit('\\').next().unwrap();
        let mut ansi = encoding_rs::SHIFT_JIS.encode(leaf).0.into_owned();
        ansi.push(0);
        assert_eq!(read(base + 0x80, ansi.len()), ansi);
        let wide = |text: &str| {
            text.encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>()
        };
        let expected = wide(DLL_PATH);
        assert_eq!(read(base + 0x180, expected.len()), expected);
        let expected = wide(r"\Device\HarddiskVolume1\before\Main.Exe");
        assert_eq!(read(base + 0x300, expected.len()), expected);
        let expected = wide(&format!(r"\Device\HarddiskVolume1{}", &DLL_PATH[2..]));
        assert_eq!(read(base + 0x500, expected.len()), expected);
        assert_eq!(read(base + 0x680, 8), wide("MiX"));
        assert_eq!(read(base + 0x690, 8), wide(r"\De"));
        let expected = wide(r"\Device\HarddiskVolume1\before\mapped.bin");
        assert_eq!(read(base + 0x740, expected.len()), expected);
        assert_eq!(read(base + 0x400, 8), [0xcd; 8]);
        assert_eq!(read(base + 0x700, 8), [0xcd; 8]);
    }
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict))
    );
}
#[test]
// spec: SYS-PROCESS-1
fn psapi_and_k32_read_the_same_owners_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        queries(cpu);
    }
    queries(UnicornCpu::new(PARK_ADDR).unwrap());
}
