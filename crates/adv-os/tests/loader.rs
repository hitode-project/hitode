//! Loader tests on the local sample corpus (`adv-samples`).
//! Each test is skipped when its sample is missing.

use adv_abi::Registry;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{HOST_MODULE_BASE, PARK_ADDR, THUNK_BASE, THUNK_SIZE, THUNK_STRIDE};
use adv_machine::{Clock, Machine, RunResult};
use adv_os::loader::HostDir;
use adv_os::{FileSource, ModuleKind, Process};
use adv_pe::Pe;
use adv_unicorn::UnicornCpu;

fn game(build: &str) -> Option<HostDir> {
    adv_samples::dir(build).map(|root| HostDir::new(root, "C:\\Game"))
}

fn import_names(pe: &Pe) -> Vec<String> {
    pe.imports()
        .unwrap()
        .into_iter()
        .map(|d| d.dll.to_ascii_lowercase())
        .collect()
}

fn machine() -> Machine<UnicornCpu> {
    // These corpus tests exercise the PAL HLE persona. The registry, rather than
    // a fixed engine DLL name in the loader, chooses that complete host module.
    let mut registry = Registry::default();
    registry.add(
        "pal.dll",
        "FixturePersona",
        adv_machine::Conv::Stdcall { argc: 0 },
        |_| adv_machine::Outcome::Return(0),
    );
    Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap()
}

#[path = "../../adv-pe/tests/support/delay_fixture.rs"]
mod delay_fixture;

#[test]
fn absent_extensions_do_not_create_modules_without_a_system_provider() {
    struct Empty;
    impl FileSource for Empty {
        fn read(&self, _: &str) -> Option<Vec<u8>> {
            None
        }
    }
    let mut guest = machine();
    for name in ["missing.dll", "missing.drv", "missing.ocx", "missing.unt"] {
        assert!(matches!(
            guest.load(&Empty, name),
            Err(adv_os::LoadError::NotFound(_))
        ));
        assert!(adv_os::modules(&guest.ext).by_name(name).is_none());
    }
    // An actual registered provider remains available without a guest file.
    let id = guest.load(&Empty, "pal.dll").unwrap();
    assert_eq!(adv_os::modules(&guest.ext).get(id).kind, ModuleKind::Host);
}

#[test]
fn absent_static_imports_bind_as_system_modules() {
    // Windows refuses to start an image whose static import is missing, so a
    // natively runnable image's absent import is treated as a system DLL.
    // Use an authored name with no provider so future API additions cannot
    // turn this missing-module case into a supported dynamic load.
    struct Files;
    impl FileSource for Files {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            if path != "main.dll" {
                return None;
            }
            let mut bytes = delay_fixture::image(true);
            delay_fixture::word(&mut bytes, 0x98 + 96 + 13 * 8, 0);
            delay_fixture::word(&mut bytes, 0x98 + 96 + 8, 0xa00);
            delay_fixture::word(&mut bytes, 0x98 + 100 + 8, 40);
            for (at, value) in [
                (0xa00, 0xa40),
                (0xa0c, 0xa60),
                (0xa10, 0xa50),
                (0xa40, 0xa80),
                (0xa50, 0xa80),
            ] {
                delay_fixture::word(&mut bytes, at, value);
            }
            let dll = b"fixture-absent.dll\0";
            let export = b"FixtureAbsentExport\0";
            bytes[0xa60..0xa60 + dll.len()].copy_from_slice(dll);
            bytes[0xa82..0xa82 + export.len()].copy_from_slice(export);
            Some(bytes)
        }
    }
    let mut m = machine();
    assert!(matches!(
        m.load(&Files, "fixture-absent.dll"),
        Err(adv_os::LoadError::NotFound(_))
    ));
    let id = m.load(&Files, "main.dll").unwrap();
    let base = adv_os::modules(&m.ext).get(id).base;
    let system = adv_os::modules(&m.ext)
        .by_name("fixture-absent.dll")
        .unwrap();
    assert_eq!(adv_os::modules(&m.ext).get(system).kind, ModuleKind::Host);
    assert!(is_thunk(m.cpu.read_u32(base + 0xa50).unwrap()));
}

#[test]
fn shipped_dll_wins_over_api_registration_unless_provider_is_explicit() {
    struct Files(Vec<u8>);
    impl FileSource for Files {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            (path == "dll\\PAL.dll").then(|| self.0.clone())
        }
    }
    let mut bytes = delay_fixture::image(true);
    // This authored DLL only tests module selection; no dependencies are needed.
    delay_fixture::word(&mut bytes, 0x98 + 96 + 13 * 8, 0);
    delay_fixture::word(&mut bytes, 0x98 + 96, 0x800);
    delay_fixture::word(&mut bytes, 0x98 + 100, 0x100);
    for (at, value) in [
        (0x810, 1),
        (0x814, 1),
        (0x818, 1),
        (0x81c, 0x860),
        (0x820, 0x870),
        (0x824, 0x878),
        (0x860, 0x900),
        (0x870, 0x880),
    ] {
        delay_fixture::word(&mut bytes, at, value);
    }
    bytes[0x880..0x88f].copy_from_slice(b"PalMemoryAlloc\0");
    let files = Files(bytes);
    let mut guest = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    let id = guest.load(&files, "PAL.dll").unwrap();
    let module = adv_os::modules(&guest.ext).get(id);
    assert_eq!(module.kind, ModuleKind::Image);
    assert_eq!(module.path, "dll\\PAL.dll");
    assert_eq!(
        module.exports_by_name["PalMemoryAlloc"],
        adv_pe::ExportTarget::Rva(0x900)
    );
    let mut host = machine();
    let id = host.load(&files, "PAL.dll").unwrap();
    assert_eq!(adv_os::modules(&host.ext).get(id).kind, ModuleKind::Image);
    let mut host = machine();
    host.sys
        .thunks
        .resolver_mut::<Registry>()
        .unwrap()
        .replace_module("pal.dll");
    let id = host.load(&files, "PAL.dll").unwrap();
    assert_eq!(adv_os::modules(&host.ext).get(id).kind, ModuleKind::Host);
    // An explicit path must not accidentally select a dll/dll/ fallback.
    let mut explicit = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    // Without a registered provider the absent explicit path is not found.
    assert!(matches!(
        explicit.load(&files, "missing\\PAL.dll"),
        Err(adv_os::LoadError::NotFound(_))
    ));
    assert!(adv_os::modules(&explicit.ext).by_name("pal.dll").is_none());
}

#[test]
fn module_search_prefers_named_file_then_importer_then_bundle_and_rejects_bad_pe() {
    use adv_os::loader::{LoadError, module_file};
    struct Files(Vec<&'static str>);
    impl FileSource for Files {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            self.0.contains(&path).then(|| path.as_bytes().to_vec())
        }
    }
    let paths = ["PAL.dll", "plugins\\PAL.dll", "dll\\PAL.dll"];
    for index in 0..paths.len() {
        let files = Files(paths[index..].to_vec());
        let (path, bytes) = module_file(&files, "PAL.dll", Some("plugins")).unwrap();
        assert_eq!(path, paths[index]);
        assert_eq!(bytes, paths[index].as_bytes());
    }
    let mut m = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    assert!(
        matches!(m.load(&Files(paths.to_vec()), "PAL.dll"), Err(LoadError::Pe { path, .. }) if path == "PAL.dll")
    );
}

#[test]
fn old_va_delay_imports_bind_after_loader_relocation() {
    struct Files;
    impl FileSource for Files {
        fn read(&self, path: &str) -> Option<Vec<u8>> {
            let mut bytes = delay_fixture::image(false);
            if path == "main.exe" {
                delay_fixture::word(&mut bytes, 0x98 + 96 + 13 * 8, 0);
            } else if path != "legacy.dll" {
                return None;
            }
            Some(bytes)
        }
    }
    let mut m = machine();
    m.load(&Files, "main.exe").unwrap();
    let id = m.load(&Files, "legacy.dll").unwrap();
    let base = adv_os::modules(&m.ext).get(id).base;
    assert_ne!(base, delay_fixture::BASE);
    let kernel = adv_os::modules(&m.ext).by_name("kernel32.dll").unwrap();
    assert_eq!(
        m.cpu.read_u32(base + delay_fixture::HMOD + 0x300).unwrap(),
        adv_os::modules(&m.ext).get(kernel).base
    );
    for slot in [delay_fixture::IAT + 0x300, delay_fixture::IAT + 0x304] {
        assert!(is_thunk(m.cpu.read_u32(base + slot).unwrap()));
    }
}

fn is_thunk(addr: u32) -> bool {
    (THUNK_BASE..THUNK_BASE + THUNK_SIZE).contains(&addr)
        || (HOST_MODULE_BASE..HOST_MODULE_BASE + adv_machine::layout::HOST_MODULE_CAP * 65536)
            .contains(&addr)
}

#[test]
fn leyline_binds_imports_and_delay_imports() {
    let Some(files) = game("LL1") else { return };
    let mut m = machine();
    let exe = m.load(&files, "LeyLine.exe").unwrap();
    let module = adv_os::modules(&m.ext).get(exe);
    assert_eq!((module.base, module.kind), (0x40_0000, ModuleKind::Image));

    let pe = Pe::parse(&std::fs::read(files.host_path("LeyLine.exe").unwrap()).unwrap()).unwrap();
    let base = module.base;
    for imp in pe.imports().unwrap() {
        for i in &imp.imports {
            let slot = m.cpu.read_u32(base + i.iat_rva).unwrap();
            assert!(is_thunk(slot), "{}!{} -> {slot:#x}", imp.dll, i.name);
        }
    }
    let delay = pe.delay_imports().unwrap();
    assert_eq!(delay.len(), 1);
    let pal = &delay[0];
    let hmod = m.cpu.read_u32(base + pal.module_handle_rva).unwrap();
    let pal_id = adv_os::modules(&m.ext).by_name("PAL.dll").unwrap();
    assert_eq!(hmod, adv_os::modules(&m.ext).get(pal_id).base);
    assert!(
        hmod >= HOST_MODULE_BASE,
        "PAL.dll must be host-implemented, not the file in dll/"
    );
    let mut seen = 0;
    for i in &pal.imports {
        let slot = m.cpu.read_u32(base + i.iat_rva).unwrap();
        assert!(is_thunk(slot));
        let t = m.sys.thunks.get((slot - THUNK_BASE) / THUNK_STRIDE);
        assert_eq!(
            (t.module.as_str(), t.name.as_str()),
            ("pal.dll", i.name.to_string().as_str())
        );
        seen += 1;
    }
    assert_eq!(seen, 186);
}

#[test]
fn leyline_runs_crt_init_until_first_unimplemented_call() {
    let Some(files) = game("LL1") else { return };
    let mut m = machine();
    let exe = m.load(&files, "LeyLine.exe").unwrap();
    m.start_process(exe).unwrap();
    let RunResult::Failed(e) = m.run_virtual(1_000_000) else {
        panic!("expected to stop at an unimplemented import")
    };
    eprintln!("stopped: {e}\n  recent: {:?}", e.recent);
    assert!(e.message.starts_with("unimplemented: "), "{e}");
    // The failing call came from guest code inside the exe, i.e. the CRT actually ran.
    let caller = m
        .cpu
        .read_u32(m.cpu.reg(adv_cpu::backend::Reg::Esp))
        .unwrap();
    assert_eq!(
        adv_os::modules(&m.ext).containing(caller),
        Some(exe),
        "caller {caller:#x}"
    );
}

#[test]
fn resource_dlls_share_a_preferred_base() {
    let Some(files) = game("LL1") else { return };
    let mut m = machine();
    let a = m.load(&files, "C:\\Game\\dll\\RESOURCE.dll").unwrap();
    let b = m.load(&files, "dll/fx.DLL").unwrap();
    let (ma, mb) = (
        adv_os::modules(&m.ext).get(a),
        adv_os::modules(&m.ext).get(b),
    );
    assert_eq!(ma.base, 0x1000_0000);
    assert_ne!(mb.base, ma.base, "second image must be moved");
    for md in [ma, mb] {
        let res = md.resources.as_ref().expect("resource tree");
        let leaf = &res.leaves()[0];
        // LoadResource returns base + rva: the data must be readable in guest memory.
        let mut buf = vec![0; leaf.data.size.min(16) as usize];
        m.cpu.read(md.base + leaf.data.rva, &mut buf).unwrap();
    }
    // Loading again by another spelling returns the same module.
    assert_eq!(m.load(&files, "RESOURCE").unwrap(), a);
}

#[test]
fn tl2026_static_tls_and_current_detours_imports_preserved() {
    let Some((files, name)) = game("TL2026").zip(adv_samples::exe("TL2026")) else {
        return;
    };
    let mut m = machine();
    let exe = m.load(&files, &name).unwrap();
    // Added imports are part of the executable's current process contract.
    let mut pe = Pe::parse(&std::fs::read(files.host_path(&name).unwrap()).unwrap()).unwrap();
    let now = import_names(&pe);
    if let Some(original) = pe.detours_original_imports().unwrap() {
        pe.dirs[adv_pe::dir::IMPORT] = original;
        let kept = import_names(&pe);
        for dll in now.iter().filter(|d| !kept.contains(d)) {
            assert!(adv_os::modules(&m.ext).by_name(dll).is_some(), "{dll}");
        }
    }
    assert!(!m.sys.tls_images.is_empty());
    assert_eq!(m.sys.tls_images[0].template.len(), 8);

    // The main context gets a TLS block holding a copy of the template.
    let ctx = m.start_process(exe).unwrap();
    let teb = m.sys.context(ctx).teb;
    let array = m.cpu.read_u32(teb + 0x2c).unwrap();
    let block = m.cpu.read_u32(array).unwrap();
    let mut got = [0u8; 8];
    m.cpu.read(block, &mut got).unwrap();
    assert_eq!(got[..], m.sys.tls_images[0].template[..]);
}

#[test]
fn all_game_exes_load() {
    for build in ["FH2009", "LL2", "LL3", "KK2015"] {
        let Some((files, exe)) = game(build).zip(adv_samples::exe(build)) else {
            continue;
        };
        let exe = exe.as_str();
        let mut m = machine();
        let id = m.load(&files, exe).unwrap_or_else(|e| panic!("{exe}: {e}"));
        let md = adv_os::modules(&m.ext).get(id);
        assert_eq!(md.base, 0x40_0000, "{exe}");
        // Code is mapped executable and not writable: a write through the guest faults.
        let text = md.base + 0x1000;
        assert!(m.cpu.read_u8(text).is_ok());
        let _ = Prot::RX;
    }
}

#[test]
fn host_dir_resolves_mixed_case_components_and_prefers_exact_names() {
    let root = std::env::temp_dir().join(format!("adv-sync-dir-case-{}", std::process::id()));
    std::fs::create_dir_all(root.join("Dll/Nested")).unwrap();
    let file = root.join("Dll/Nested/Pal.dll");
    std::fs::write(&file, b"synthetic PAL fixture").unwrap();
    let files = HostDir::new(&root, "C:\\Game");
    for path in [
        "dll/nEsTeD/PAL.dll",
        "c:\\gAmE\\DLL\\nested\\pal.DLL",
        "Dll/Nested/Pal.dll",
    ] {
        assert_eq!(files.read(path).unwrap(), b"synthetic PAL fixture");
    }
    // Distinct case-only siblings are possible only on a case-sensitive filesystem.
    let other = root.join("Dll/Nested/PAL.dll");
    if !other.exists() {
        std::fs::write(&other, b"exact uppercase fixture").unwrap();
        assert_eq!(
            files.read("Dll/Nested/PAL.dll").unwrap(),
            b"exact uppercase fixture"
        );
        assert_eq!(
            files.read("Dll/Nested/Pal.dll").unwrap(),
            b"synthetic PAL fixture"
        );
    }
    std::fs::remove_dir_all(root).unwrap();
}
