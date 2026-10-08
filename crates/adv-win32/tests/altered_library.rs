//! Authored LoadLibraryEx code and DLLs; no game data, native DLL or host window.
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Loader, Modules, Process};
use adv_pe::ImportName;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const COUNT: u32 = DATA + 1024;
const HOOK_COUNT: u32 = DATA + 512;
const HOOK_ARG: u32 = DATA + 544;
struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}
struct Test {
    machine: Machine<JitCpu>,
    asm: Asm,
    next: u32,
}
impl Test {
    fn new(jit: bool, rejects: bool) -> Self {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "target/altered-library-{}-{jit}-{rejects}",
                std::process::id()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let body = if rejects {
            vec![0x31, 0xc0, 0xc2, 12, 0]
        } else {
            let mut body = vec![0xff, 0x05]; // inc DWORD PTR [COUNT]; mov eax,1; ret 12
            body.extend_from_slice(&COUNT.to_le_bytes());
            body.extend_from_slice(&[0xb8, 1, 0, 0, 0, 0xc2, 12, 0]);
            body
        };
        let mut image = adv_os::synthetic::build(
            0x500000,
            "owned.dll",
            &[adv_os::synthetic::Export {
                name: Some("DllMain".into()),
                ordinal: Some(1),
                target: adv_os::synthetic::Target::Code(body),
            }],
        )
        .unwrap()
        .bytes;
        let pe = adv_pe::Pe::parse(&image).unwrap();
        let adv_pe::ExportTarget::Rva(entry) = pe.exports().unwrap()[0].target else {
            panic!("entry")
        };
        image[0x98 + 16..0x98 + 20].copy_from_slice(&entry.to_le_bytes());
        std::fs::write(root.join("owned.dll"), image).unwrap();
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(&root, "authored.exe", root.join("overlay")),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.write(DATA, b"C:\\Game\\owned.dll\0").unwrap();
        let wide: Vec<_> = "C:\\Game\\owned.dll\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        machine.cpu.write(DATA + 128, &wide).unwrap();
        machine.cpu.write(DATA + 256, b"owned.dll\0").unwrap();
        Self {
            machine,
            asm: Asm::new(CODE),
            next: DATA + 2048,
        }
    }
    fn call(&mut self, name: &str, args: &[u32]) -> u32 {
        self.call_module("kernel32.dll", name, args)
    }
    fn call_module(&mut self, module: &str, name: &str, args: &[u32]) -> u32 {
        for &arg in args.iter().rev() {
            self.asm.push_imm(arg);
        }
        let address = self.machine.resolve_import(module, name).unwrap();
        let output = self.next;
        self.next += 4;
        self.asm.call(address).mov_mem_eax(output);
        output
    }
    fn hook(&mut self, index: u32, name: &str, failure: Option<u32>) {
        let native = self.machine.load(&Empty, "ntdll.dll").unwrap();
        let target = Loader::new(
            &mut self.machine.cpu,
            &mut self.machine.sys,
            self.machine.ext.get_mut::<Modules>(),
            &Empty,
        )
        .resolve_dynamic(
            native,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .unwrap()
        .0;
        let hook = CODE + 0x800 + index * 0x80;
        let original = hook + 0x40;
        let mut pristine = [0; 32];
        self.machine.cpu.read(target, &mut pristine).unwrap();
        self.machine.cpu.write(original, &pristine).unwrap();
        let mut code = vec![0xff, 0x05]; // inc counter; preserve EAX and observe first argument
        code.extend((HOOK_COUNT + index * 4).to_le_bytes());
        code.extend([0x50, 0x8b, 0x44, 0x24, 8, 0xa3]);
        code.extend((HOOK_ARG + index * 4).to_le_bytes());
        code.push(0x58);
        if let Some(status) = failure {
            let argc = if name == "LdrAddRefDll" { 2 } else { 4 };
            code.push(0xb8);
            code.extend(status.to_le_bytes());
            code.extend([0xc2, argc * 4, 0]);
        } else {
            code.push(0xe9);
            code.extend(
                original
                    .wrapping_sub(hook + code.len() as u32 + 4)
                    .to_le_bytes(),
            );
        }
        self.machine.cpu.write(hook, &code).unwrap();
        let mut jump = vec![0xe9];
        jump.extend(hook.wrapping_sub(target + 5).to_le_bytes());
        self.machine.cpu.write(target, &jump).unwrap();
    }
    fn stored_call(&mut self, name: &str, arg: u32) -> u32 {
        self.asm.mov_eax_mem(arg).push_eax();
        let address = self.machine.resolve_import("kernel32.dll", name).unwrap();
        let output = self.next;
        self.next += 4;
        self.asm.call(address).mov_mem_eax(output);
        output
    }
    fn count(&mut self) -> u32 {
        self.count_at(COUNT)
    }
    fn count_at(&mut self, address: u32) -> u32 {
        let output = self.next;
        self.next += 4;
        self.asm.mov_eax_mem(address).mov_mem_eax(output);
        output
    }
    fn run(&mut self) {
        self.call("ExitProcess", &[0]);
        self.machine.cpu.write(CODE, &self.asm.bytes).unwrap();
        self.machine.spawn("authored", CODE, &[], 65536).unwrap();
        assert!(matches!(
            self.machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
    }
    fn value(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
}

#[test]
fn altered_a_w_loads_run_initialization_once_retain_one_owner_and_unload_normally() {
    for jit in [false, true] {
        let mut test = Test::new(jit, false);
        let first = test.call("LoadLibraryExA", &[DATA, 0, 8]);
        let initialized = test.count();
        let second = test.call("LoadLibraryExW", &[DATA + 128, 0, 8]);
        let repeated = test.count();
        let release1 = test.stored_call("FreeLibrary", first);
        let visible = test.call("GetModuleHandleA", &[DATA + 256]);
        let release2 = test.stored_call("FreeLibrary", second);
        let gone = test.call("GetModuleHandleA", &[DATA + 256]);
        let detached = test.count();
        test.run();
        assert_eq!(test.value(first), 0x500000);
        assert_eq!(test.value(second), test.value(first));
        assert_eq!(test.value(initialized), 1);
        assert_eq!(test.value(repeated), 1);
        assert_eq!(test.value(release1), 1);
        assert_eq!(test.value(visible), test.value(first));
        assert_eq!(test.value(release2), 1);
        assert_eq!(test.value(gone), 0);
        assert_eq!(test.value(detached), 2);
    }
}

#[test]
fn altered_failed_initializer_and_invalid_flag_combinations_leave_no_live_image() {
    let mut test = Test::new(false, true);
    let rejected = test.call("LoadLibraryExA", &[DATA, 0, 8]);
    let init_error = test.call("GetLastError", &[]);
    let gone = test.call("GetModuleHandleA", &[DATA + 256]);
    let invalid = test.call("LoadLibraryExW", &[DATA + 128, 1, 8]);
    let parameter_error = test.call("GetLastError", &[]);
    let combined = test.call("LoadLibraryExA", &[DATA, 0, 8 | 0x800]);
    let combined_error = test.call("GetLastError", &[]);
    let unknown = test.call("LoadLibraryExA", &[DATA, 0, 0x20]);
    let unknown_error = test.call("GetLastError", &[]);
    test.run();
    assert_eq!(test.value(rejected), 0);
    assert_eq!(test.value(init_error), 1114);
    assert_eq!(test.value(gone), 0);
    assert_eq!(test.value(invalid), 0);
    assert_eq!(test.value(parameter_error), 87);
    assert_eq!(test.value(combined), 0);
    assert_eq!(test.value(combined_error), 87);
    assert_eq!(test.value(unknown), 0);
    assert_eq!(test.value(unknown_error), 50);
}

#[test]
fn altered_load_and_module_queries_observe_native_hooks_and_reference_lifetime() {
    for jit in [false, true] {
        let mut test = Test::new(jit, false);
        for (i, name) in [
            "LdrLoadDll",
            "LdrGetDllHandle",
            "LdrGetDllHandleEx",
            "LdrAddRefDll",
            "RtlPcToFileHeader",
        ]
        .into_iter()
        .enumerate()
        {
            test.hook(i as u32, name, None);
        }
        let loaded = test.call("LoadLibraryExW", &[DATA + 128, 0, 8]);
        let load_flags = test.count_at(HOOK_ARG);
        let named_a = test.call("GetModuleHandleA", &[DATA + 256]);
        let named_w = test.call("GetModuleHandleW", &[DATA + 128]);
        let unchanged = test.call("GetModuleHandleExA", &[2, DATA + 256, DATA + 384]);
        let retained = test.call("GetModuleHandleExW", &[0, DATA + 128, DATA + 388]);
        let address_unchanged = test.call("GetModuleHandleExW", &[6, 0x500400, DATA + 392]);
        let address_retained = test.call("GetModuleHandleExA", &[4, 0x500400, DATA + 396]);
        let counts: Vec<_> = (0..5).map(|i| test.count_at(HOOK_COUNT + i * 4)).collect();
        let ex_flags = test.count_at(HOOK_ARG + 8);
        let add_flags = test.count_at(HOOK_ARG + 12);
        test.stored_call("FreeLibrary", loaded);
        test.stored_call("FreeLibrary", loaded);
        let visible = test.call("GetModuleHandleA", &[DATA + 256]);
        test.stored_call("FreeLibrary", loaded);
        let gone = test.call("GetModuleHandleA", &[DATA + 256]);
        test.run();
        assert_eq!(test.value(loaded), 0x500000);
        assert_eq!(test.value(load_flags), 9);
        assert_eq!([test.value(named_a), test.value(named_w)], [0x500000; 2]);
        assert_eq!(
            [
                test.value(unchanged),
                test.value(retained),
                test.value(address_unchanged),
                test.value(address_retained)
            ],
            [1; 4]
        );
        for out in [384, 388, 392, 396] {
            assert_eq!(test.value(DATA + out), 0x500000);
        }
        assert_eq!(
            counts
                .into_iter()
                .map(|p| test.value(p))
                .collect::<Vec<_>>(),
            [1, 4, 4, 2, 2]
        );
        assert_eq!([test.value(ex_flags), test.value(add_flags)], [1, 0]);
        assert_eq!(test.value(visible), 0x500000);
        assert_eq!(test.value(gone), 0);
        assert!(
            test.machine
                .ext
                .get_or_default::<ApiDiagnostics>()
                .acceptance_eligible(RunMode::Strict)
        );
    }
}

#[test]
fn inner_lookup_hook_and_pin_are_observable_without_an_outer_hook() {
    for jit in [false, true] {
        let mut test = Test::new(jit, false);
        test.hook(0, "LdrGetDllHandleEx", None);
        test.hook(1, "LdrAddRefDll", None);
        let loaded = test.call("LoadLibraryExA", &[DATA, 0, 8]);
        let queried = test.call("GetModuleHandleW", &[DATA + 128]);
        let pinned = test.call("GetModuleHandleExA", &[1, DATA + 256, DATA + 384]);
        let pin_flags = test.count_at(HOOK_ARG + 4);
        test.stored_call("FreeLibrary", loaded);
        test.stored_call("FreeLibrary", loaded);
        let visible = test.call("GetModuleHandleA", &[DATA + 256]);
        test.run();
        assert_eq!([test.value(queried), test.value(visible)], [0x500000; 2]);
        assert_eq!([test.value(pinned), test.value(pin_flags)], [1, 1]);
        assert_eq!(test.value(HOOK_COUNT), 3);
        assert_eq!(test.value(HOOK_COUNT + 4), 1);
    }
}

#[test]
fn failed_native_lookup_and_retain_preserve_failed_win32_outputs() {
    for retain in [false, true] {
        let mut test = Test::new(false, false);
        test.hook(
            0,
            if retain {
                "LdrAddRefDll"
            } else {
                "LdrGetDllHandle"
            },
            Some(if retain { 0xc0000022 } else { 0xc0000135 }),
        );
        let loaded = test.call("LoadLibraryExW", &[DATA + 128, 0, 8]);
        let failed = test.call("GetModuleHandleExW", &[0, DATA + 128, DATA + 384]);
        let error = test.call("GetLastError", &[]);
        test.stored_call("FreeLibrary", loaded);
        let gone = test.call("GetModuleHandleA", &[DATA + 256]);
        test.run();
        assert_eq!(test.value(failed), 0);
        assert_eq!(test.value(DATA + 384), 0);
        assert_eq!(test.value(error), if retain { 5 } else { 126 });
        assert_eq!(test.value(gone), 0);
    }
}

#[test]
fn direct_counted_lookup_preserves_native_failure_output_and_last_error() {
    let mut test = Test::new(false, false);
    test.machine.cpu.write(DATA + 320, &[34, 0, 36, 0]).unwrap();
    test.machine.cpu.write_u32(DATA + 324, DATA + 128).unwrap();
    let loaded = test.call("LoadLibraryExA", &[DATA, 0, 8]);
    test.call("SetLastError", &[0x13572468]);
    let found = test.call_module(
        "ntdll.dll",
        "LdrGetDllHandle",
        &[0, 0, DATA + 320, DATA + 384],
    );
    let found_base = test.count_at(DATA + 384);
    let last_error = test.call("GetLastError", &[]);
    test.stored_call("FreeLibrary", loaded);
    let missing = test.call_module(
        "ntdll.dll",
        "LdrGetDllHandleEx",
        &[1, 0, 0, DATA + 320, DATA + 384],
    );
    let preserved = test.count_at(DATA + 384);
    let invalid = test.call_module(
        "ntdll.dll",
        "LdrGetDllHandleEx",
        &[3, 0, 0, DATA + 320, DATA + 384],
    );
    test.run();
    assert_eq!(test.value(found), 0);
    assert_eq!(test.value(found_base), 0x500000);
    assert_eq!(test.value(last_error), 0x13572468);
    assert_eq!(test.value(missing), 0xc0000135);
    assert_eq!(test.value(preserved), 0x500000);
    assert_eq!(test.value(invalid), 0xc000000d);
}

#[test]
fn datafile_loading_avoids_executable_native_load_and_queries() {
    let mut test = Test::new(false, false);
    test.hook(0, "LdrLoadDll", None);
    let datafile = test.call("LoadLibraryExA", &[DATA, 0, 2]);
    let absent = test.call("GetModuleHandleW", &[DATA + 128]);
    let initialized = test.count();
    test.stored_call("FreeLibrary", datafile);
    test.run();
    assert_ne!(test.value(datafile), 0);
    assert_eq!(test.value(datafile) & 1, 1);
    assert_eq!(test.value(absent), 0);
    assert_eq!(test.value(initialized), 0);
    assert_eq!(test.value(HOOK_COUNT), 0);
}

#[test]
fn datafile_refuses_to_bypass_a_live_native_section_entry() {
    let mut test = Test::new(false, false);
    test.hook(0, "NtCreateSection", None);
    let rejected = test.call("LoadLibraryExW", &[DATA + 128, 0, 2]);
    let error = test.call("GetLastError", &[]);
    test.run();
    assert_eq!(test.value(rejected), 0);
    assert_eq!(test.value(error), 50);
    assert_eq!(test.value(HOOK_COUNT), 0);
    assert!(
        !test
            .machine
            .ext
            .get_or_default::<ApiDiagnostics>()
            .acceptance_eligible(RunMode::Strict)
    );
}
