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
    let mut registry = Registry::default();
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
