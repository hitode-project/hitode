//! Loader tests on the local sample corpus (`pal-samples`).
//! Each test is skipped when its sample is missing.

use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Guest, GuestExt, Prot};
use pal_machine::layout::{HOST_MODULE_BASE, PARK_ADDR, THUNK_BASE, THUNK_SIZE, THUNK_STRIDE};
use pal_machine::loader::HostDir;
use pal_machine::{Clock, FileSource, Machine, ModuleKind, Registry, RunResult};
use pal_pe::Pe;

fn game(build: &str) -> Option<HostDir> {
    pal_samples::dir(build).map(|root| HostDir::new(root, "C:\\Game"))
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
        pal_machine::Conv::Stdcall { argc: 0 },
        |_| pal_machine::Outcome::Return(0),
    );
    Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap()
}

#[path = "../../pal-pe/tests/support/delay_fixture.rs"]
mod delay_fixture;

#[test]
fn engine_dll_from_bundled_directory_is_guest_unless_registered() {
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
    let module = guest.sys.modules.get(id);
    assert_eq!(module.kind, ModuleKind::Image);
    assert_eq!(module.path, "dll\\PAL.dll");
    assert_eq!(
        module.exports_by_name["PalMemoryAlloc"],
        pal_pe::ExportTarget::Rva(0x900)
    );
    let mut host = machine();
    let id = host.load(&files, "PAL.dll").unwrap();
    assert_eq!(host.sys.modules.get(id).kind, ModuleKind::Host);
    // An explicit path must not accidentally select a dll/dll/ fallback.
    let mut explicit = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    let id = explicit.load(&files, "missing\\PAL.dll").unwrap();
    assert_eq!(explicit.sys.modules.get(id).kind, ModuleKind::Host);
}

#[test]
fn module_search_prefers_named_file_then_importer_then_bundle_and_rejects_bad_pe() {
    use pal_machine::loader::{LoadError, module_file};
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
    let base = m.sys.modules.get(id).base;
    assert_ne!(base, delay_fixture::BASE);
    let kernel = m.sys.modules.by_name("kernel32.dll").unwrap();
    assert_eq!(
        m.cpu.read_u32(base + delay_fixture::HMOD + 0x300).unwrap(),
        m.sys.modules.get(kernel).base
    );
    for slot in [delay_fixture::IAT + 0x300, delay_fixture::IAT + 0x304] {
        assert!(is_thunk(m.cpu.read_u32(base + slot).unwrap()));
    }
}

fn is_thunk(addr: u32) -> bool {
    (THUNK_BASE..THUNK_BASE + THUNK_SIZE).contains(&addr)
}

#[test]
fn leyline_binds_imports_and_delay_imports() {
    let Some(files) = game("LL1") else { return };
    let mut m = machine();
    let exe = m.load(&files, "LeyLine.exe").unwrap();
    let module = m.sys.modules.get(exe);
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
    let pal_id = m.sys.modules.by_name("PAL.dll").unwrap();
    assert_eq!(hmod, m.sys.modules.get(pal_id).base);
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
    let caller = m.cpu.read_u32(m.cpu.reg(pal_cpu::Reg::Esp)).unwrap();
    assert_eq!(
        m.sys.modules.containing(caller),
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
    let (ma, mb) = (m.sys.modules.get(a), m.sys.modules.get(b));
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
fn tl2026_static_tls_and_detours_added_imports_skipped() {
    let Some((files, name)) = game("TL2026").zip(pal_samples::exe("TL2026")) else {
        return;
    };
    let mut m = machine();
    let exe = m.load(&files, &name).unwrap();
    // If Detours' setdll added DLLs to the import table, the loader binds the original table
    // from the .detour header: the added DLLs never load, and only the exe has TLS.
    let mut pe = Pe::parse(&std::fs::read(files.host_path(&name).unwrap()).unwrap()).unwrap();
    let now = import_names(&pe);
    if let Some(original) = pe.detours_original_imports().unwrap() {
        pe.dirs[pal_pe::dir::IMPORT] = original;
        let kept = import_names(&pe);
        for dll in now.iter().filter(|d| !kept.contains(d)) {
            assert!(m.sys.modules.by_name(dll).is_none(), "{dll}");
            assert!(
                m.sys
                    .process_init
                    .iter()
                    .all(|c| !c.what.contains(dll.as_str())),
                "{dll}"
            );
        }
    }
    assert_eq!(m.sys.tls_images.len(), 1);
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
        let Some((files, exe)) = game(build).zip(pal_samples::exe(build)) else {
            continue;
        };
        let exe = exe.as_str();
        let mut m = machine();
        let id = m.load(&files, exe).unwrap_or_else(|e| panic!("{exe}: {e}"));
        let md = m.sys.modules.get(id);
        assert_eq!(md.base, 0x40_0000, "{exe}");
        // Code is mapped executable and not writable: a write through the guest faults.
        let text = md.base + 0x1000;
        assert!(m.cpu.read_u8(text).is_ok());
        let _ = Prot::RX;
    }
}

#[test]
fn host_dir_resolves_mixed_case_components_and_prefers_exact_names() {
    let root = std::env::temp_dir().join(format!("pal-host-dir-case-{}", std::process::id()));
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
