//! Authored process fixtures; no game files or native guest execution.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::{FileSource, Loader, ModuleKind, Modules, Process};
use adv_pe::{ExportTarget, ImportName, Pe};

struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}
fn machine(jit: bool) -> Machine<JitCpu> {
    machine_exposing(jit, false)
}

fn machine_exposing(jit: bool, fixed: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    registry.set_expose_fixed_exports(fixed);
    registry.add(
        "comctl32.dll",
        "InitCommonControls",
        Conv::Stdcall { argc: 0 },
        |_| Outcome::Return(0x1234),
    );
    Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap()
}

#[test]
fn initializer_failure_policy_is_owned_by_the_frontend_and_stops_process_entry() {
    for jit in [false, true] {
        let mut machine = machine(jit);
        machine.cpu.map(0x400000, 4096, Prot::RWX).unwrap();
        machine.cpu.write(0x400000, &[0x31, 0xc0, 0xc3]).unwrap(); // xor eax,eax; ret
        machine
            .cpu
            .write(0x400010, &[0xb8, 7, 0, 0, 0, 0xc3])
            .unwrap();
        machine.sys.process_init.push_back(adv_machine::InitCall {
            target: 0x400000,
            args: vec![],
            what: "frontend initializer with no API-specific name".into(),
            failure_exit: Some(0xc0000142),
            begin: None,
        });
        machine
            .start_main(0x400000, 0x400010, 0x10000, "authored")
            .unwrap();
        assert!(matches!(
            machine.run_virtual(100000),
            RunResult::Exited(0xc0000142)
        ));
    }
}
fn resolve(machine: &mut Machine<JitCpu>, id: usize, symbol: ImportName) -> u32 {
    Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    )
    .resolve(id, &symbol)
    .unwrap()
}
fn named(name: &str) -> ImportName {
    ImportName::Name {
        hint: 0,
        name: name.into(),
    }
}

fn guest_override(machine: &mut Machine<JitCpu>, id: usize, name: &str) -> Option<u32> {
    Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    )
    .guest_override(id, name)
    .unwrap()
}

#[test]
fn guest_override_detects_live_inline_and_eat_edits_against_immutable_entries() {
    let mut machine = machine(false);
    let module = machine.load(&Empty, "comctl32.dll").unwrap();
    let name = "InitCommonControls";
    let entry = resolve(&mut machine, module, named(name));
    assert_eq!(guest_override(&mut machine, module, name), None);
    assert_eq!(guest_override(&mut machine, module, "NotAnExport"), None);
    // Editing a separate published thunk does not change the copied PE entry.
    // The reference bytes must come from the immutable emitter, not this page.
    let thunk = machine.resolve_import("comctl32.dll", name).unwrap();
    machine.cpu.protect(thunk & !4095, 4096, Prot::RWX).unwrap();
    machine.cpu.write(thunk, &[0xc3]).unwrap();
    assert_eq!(guest_override(&mut machine, module, name), None);
    machine.cpu.protect(entry & !4095, 4096, Prot::RWX).unwrap();
    machine.cpu.write(entry, &[0xc3]).unwrap();
    assert_eq!(guest_override(&mut machine, module, name), Some(entry));
    let image = adv_os::modules(&machine.ext).get(module);
    let base = image.base;
    let ExportTarget::Rva(original) = image.exports_by_name[name] else {
        panic!("code export")
    };
    let directory = base + machine.cpu.read_u32(base + 0x80 + 120).unwrap();
    let count = machine.cpu.read_u32(directory + 20).unwrap();
    let table = base + machine.cpu.read_u32(directory + 28).unwrap();
    let slot = (0..count)
        .map(|index| table + index * 4)
        .find(|slot| machine.cpu.read_u32(*slot).unwrap() == original)
        .unwrap();
    machine.cpu.protect(slot & !4095, 4096, Prot::RW).unwrap();
    machine.cpu.write_u32(slot, original + 1).unwrap();
    assert_eq!(guest_override(&mut machine, module, name), Some(entry + 1));
    let forwarder = machine.cpu.read_u32(directory + 12).unwrap();
    machine.cpu.write(base + forwarder, b"absent.F\0").unwrap();
    machine.cpu.write_u32(slot, forwarder).unwrap();
    let count = adv_os::modules(&machine.ext).iter().count();
    let mut loader = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    );
    assert!(matches!(
        loader.guest_override(module, name),
        Err(adv_os::LoadError::Pe { .. })
    ));
    assert_eq!(loader.modules.iter().count(), count);
    assert!(loader.modules.by_name("absent.dll").is_none());
    assert!(loader.inits.is_empty());
    // Capability filtering still applies to pristine diagnostic exports, while
    // the override probe itself neither requires nor reports host availability.
    let controls = machine.load(&Empty, "comctl32.dll").unwrap();
    assert_eq!(
        guest_override(&mut machine, controls, "InitCommonControlsEx"),
        None
    );
    assert!(dynamic(&mut machine, controls, named("InitCommonControlsEx")).is_err());
}

#[test]
fn guest_override_reads_changed_export_names_and_ordinals_after_warm_lookups() {
    let mut machine = machine(false);
    let module = machine.load(&Empty, "comctl32.dll").unwrap();
    let name = "InitCommonControls";
    let entry = resolve(&mut machine, module, named(name));
    assert_eq!(guest_override(&mut machine, module, name), None);
    let image = adv_os::modules(&machine.ext).get(module);
    let base = image.base;
    let directory = base + machine.cpu.read_u32(base + 0x80 + 120).unwrap();
    let names = base + machine.cpu.read_u32(directory + 32).unwrap();
    let ordinals = base + machine.cpu.read_u32(directory + 36).unwrap();
    let count = machine.cpu.read_u32(directory + 24).unwrap();
    let index = (0..count)
        .find(|index| {
            let pointer = base + machine.cpu.read_u32(names + index * 4).unwrap();
            machine.cpu.read_cstr(pointer, 128).unwrap() == name.as_bytes()
        })
        .unwrap();
    let pointer = base + machine.cpu.read_u32(names + index * 4).unwrap();
    machine
        .cpu
        .protect(pointer & !4095, 4096, Prot::RW)
        .unwrap();
    machine.cpu.write(pointer, b"?nitCommonControls\0").unwrap();
    assert!(matches!(
        Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            machine.ext.get_mut::<Modules>(),
            &Empty,
        )
        .guest_override(module, name),
        Err(adv_os::LoadError::MissingExport { .. })
    ));
    machine.cpu.write(pointer, name.as_bytes()).unwrap();
    assert_eq!(guest_override(&mut machine, module, name), None);
    let slot = ordinals + index * 2;
    let original = machine.cpu.read_u16(slot).unwrap();
    let functions = base + machine.cpu.read_u32(directory + 28).unwrap();
    let function_count = machine.cpu.read_u32(directory + 20).unwrap();
    let (replacement, rva) = (0..function_count)
        .filter(|candidate| *candidate != u32::from(original))
        .find_map(|candidate| {
            let rva = machine.cpu.read_u32(functions + candidate * 4).unwrap();
            machine
                .cpu
                .accessible(base + rva, 1, Prot::EXEC)
                .then_some((candidate as u16, rva))
        })
        .unwrap();
    machine.cpu.protect(slot & !4095, 4096, Prot::RW).unwrap();
    machine.cpu.write(slot, &replacement.to_le_bytes()).unwrap();
    assert_ne!(base + rva, entry);
    assert_eq!(guest_override(&mut machine, module, name), Some(base + rva));
    machine.cpu.write(slot, &original.to_le_bytes()).unwrap();
    assert_eq!(guest_override(&mut machine, module, name), None);
}

#[test]
fn live_export_name_comparison_handles_long_prefixes_and_utf8_without_stale_results() {
    let mut registry = Registry::default();
    let first = format!("{}A", "prefix".repeat(30));
    let second = format!("{}B", "prefix".repeat(30));
    for name in [first.as_str(), second.as_str(), "unicode_é"] {
        registry.add(
            "authored_names.dll",
            name,
            Conv::Stdcall { argc: 0 },
            |_| Outcome::Return(1),
        );
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let module = machine.load(&Empty, "authored_names.dll").unwrap();
    let first_entry = resolve(&mut machine, module, named(&first));
    let second_entry = resolve(&mut machine, module, named(&second));
    assert_ne!(first_entry, second_entry);
    for name in [first.as_str(), second.as_str(), "unicode_é"] {
        assert_eq!(guest_override(&mut machine, module, name), None);
    }
}

#[test]
fn export_hints_follow_current_headers_tables_ordinals_and_moved_names() {
    let mut registry = Registry::default();
    for name in ["A", "Target", "Z"] {
        registry.add(
            "authored_hints.dll",
            name,
            Conv::Stdcall { argc: 0 },
            |_| Outcome::Return(1),
        );
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let module = machine.load(&Empty, "authored_hints.dll").unwrap();
    let target = resolve(&mut machine, module, named("Target"));
    let other = resolve(&mut machine, module, named("Z"));
    assert_eq!(guest_override(&mut machine, module, "Target"), None);
    let image = adv_os::modules(&machine.ext).get(module);
    let (base, size) = (image.base, image.size);
    let pe = machine.cpu.read_u32(base + 0x3c).unwrap();
    let directory = base + machine.cpu.read_u32(base + pe + 120).unwrap();
    let names = base + machine.cpu.read_u32(directory + 32).unwrap();
    let ordinals = base + machine.cpu.read_u32(directory + 36).unwrap();
    let functions = base + machine.cpu.read_u32(directory + 28).unwrap();
    let pointers: Vec<_> = (0..3)
        .map(|index| machine.cpu.read_u32(names + index * 4).unwrap())
        .collect();
    let indices: Vec<_> = (0..3)
        .map(|index| machine.cpu.read_u16(ordinals + index * 2).unwrap())
        .collect();
    // Move the NT header, export directory and all three tables. The sorted,
    // unique name table adds P before Target, invalidating its warmed index 1.
    let relocated = size - 0x400;
    machine.cpu.protect(base, 4096, Prot::RW).unwrap();
    machine
        .cpu
        .protect((base + relocated) & !4095, 4096, Prot::RW)
        .unwrap();
    let header_size = 24
        + usize::from(machine.cpu.read_u16(base + pe + 20).unwrap())
        + usize::from(machine.cpu.read_u16(base + pe + 6).unwrap()) * 40;
    machine
        .cpu
        .copy_memory(base + pe, base + 0x400, header_size)
        .unwrap();
    machine.cpu.write_u32(base + 0x3c, 0x400).unwrap();
    machine
        .cpu
        .write_u32(base + 0x400 + 120, relocated)
        .unwrap();
    machine.cpu.write_u32(base + 0x400 + 124, 0xa0).unwrap();
    machine
        .cpu
        .copy_memory(directory, base + relocated, 40)
        .unwrap();
    machine.cpu.write_u32(base + relocated + 24, 4).unwrap();
    for (field, offset) in [(28, 0x40), (32, 0x50), (36, 0x70)] {
        machine
            .cpu
            .write_u32(base + relocated + field, relocated + offset)
            .unwrap();
    }
    machine
        .cpu
        .copy_memory(functions, base + relocated + 0x40, 12)
        .unwrap();
    machine.cpu.write(base + relocated + 0x90, b"P\0").unwrap();
    for (index, pointer) in [pointers[0], relocated + 0x90, pointers[1], pointers[2]]
        .into_iter()
        .enumerate()
    {
        machine
            .cpu
            .write_u32(base + relocated + 0x50 + index as u32 * 4, pointer)
            .unwrap();
    }
    for (index, ordinal) in [indices[0], indices[0], indices[1], indices[2]]
        .into_iter()
        .enumerate()
    {
        machine
            .cpu
            .write(
                base + relocated + 0x70 + index as u32 * 2,
                &ordinal.to_le_bytes(),
            )
            .unwrap();
    }
    assert_eq!(guest_override(&mut machine, module, "Target"), None);
    for hint in [1, 2, u16::MAX] {
        assert_eq!(
            resolve(
                &mut machine,
                module,
                ImportName::Name {
                    hint,
                    name: "Target".into()
                }
            ),
            target
        );
    }
    // Both the relocated ordinal and EAT are read after a matching hint.
    machine
        .cpu
        .write(base + relocated + 0x74, &indices[2].to_le_bytes())
        .unwrap();
    assert_eq!(guest_override(&mut machine, module, "Target"), Some(other));
    machine
        .cpu
        .write(base + relocated + 0x74, &indices[1].to_le_bytes())
        .unwrap();
    let slot = base + relocated + 0x40 + u32::from(indices[1]) * 4;
    machine.cpu.write_u32(slot, target - base + 1).unwrap();
    assert_eq!(
        guest_override(&mut machine, module, "Target"),
        Some(target + 1)
    );
    machine.cpu.write_u32(slot, target - base).unwrap();
    // A warm hint cannot bypass current header or complete table bounds.
    for (field, invalid) in [(base + 0x400, 0), (base + relocated + 32, size - 3)] {
        let original = machine.cpu.read_u32(field).unwrap();
        machine.cpu.write_u32(field, invalid).unwrap();
        assert!(
            Loader::new(
                &mut machine.cpu,
                &mut machine.sys,
                machine.ext.get_mut::<Modules>(),
                &Empty
            )
            .guest_override(module, "Target")
            .is_err()
        );
        machine.cpu.write_u32(field, original).unwrap();
        assert_eq!(guest_override(&mut machine, module, "Target"), None);
    }
    machine.cpu.write_u32(base + 0x3c, pe).unwrap();
    assert_eq!(guest_override(&mut machine, module, "Target"), None);
}

fn dynamic(
    machine: &mut Machine<JitCpu>,
    id: usize,
    symbol: ImportName,
) -> Result<u32, adv_os::loader::LoadError> {
    Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Empty,
    )
    .resolve_dynamic(id, &symbol)
    .map(|(address, _)| address)
}

#[test]
fn fixed_core_exports_exist_while_noncore_metadata_keeps_capability_policy() {
    let mut hidden = machine(false);
    let kernel = hidden.load(&Empty, "kernel32.dll").unwrap();
    for name in ["FlsAlloc", "FlsFree", "FlsGetValue", "FlsSetValue"] {
        assert!(dynamic(&mut hidden, kernel, named(name)).is_err());
        assert_ne!(resolve(&mut hidden, kernel, named(name)), 0);
    }
    let mut machine = machine_exposing(false, true);
    let kernel = machine.load(&Empty, "kernel32.dll").unwrap();
    for name in ["FlsAlloc", "FlsFree", "FlsGetValue", "FlsSetValue"] {
        let module = adv_os::modules(&machine.ext).get(kernel);
        let target = module.exports_by_name[name].clone();
        let ordinal = module
            .exports_by_ordinal
            .iter()
            .find(|(_, rva)| **rva == target)
            .unwrap()
            .0;
        let ordinal = *ordinal;
        assert_eq!(
            dynamic(&mut machine, kernel, named(name)).unwrap(),
            dynamic(&mut machine, kernel, ImportName::Ordinal(ordinal)).unwrap()
        );
        assert_ne!(
            resolve(&mut machine, kernel, named(name)),
            0,
            "static diagnostics remain hookable"
        );
    }
    assert!(dynamic(&mut machine, kernel, named("UnknownOptionalExport")).is_err());
    for (dll, name) in [
        ("dwrite.dll", "DWriteCreateFactory"),
        ("d3d11.dll", "D3D11CreateDevice"),
        ("gdiplus.dll", "GdiplusStartup"),
        ("gdiplus.dll", "GdiplusShutdown"),
        ("d2d1.dll", "D2D1CreateFactory"),
    ] {
        let id = machine.load(&Empty, dll).unwrap();
        assert_eq!(
            dynamic(&mut machine, id, named(name)).unwrap(),
            resolve(&mut machine, id, named(name))
        );
        assert!(!machine.sys.thunks.resolver().implemented(dll, name));
    }
    let d2d = adv_os::modules(&machine.ext).by_name("d2d1.dll").unwrap();
    assert_eq!(
        dynamic(&mut machine, d2d, ImportName::Ordinal(1)).unwrap(),
        resolve(&mut machine, d2d, named("D2D1CreateFactory"))
    );
    // A discoverable signature and HRESULT failure value are not capabilities.
    let d3d11 = adv_os::modules(&machine.ext).by_name("d3d11.dll").unwrap();
    assert!(adv_abi::signature::lookup("d3d11.dll", "D3D11CreateDeviceAndSwapChain").is_some());
    assert!(dynamic(&mut machine, d3d11, named("D3D11CreateDeviceAndSwapChain")).is_err());
}

#[test]
fn dynamic_unimplemented_entries_become_visible_when_guest_hooks_the_live_eat_or_code() {
    for jit in [false, true] {
        let mut machine = machine(jit);
        let kernel = machine.load(&Empty, "comctl32.dll").unwrap();
        let module = adv_os::modules(&machine.ext).get(kernel);
        let (base, size) = (module.base, module.size);
        let original = resolve(&mut machine, kernel, named("InitCommonControlsEx"));
        assert!(dynamic(&mut machine, kernel, named("InitCommonControlsEx")).is_err());
        machine
            .cpu
            .protect(original & !4095, 4096, Prot::RWX)
            .unwrap();
        machine
            .cpu
            .write(original, &[0xb8, 99, 0, 0, 0, 0xc2, 4, 0])
            .unwrap();
        assert_eq!(
            dynamic(&mut machine, kernel, named("InitCommonControlsEx")).unwrap(),
            original
        );
        machine.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        let mut code = adv_cpu::backend::asm::Asm::new(0x400000);
        code.push_imm(0).call(original).ret();
        machine.cpu.write(0x400000, &code.bytes).unwrap();
        machine
            .start_main(0x400000, 0x400000, 65536, "hooked-probe")
            .unwrap();
        assert!(matches!(machine.run_virtual(100000), RunResult::Exited(99)));
        let directory = base + machine.cpu.read_u32(base + 0x80 + 120).unwrap();
        let ExportTarget::Rva(original_rva) =
            adv_os::modules(&machine.ext).get(kernel).exports_by_name["InitCommonControlsEx"]
        else {
            panic!("code")
        };
        let count = machine.cpu.read_u32(directory + 20).unwrap();
        let table = base + machine.cpu.read_u32(directory + 28).unwrap();
        let slot = (0..count)
            .map(|i| table + i * 4)
            .find(|slot| machine.cpu.read_u32(*slot).unwrap() == original_rva)
            .unwrap();
        machine.cpu.write_u32(slot, original_rva + 1).unwrap();
        assert_eq!(
            dynamic(&mut machine, kernel, named("InitCommonControlsEx")).unwrap(),
            original + 1
        );
        for invalid in [0, size] {
            machine.cpu.write_u32(slot, invalid).unwrap();
            assert!(dynamic(&mut machine, kernel, named("InitCommonControlsEx")).is_err());
        }
    }
}

#[test]
fn real_guest_exports_and_forwarders_are_not_filtered_by_host_capability_metadata() {
    struct Files;
    impl FileSource for Files {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            let (base, target) = match path {
                "kernel32.dll" => (
                    0x400000,
                    adv_os::synthetic::Target::Code(vec![0xb8, 99, 0, 0, 0, 0xc2, 4, 0]),
                ),
                "proxy.dll" => (
                    0x600000,
                    adv_os::synthetic::Target::Forwarder("kernel32.FlsAlloc".into()),
                ),
                _ => return None,
            };
            Some(
                adv_os::synthetic::build(
                    base,
                    path,
                    &[adv_os::synthetic::Export {
                        name: Some("FlsAlloc".into()),
                        ordinal: Some(1),
                        target,
                    }],
                )
                .unwrap()
                .bytes,
            )
        }
    }
    let mut machine = machine(false);
    let guest = machine.load(&Files, "kernel32.dll").unwrap();
    let proxy = machine.load(&Files, "proxy.dll").unwrap();
    assert_eq!(
        adv_os::modules(&machine.ext).get(guest).kind,
        ModuleKind::Image
    );
    let mut loader = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Files,
    );
    let entry = loader.resolve_dynamic(guest, &named("FlsAlloc")).unwrap().0;
    assert_eq!(
        loader.resolve_dynamic(proxy, &named("FlsAlloc")).unwrap().0,
        entry
    );
    assert_eq!(
        loader
            .resolve_dynamic(proxy, &ImportName::Ordinal(1))
            .unwrap()
            .0,
        entry
    );
    let host = loader.load("C:\\Windows\\system32\\kernel32.dll").unwrap();
    let module = loader.modules.get(host);
    let base = module.base;
    let ExportTarget::Rva(original_rva) = module.exports_by_name["FlsAlloc"] else {
        panic!("code")
    };
    let directory_rva = loader.cpu.read_u32(base + 0x80 + 120).unwrap();
    let directory = base + directory_rva;
    let count = loader.cpu.read_u32(directory + 20).unwrap();
    let table = base + loader.cpu.read_u32(directory + 28).unwrap();
    let slot = (0..count)
        .map(|i| table + i * 4)
        .find(|slot| loader.cpu.read_u32(*slot).unwrap() == original_rva)
        .unwrap();
    let old_size = loader.cpu.read_u32(base + 0x80 + 124).unwrap();
    let text = b"kernel32.FlsAlloc\0";
    // Extend the live export directory into its existing alignment padding.
    loader
        .cpu
        .write(base + directory_rva + old_size, text)
        .unwrap();
    loader
        .cpu
        .write_u32(base + 0x80 + 124, old_size + text.len() as u32)
        .unwrap();
    loader
        .cpu
        .write_u32(slot, directory_rva + old_size)
        .unwrap();
    assert_eq!(
        loader.resolve_dynamic(host, &named("FlsAlloc")).unwrap().0,
        entry,
        "a live host forwarder must reach the real guest DLL"
    );
}

#[test]
fn published_entries_are_real_pe_code_aliases_hookable_and_callable() {
    for jit in [false, true] {
        for patched in [false, true] {
            let mut m = machine(jit);
            let id = m.load(&Empty, "comctl32.dll").unwrap();
            let module = adv_os::modules(&m.ext).get(id);
            let base = module.base;
            let mut bytes = vec![0; module.size as usize];
            m.cpu.read(base, &mut bytes).unwrap();
            let pe = Pe::parse(&bytes).unwrap();
            let export = pe
                .exports()
                .unwrap()
                .into_iter()
                .find(|e| e.name.as_deref() == Some("InitCommonControls"))
                .unwrap();
            assert_eq!(export.ordinal, 17);
            let ExportTarget::Rva(rva) = export.target else {
                panic!("code export")
            };
            let entry = resolve(&mut m, id, named("InitCommonControls"));
            assert_eq!(entry, base + rva);
            assert_eq!(entry, resolve(&mut m, id, ImportName::Ordinal(17)));
            assert_eq!(m.cpu.read_u16(entry).unwrap(), 0xff8b);
            assert_eq!(adv_os::modules(&m.ext).containing(entry), Some(id));
            if patched {
                m.cpu.protect(entry & !4095, 4096, Prot::RWX).unwrap();
                m.cpu.write(entry, &[0xb8, 0x78, 0x56, 0, 0, 0xc3]).unwrap();
                m.cpu.protect(entry & !4095, 4096, Prot::RX).unwrap();
            }
            m.start_main(base, entry, 65536, "fixture").unwrap();
            assert!(
                matches!(m.run(), RunResult::Exited(code) if code == if patched { 0x5678 } else { 0x1234 })
            );
            // Merely publishing generated stubs is not an import/feature probe.
            assert!(
                m.sys
                    .thunks
                    .iter()
                    .filter(|(_, thunk)| !thunk.implemented)
                    .all(|(_, thunk)| !thunk.requested)
            );
        }
    }
}

#[test]
fn game_proxy_and_absolute_system_module_coexist_without_name_shadowing() {
    struct Proxy(Vec<u8>);
    impl FileSource for Proxy {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            path.eq_ignore_ascii_case("comctl32.dll")
                .then(|| self.0.clone())
        }
    }
    let proxy = adv_os::synthetic::build(
        0x400000,
        "comctl32.dll",
        &[adv_os::synthetic::Export {
            name: Some("InitCommonControls".into()),
            ordinal: Some(17),
            target: adv_os::synthetic::Target::Code(vec![0xb8, 99, 0, 0, 0, 0xc3]),
        }],
    )
    .unwrap();
    let files = Proxy(proxy.bytes);
    let mut m = machine(true);
    let proxy_id = m.load(&files, "comctl32.dll").unwrap();
    assert_eq!(
        adv_os::modules(&m.ext).get(proxy_id).kind,
        ModuleKind::Image
    );
    let system_id = m.load(&files, "C:/Windows/System32/comctl32.dll").unwrap();
    assert_ne!(system_id, proxy_id);
    assert_eq!(
        adv_os::modules(&m.ext).get(system_id).kind,
        ModuleKind::Host
    );
    assert_eq!(m.load(&files, "COMCTL32").unwrap(), proxy_id);
    assert_eq!(
        m.load(&files, "c:\\windows\\SYSTEM32\\COMCTL32.dll")
            .unwrap(),
        system_id
    );
    assert_eq!(
        adv_os::modules(&m.ext).by_name("comctl32.dll"),
        Some(proxy_id)
    );
    assert_eq!(
        adv_os::modules(&m.ext).by_name("C:\\Windows\\system32\\comctl32.dll"),
        Some(system_id)
    );
    assert_ne!(
        resolve(&mut m, proxy_id, named("InitCommonControls")),
        resolve(&mut m, system_id, named("InitCommonControls"))
    );
}

#[test]
fn get_proc_observes_eat_replacement_and_rejects_removed_or_out_of_image_targets() {
    let mut m = machine(true);
    let id = m.load(&Empty, "comctl32.dll").unwrap();
    let base = adv_os::modules(&m.ext).get(id).base;
    let directory = base + m.cpu.read_u32(base + 0x80 + 120).unwrap();
    let ordinal_base = m.cpu.read_u32(directory + 16).unwrap();
    let slot = base + m.cpu.read_u32(directory + 28).unwrap() + (17 - ordinal_base) * 4;
    let entry = resolve(&mut m, id, named("InitCommonControls"));
    m.cpu.write_u32(slot, entry - base + 1).unwrap();
    assert_eq!(resolve(&mut m, id, named("InitCommonControls")), entry + 1);
    for invalid in [0, u32::MAX] {
        m.cpu.write_u32(slot, invalid).unwrap();
        assert!(
            Loader::new(&mut m.cpu, &mut m.sys, m.ext.get_mut::<Modules>(), &Empty)
                .resolve(id, &named("InitCommonControls"))
                .is_err()
        );
    }
}

#[test]
fn forwarder_cycles_fail_with_a_bounded_diagnostic() {
    struct Files;
    impl FileSource for Files {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            let (base, target) = match path {
                "one.dll" => (0x400000, "two.Loop"),
                "two" | "two.dll" => (0x600000, "one.Loop"),
                _ => return None,
            };
            Some(
                adv_os::synthetic::build(
                    base,
                    path,
                    &[adv_os::synthetic::Export {
                        name: Some("Loop".into()),
                        ordinal: Some(1),
                        target: adv_os::synthetic::Target::Forwarder(target.into()),
                    }],
                )
                .unwrap()
                .bytes,
            )
        }
    }
    let mut m = machine(true);
    let id = m.load(&Files, "one.dll").unwrap();
    let result = Loader::new(&mut m.cpu, &mut m.sys, m.ext.get_mut::<Modules>(), &Files)
        .resolve(id, &named("Loop"));
    assert!(result.unwrap_err().to_string().contains("forwarder cycle"));
}

#[test]
fn generated_system_catalog_fits_the_reserved_arena_and_thunk_capacity() {
    let modules: std::collections::BTreeSet<_> = adv_abi::signature::signatures()
        .iter()
        .map(|s| s.module)
        .collect();
    let mut m = machine(true);
    for module in modules {
        let id = m
            .load(&Empty, module)
            .unwrap_or_else(|error| panic!("{module}: {error}"));
        let image = adv_os::modules(&m.ext).get(id);
        assert!(image.size > 4096);
        assert_eq!(m.cpu.read_u16(image.base).unwrap(), 0x5a4d);
    }
    assert!(m.sys.thunks.len() < adv_machine::layout::THUNK_CAP);
}

#[test]
fn family_system_images_share_addresses_and_keep_independent_bytes_and_references() {
    for jit in [false, true] {
        let mut parent = machine(jit);
        parent.load(&Empty, "comctl32.dll").unwrap();
        let native = parent.load(&Empty, "ntdll.dll").unwrap();
        let base = adv_os::modules(&parent.ext).get(native).base;
        let mut child = machine(jit);
        adv_os::modules_mut(&mut child.ext)
            .inherit_system_image_layout(adv_os::modules(&parent.ext))
            .unwrap();
        child.load(&Empty, "kernel32.dll").unwrap();
        let child_native = child.load(&Empty, "ntdll.dll").unwrap();
        assert_eq!(adv_os::modules(&child.ext).get(child_native).base, base);
        parent.load(&Empty, "ntdll.dll").unwrap();
        assert_eq!(adv_os::modules(&child.ext).get(child_native).load_count, 1);
        child.cpu.write_u32(base + 0x24, 0x12345678).unwrap();
        assert_ne!(parent.cpu.read_u32(base + 0x24).unwrap(), 0x12345678);
        assert!(
            adv_os::modules_mut(&mut child.ext)
                .inherit_system_image_layout(adv_os::modules(&parent.ext))
                .is_err()
        );
    }
}

#[test]
fn native_initialization_list_precedes_other_dlls_and_keeps_both_link_directions() {
    let mut m = machine(false);
    let prior = m.load(&Empty, "comctl32.dll").unwrap();
    let native = m.load(&Empty, "ntdll.dll").unwrap();
    let ldr = m
        .cpu
        .read_u32(adv_machine::layout::PEB_ADDR + 0x0c)
        .unwrap();
    let head = ldr + 0x1c;
    let first = adv_os::modules(&m.ext).get(native).ldr_entry + 16;
    let second = adv_os::modules(&m.ext).get(prior).ldr_entry + 16;
    assert_eq!(m.cpu.read_u32(head).unwrap(), first);
    assert_eq!(m.cpu.read_u32(first + 4).unwrap(), head);
    assert_eq!(m.cpu.read_u32(first).unwrap(), second);
    assert_eq!(m.cpu.read_u32(second + 4).unwrap(), first);
    assert_eq!(m.cpu.read_u32(second).unwrap(), head);
    assert_eq!(m.cpu.read_u32(head + 4).unwrap(), second);
}

#[test]
fn peb_loader_lists_keep_stable_links_names_counts_and_dll_preferred_bases() {
    struct Files;
    impl FileSource for Files {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            let (base, exe) = match path {
                "helper.dll" => (0x10000000, false),
                "app.exe" => (0x400000, true),
                _ => return None,
            };
            let mut bytes = adv_os::synthetic::build(base, path, &[]).unwrap().bytes;
            if exe {
                bytes[0x97] &= !0x20;
            }
            Some(bytes)
        }
    }
    let mut m = machine(true);
    let system = m.load(&Files, "comctl32.dll").unwrap();
    let system_entry = adv_os::modules(&m.ext).get(system).ldr_entry;
    let helper = m.load(&Files, "helper.dll").unwrap();
    assert_eq!(adv_os::modules(&m.ext).get(helper).base, 0x10000000);
    let exe = m.load(&Files, "app.exe").unwrap();
    let ldr = m
        .cpu
        .read_u32(adv_machine::layout::PEB_ADDR + 0x0c)
        .unwrap();
    assert_eq!(m.cpu.read_u32(ldr).unwrap(), 0x30);
    for (offset, head_offset) in [(0, 0x0c), (8, 0x14), (16, 0x1c)] {
        let head = ldr + head_offset;
        let mut current = m.cpu.read_u32(head).unwrap();
        let mut previous = head;
        let mut actual = Vec::new();
        while current != head {
            assert!(actual.len() < 4, "unterminated LDR list");
            assert_eq!(m.cpu.read_u32(current + 4).unwrap(), previous);
            actual.push(current - offset);
            previous = current;
            current = m.cpu.read_u32(current).unwrap();
        }
        assert_eq!(m.cpu.read_u32(head + 4).unwrap(), previous);
        let expected: Vec<_> = [system, helper, exe]
            .into_iter()
            .filter(|&id| offset != 16 || id != exe)
            .map(|id| adv_os::modules(&m.ext).get(id).ldr_entry)
            .collect();
        assert_eq!(actual, expected);
    }
    let size = m.cpu.read_u16(system_entry + 0x24).unwrap();
    let text = m.cpu.read_u32(system_entry + 0x28).unwrap();
    let units: Vec<_> = (0..u32::from(size) / 2)
        .map(|i| m.cpu.read_u16(text + i * 2).unwrap())
        .collect();
    assert_eq!(
        String::from_utf16(&units).unwrap(),
        "C:\\Windows\\system32\\comctl32.dll"
    );
    assert_eq!(m.load(&Files, "comctl32.dll").unwrap(), system);
    assert_eq!(adv_os::modules(&m.ext).get(system).ldr_entry, system_entry);
    assert_eq!(m.cpu.read_u16(system_entry + 0x38).unwrap(), 2);
}
