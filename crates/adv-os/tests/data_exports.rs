//! Data binding follows the same live image and ordinal-authority contracts as code.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, HostFn, Machine, Resolver, RunResult, layout::PARK_ADDR};
use adv_os::{
    FileSource, Loader, ModuleKind, Modules, Process,
    synthetic::{self, Export, Target},
};
use adv_pe::ImportName;

struct Empty;
impl FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}
fn named(name: &str) -> ImportName {
    ImportName::Name {
        hint: 0,
        name: name.into(),
    }
}

#[test]
fn uninitialized_data_is_a_named_failure_until_guest_patches_the_cell_or_eat() {
    for jit in [false, true] {
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        let id = machine.load(&Empty, "msvcr90.dll").unwrap();
        let base = adv_os::modules(&machine.ext).get(id).base;
        let adv_pe::ExportTarget::Rva(rva) =
            adv_os::modules(&machine.ext).get(id).exports_by_name["_acmdln"]
        else {
            panic!("data")
        };
        for dynamic in [false, true] {
            let mut loader = Loader::new(
                &mut machine.cpu,
                &mut machine.sys,
                machine.ext.get_mut::<Modules>(),
                &Empty,
            );
            let result = if dynamic {
                loader
                    .resolve_dynamic(id, &named("_acmdln"))
                    .map(|value| value.0)
            } else {
                loader.resolve(id, &named("_acmdln"))
            };
            assert!(
                matches!(result, Err(adv_os::loader::LoadError::MissingExport { name, .. }) if name=="_acmdln")
            );
        }
        assert!(machine.resolve_import("msvcr90.dll", "_acmdln").is_err());
        machine.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        let mut code = Asm::new(0x400000);
        code.mov_mem_imm(base + rva, 17).ret();
        machine.cpu.write(0x400000, &code.bytes).unwrap();
        machine
            .start_main(0x400000, 0x400000, 65536, "data patch")
            .unwrap();
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(_)
        ));
        let mut loader = Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            machine.ext.get_mut::<Modules>(),
            &Empty,
        );
        assert_eq!(
            loader.resolve_dynamic(id, &named("_acmdln")).unwrap().0,
            base + rva
        );
        assert_eq!(loader.resolve(id, &named("_acmdln")).unwrap(), base + rva);
        let directory = base + loader.cpu.read_u32(base + 0x80 + 120).unwrap();
        let count = loader.cpu.read_u32(directory + 20).unwrap();
        let table = base + loader.cpu.read_u32(directory + 28).unwrap();
        let slot = (0..count)
            .map(|index| table + index * 4)
            .find(|slot| loader.cpu.read_u32(*slot).unwrap() == rva)
            .unwrap();
        loader.cpu.write_u32(slot, rva + 8).unwrap();
        assert_eq!(
            loader.resolve_dynamic(id, &named("_acmdln")).unwrap().0,
            base + rva + 8,
            "guest EAT ownership wins"
        );
    }
}

// A fixture-supplied authoritative ordinal alias exercises the neutral Resolver
// contract without inventing a historic MSVCRT ordinal in production metadata.
struct Alias(bool);
impl Resolver for Alias {
    fn host_fn(&self, _: &str, _: &str) -> Option<HostFn> {
        None
    }
    fn provides(&self, _: &str) -> bool {
        false
    }
    fn canonical_name(&self, _: &str, name: &str) -> Option<String> {
        (name == "#17").then(|| "Cell".into())
    }
    fn data_size(&self, _: &str, name: &str) -> Option<u32> {
        matches!(name, "Cell" | "#17").then_some(4)
    }
    fn host_data(&self, _: &str, name: &str) -> Option<&[u8]> {
        (self.0 && matches!(name, "Cell" | "#17")).then_some(&[0; 4])
    }
}
#[path = "../../adv-pe/tests/support/delay_fixture.rs"]
mod import_fixture;
struct Files;
impl FileSource for Files {
    fn read(&self, path: &str) -> Option<Vec<u8>> {
        if path == "data.dll" {
            return Some(
                synthetic::build(
                    0x600000,
                    "data.dll",
                    &[
                        Export {
                            name: Some("Cell".into()),
                            ordinal: Some(1),
                            target: Target::Data {
                                binding: "cell".into(),
                                bytes: vec![0; 4],
                            },
                        },
                        Export {
                            name: None,
                            ordinal: Some(17),
                            target: Target::Data {
                                binding: "cell".into(),
                                bytes: vec![0; 4],
                            },
                        },
                    ],
                )
                .unwrap()
                .bytes,
            );
        }
        if path != "main.exe" {
            return None;
        }
        let mut bytes = import_fixture::image(true);
        import_fixture::word(&mut bytes, 0x98 + 96 + 13 * 8, 0);
        import_fixture::word(&mut bytes, 0x98 + 104, 0x400);
        import_fixture::word(&mut bytes, 0x98 + 108, 40);
        bytes[0x400..0x440].fill(0);
        for (at, value) in [
            (0x400, 0x500),
            (0x40c, 0x700),
            (0x410, 0x540),
            (0x500, 0x600),
            (0x504, 0x80000011),
        ] {
            import_fixture::word(&mut bytes, at, value);
        }
        bytes[0x602..0x60f].fill(0);
        bytes[0x602..0x607].copy_from_slice(b"Cell\0");
        bytes[0x700..0x709].copy_from_slice(b"data.dll\0");
        Some(bytes)
    }
}
#[test]
fn authoritative_static_data_ordinal_iat_matches_named_and_dynamic_bindings() {
    for initialized in [false, true] {
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Alias(initialized),
            Clock::virtual_time(),
        )
        .unwrap();
        let data = machine.load(&Files, "data.dll").unwrap();
        machine.ext.get_mut::<Modules>().get_mut(data).kind = ModuleKind::Host;
        if !initialized {
            let mut loader = Loader::new(
                &mut machine.cpu,
                &mut machine.sys,
                machine.ext.get_mut::<Modules>(),
                &Files,
            );
            assert!(matches!(
                loader.resolve(data, &ImportName::Ordinal(17)),
                Err(adv_os::loader::LoadError::MissingExport { .. })
            ));
            continue;
        }
        let main = machine.load(&Files, "main.exe").unwrap();
        let base = adv_os::modules(&machine.ext).get(main).base;
        let by_name = machine.cpu.read_u32(base + 0x540).unwrap();
        assert_eq!(by_name, machine.cpu.read_u32(base + 0x544).unwrap());
        let mut loader = Loader::new(
            &mut machine.cpu,
            &mut machine.sys,
            machine.ext.get_mut::<Modules>(),
            &Files,
        );
        assert_eq!(
            loader
                .resolve_dynamic(data, &ImportName::Ordinal(17))
                .unwrap()
                .0,
            by_name
        );
        assert_eq!(loader.resolve(data, &named("Cell")).unwrap(), by_name);
        assert_ne!(
            loader.resolve(data, &ImportName::Ordinal(1)).unwrap(),
            by_name,
            "unpromised local static ordinal retains a diagnostic thunk"
        );
        assert_eq!(
            loader
                .resolve_dynamic(data, &ImportName::Ordinal(1))
                .unwrap()
                .0,
            by_name
        );
    }
}

#[test]
fn guest_data_and_forwarders_keep_precedence_over_host_descriptors() {
    struct Real;
    impl FileSource for Real {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            let (base, target) = match path {
                "msvcrt.dll" => (
                    0x600000,
                    Target::Data {
                        binding: "guest-cell".into(),
                        bytes: 99u32.to_le_bytes().to_vec(),
                    },
                ),
                "proxy.dll" => (0x800000, Target::Forwarder("msvcrt._acmdln".into())),
                _ => return None,
            };
            Some(
                synthetic::build(
                    base,
                    path,
                    &[Export {
                        name: Some("_acmdln".into()),
                        ordinal: Some(1),
                        target,
                    }],
                )
                .unwrap()
                .bytes,
            )
        }
    }
    let mut registry = Registry::default();
    registry
        .register_data("msvcrt.dll", "_acmdln", &17u32.to_le_bytes())
        .unwrap();
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let id = machine.load(&Real, "msvcrt.dll").unwrap();
    let proxy = machine.load(&Real, "proxy.dll").unwrap();
    assert_eq!(
        adv_os::modules(&machine.ext).get(id).kind,
        ModuleKind::Image
    );
    let mut loader = Loader::new(
        &mut machine.cpu,
        &mut machine.sys,
        machine.ext.get_mut::<Modules>(),
        &Real,
    );
    let cell = loader.resolve_dynamic(id, &named("_acmdln")).unwrap().0;
    assert_eq!(loader.cpu.read_u32(cell).unwrap(), 99);
    assert_eq!(loader.resolve(proxy, &named("_acmdln")).unwrap(), cell);
}
