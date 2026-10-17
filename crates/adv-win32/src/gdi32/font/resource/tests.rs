use super::*;

#[test]
fn resource_entry_points_use_the_single_generated_abi() {
    let mut registry = Registry::default();
    register(&mut registry);
    for name in [
        "AddFontResourceA",
        "AddFontResourceW",
        "RemoveFontResourceA",
        "RemoveFontResourceW",
        "AddFontResourceExA",
        "AddFontResourceExW",
        "RemoveFontResourceExA",
        "RemoveFontResourceExW",
    ] {
        assert_eq!(
            registry.level("gdi32.dll", name),
            Some(adv_abi::ImplementationLevel::Implement)
        );
    }
    assert!(registry.signature_conflicts().is_empty());
}

#[cfg(windows)]
mod windows {
    use super::*;
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt, Prot, Reg, asm::Asm},
    };
    use adv_machine::{
        Clock, Machine, RunResult,
        layout::{PARK_ADDR, teb},
    };
    use std::{
        path::PathBuf,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    const FONT: &[u8] =
        include_bytes!("../../../../../adv-device/tests/fixtures/font-resource.ttf");
    const COLLECTION: &[u8] =
        include_bytes!("../../../../../adv-device/tests/fixtures/font-resource.ttc");
    const CODE: u32 = 0x400000;
    const DATA: u32 = 0x410000;
    const MARKER: u32 = 0x12345678;
    const PRIVATE_HIDDEN: u32 = FR_PRIVATE | FR_NOT_ENUM;
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    static NATIVE: Mutex<()> = Mutex::new(());

    fn fixture(codepage: u32) -> (Machine<JitCpu>, PathBuf) {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let directory = workspace.join("target/font-resource-tests").join(format!(
            "{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(directory.join("Fonts")).unwrap();
        std::fs::write(directory.join("Fonts/fixture.ttf"), FONT).unwrap();
        std::fs::write(directory.join("Fonts/collection.ttc"), COLLECTION).unwrap();
        std::fs::write(directory.join("Fonts/字形.ttf"), FONT).unwrap();
        std::fs::write(directory.join("Fonts/corrupt.ttf"), [0x5a; 128]).unwrap();
        let mut registry = Registry::default();
        crate::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        let mut config = crate::Win32Config::new(
            &directory,
            "font-fixture.exe",
            directory.join("overlay-data"),
        );
        config.os.codepage = codepage;
        crate::Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.spawn("font resources", CODE, &[], 65536).unwrap();
        machine
            .cpu
            .write_u32(machine.sys.context(0).teb + teb::LAST_ERROR, MARKER)
            .unwrap();
        (machine, directory)
    }
    fn invoke(machine: &mut Machine<JitCpu>, name: &str, path: Option<&str>) -> u32 {
        invoke_flags(machine, name, path, 0, 0)
    }
    fn invoke_flags(
        machine: &mut Machine<JitCpu>,
        name: &str,
        path: Option<&str>,
        flags: u32,
        reserved: u32,
    ) -> u32 {
        let pointer = path.map_or(0, |path| {
            let bytes = if name.ends_with('W') {
                path.encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
            } else {
                machine
                    .ext
                    .get::<Win32>()
                    .codec
                    .encode(path)
                    .into_iter()
                    .chain([0])
                    .collect()
            };
            machine.cpu.write(DATA, &bytes).unwrap();
            DATA
        });
        let address = machine.resolve_import("gdi32.dll", name).unwrap();
        let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
        let handler = machine.sys.thunks.get(index).f.handler;
        machine.cpu.set_reg(Reg::Esp, DATA + 2048);
        machine.cpu.write_u32(DATA + 2052, pointer).unwrap();
        machine.cpu.write_u32(DATA + 2056, flags).unwrap();
        machine.cpu.write_u32(DATA + 2060, reserved).unwrap();
        machine
            .cpu
            .write_u32(machine.sys.context(0).teb + teb::LAST_ERROR, MARKER)
            .unwrap();
        let result = handler(&mut Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: index,
        });
        assert_eq!(
            machine
                .cpu
                .read_u32(machine.sys.context(0).teb + teb::LAST_ERROR)
                .unwrap(),
            if flags & !(FR_PRIVATE | FR_NOT_ENUM) == 0 {
                MARKER
            } else {
                87
            }
        );
        let Outcome::Return(value) = result else {
            panic!("resource call did not return");
        };
        value
    }
    fn glyph(machine: &mut Machine<JitCpu>) -> Arc<text::Glyph> {
        let font = FontRequest {
            default: None,
            face: "ADV Resource Fixture".into(),
            height: -40,
            width: 0,
            baseline_angle: 0,
            glyph_angle: 0,
            weight: 400,
            italic: false,
            underline: false,
            strikeout: false,
            encoding: Encoding::Automatic,
            precision: Precision::Default,
            clipping: Clipping {
                precision: ClipPrecision::Default,
                transform_rotation: false,
                force_outline: false,
                disable_association: false,
                embedded: false,
            },
            quality: Quality::Antialiased,
            pitch: Pitch::Default,
            family: Family::Default,
        };
        machine
            .ext
            .get_mut::<Fonts>()
            .text
            .glyph(
                &font,
                GlyphRequest {
                    character: Character::Unicode(u32::from(b'B')),
                    format: GlyphFormat::Gray8,
                    hinted: true,
                    transform: Transform::IDENTITY,
                },
            )
            .unwrap()
    }

    #[test]
    fn private_hidden_flags_share_ansi_wide_references_and_real_glyphs() {
        let _native = NATIVE.lock().unwrap();
        for codepage in [932, 936] {
            let (mut machine, _) = fixture(codepage);
            let fallback = glyph(&mut machine);
            for name in ["AddFontResourceExA", "AddFontResourceExW"] {
                assert_eq!(
                    invoke_flags(
                        &mut machine,
                        name,
                        Some("Fonts/字形.ttf"),
                        PRIVATE_HIDDEN,
                        0
                    ),
                    1
                );
            }
            assert_eq!(glyph(&mut machine).metrics.advance_x, 20);
            assert_ne!(glyph(&mut machine).data, fallback.data);
            assert!(
                machine
                    .ext
                    .get::<Fonts>()
                    .text
                    .enumerate(
                        Some("ADV Resource Fixture"),
                        text::FontEnumeration::Families
                    )
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(machine.ext.get::<Fonts>().resources.len(), 1);
            assert_eq!(
                invoke(&mut machine, "RemoveFontResourceW", Some("Fonts/字形.ttf")),
                0
            );
            // Different valid modes cannot consume this hidden reference.
            for flags in [0, FR_PRIVATE, FR_NOT_ENUM] {
                assert_eq!(
                    invoke_flags(
                        &mut machine,
                        "RemoveFontResourceExW",
                        Some("Fonts/字形.ttf"),
                        flags,
                        0
                    ),
                    0
                );
            }
            for name in ["AddFontResourceExW", "RemoveFontResourceExW"] {
                assert_eq!(
                    invoke_flags(&mut machine, name, Some("Fonts/字形.ttf"), 0x40, 0),
                    0
                );
            }
            // The native oracle also accepts nonzero reserved values without
            // dereferencing them; it shares the same path/flags reference.
            for name in ["AddFontResourceExW", "RemoveFontResourceExW"] {
                assert_eq!(
                    invoke_flags(
                        &mut machine,
                        name,
                        Some("Fonts/字形.ttf"),
                        PRIVATE_HIDDEN,
                        1
                    ),
                    1
                );
            }
            assert_eq!(
                machine
                    .ext
                    .get::<Fonts>()
                    .resources
                    .values()
                    .next()
                    .unwrap()
                    .count,
                2
            );
            assert_eq!(
                invoke_flags(
                    &mut machine,
                    "RemoveFontResourceExA",
                    Some("c:/gAmE/Fonts/./字形.ttf"),
                    PRIVATE_HIDDEN,
                    0
                ),
                1
            );
            assert_eq!(glyph(&mut machine).metrics.advance_x, 20);
            assert_eq!(
                invoke_flags(
                    &mut machine,
                    "RemoveFontResourceExW",
                    Some("Fonts/字形.ttf"),
                    PRIVATE_HIDDEN,
                    0
                ),
                1
            );
            assert_eq!(glyph(&mut machine).data, fallback.data);
            assert_eq!(
                invoke_flags(
                    &mut machine,
                    "RemoveFontResourceExW",
                    Some("Fonts/字形.ttf"),
                    PRIVATE_HIDDEN,
                    0
                ),
                0
            );
            assert!(machine.ext.get::<Fonts>().resources.is_empty());
        }
    }

    #[test]
    fn private_hidden_bad_paths_preserve_resources_and_process_exit_retires_them() {
        let _native = NATIVE.lock().unwrap();
        let (mut machine, _) = fixture(936);
        let fallback = glyph(&mut machine);
        assert_eq!(
            invoke_flags(
                &mut machine,
                "AddFontResourceExW",
                Some("Fonts/fixture.ttf"),
                PRIVATE_HIDDEN,
                0
            ),
            1
        );
        let cached = glyph(&mut machine);
        for path in [
            None,
            Some(""),
            Some("Fonts/missing.ttf"),
            Some("Fonts/corrupt.ttf"),
        ] {
            for name in [
                "AddFontResourceExA",
                "AddFontResourceExW",
                "RemoveFontResourceExA",
                "RemoveFontResourceExW",
            ] {
                assert_eq!(invoke_flags(&mut machine, name, path, PRIVATE_HIDDEN, 0), 0);
                assert!(Arc::ptr_eq(&cached, &glyph(&mut machine)));
            }
        }
        let exit = machine
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        let mut code = Asm::new(CODE);
        code.push_imm(0).call(exit);
        machine.cpu.write(CODE, &code.bytes).unwrap();
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        assert!(machine.ext.get::<Fonts>().resources.is_empty());
        assert_eq!(machine.ext.get::<Fonts>().text.resource_stats(), (0, 0));
        assert_eq!(glyph(&mut machine).data, fallback.data);
    }

    #[test]
    fn ansi_wide_normalized_paths_share_references_and_real_rasterization() {
        let _native = NATIVE.lock().unwrap();
        for codepage in [932, 936] {
            let (mut machine, _) = fixture(codepage);
            let fallback = glyph(&mut machine);
            assert_eq!(
                invoke(&mut machine, "AddFontResourceA", Some("Fonts\\字形.ttf")),
                1
            );
            assert_eq!(machine.ext.get::<Fonts>().text.stats().fonts, 0);
            let loaded = glyph(&mut machine);
            assert_eq!(loaded.metrics.advance_x, 20);
            assert_ne!(loaded.data, fallback.data);
            assert_eq!(
                invoke(
                    &mut machine,
                    "AddFontResourceW",
                    Some("c:/gAmE/Fonts/./字形.ttf")
                ),
                1
            );
            assert_eq!(machine.ext.get::<Fonts>().resources.len(), 1);
            assert_eq!(
                machine.ext.get::<Fonts>().text.resource_stats(),
                (1, FONT.len())
            );
            assert_eq!(
                invoke(&mut machine, "RemoveFontResourceA", Some("Fonts\\字形.ttf")),
                1
            );
            assert_eq!(glyph(&mut machine).metrics.advance_x, 20);
            assert_eq!(
                invoke(
                    &mut machine,
                    "RemoveFontResourceW",
                    Some("C:\\Game\\Fonts\\字形.ttf")
                ),
                1
            );
            assert_eq!(glyph(&mut machine).data, fallback.data);
            assert_eq!(
                invoke(
                    &mut machine,
                    "RemoveFontResourceW",
                    Some("C:\\Game\\Fonts\\字形.ttf")
                ),
                0
            );
            assert_eq!(loaded.metrics.advance_x, 20);
            assert_eq!(
                invoke(
                    &mut machine,
                    "AddFontResourceW",
                    Some("Fonts\\collection.ttc")
                ),
                2
            );
            assert_eq!(
                invoke(
                    &mut machine,
                    "RemoveFontResourceA",
                    Some("Fonts/collection.ttc")
                ),
                1
            );
        }
    }

    #[test]
    fn failed_load_remove_and_reference_overflow_preserve_cache_and_existing_resource() {
        let _native = NATIVE.lock().unwrap();
        let (mut machine, _) = fixture(932);
        assert_eq!(
            invoke(&mut machine, "AddFontResourceA", Some("Fonts/fixture.ttf")),
            1
        );
        let cached = glyph(&mut machine);
        for path in [
            Some("Fonts/missing.ttf"),
            Some("Fonts/corrupt.ttf"),
            Some(""),
            None,
        ] {
            for name in [
                "AddFontResourceA",
                "AddFontResourceW",
                "RemoveFontResourceA",
                "RemoveFontResourceW",
            ] {
                assert_eq!(invoke(&mut machine, name, path), 0);
                assert!(Arc::ptr_eq(&cached, &glyph(&mut machine)));
            }
        }
        let key = ("c:\\game\\fonts\\fixture.ttf".to_owned(), 0);
        machine
            .ext
            .get_mut::<Fonts>()
            .resources
            .get_mut(&key)
            .unwrap()
            .count = u32::MAX;
        assert_eq!(
            invoke(&mut machine, "AddFontResourceA", Some("Fonts/fixture.ttf")),
            0
        );
        assert_eq!(machine.ext.get::<Fonts>().resources[&key].count, u32::MAX);
        assert!(Arc::ptr_eq(&cached, &glyph(&mut machine)));
        machine
            .ext
            .get_mut::<Fonts>()
            .resources
            .get_mut(&key)
            .unwrap()
            .count = 1;
        assert_eq!(
            invoke(
                &mut machine,
                "RemoveFontResourceA",
                Some("Fonts/fixture.ttf")
            ),
            1
        );
    }

    #[test]
    fn virtual_file_dirty_pages_are_the_bytes_registered_with_the_backend() {
        let _native = NATIVE.lock().unwrap();
        let (mut machine, directory) = fixture(932);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(directory.join("Fonts/fixture.ttf"))
            .unwrap();
        let section = machine
            .ext
            .get::<Win32>()
            .vfs
            .sections
            .file(file, FONT.len() as u64, true)
            .unwrap();
        let page = section.page(0);
        page.prepare().unwrap();
        page.write(0, &[0; 4]).unwrap();
        let cached = glyph(&mut machine);
        assert_eq!(
            invoke(&mut machine, "AddFontResourceW", Some("Fonts/fixture.ttf")),
            0
        );
        assert!(Arc::ptr_eq(&cached, &glyph(&mut machine)));
        page.write(0, &FONT[..4]).unwrap();
        assert_eq!(
            invoke(&mut machine, "AddFontResourceA", Some("Fonts/fixture.ttf")),
            1
        );
        assert_eq!(glyph(&mut machine).metrics.advance_x, 20);
        assert_eq!(
            std::fs::read(directory.join("Fonts/fixture.ttf")).unwrap(),
            FONT,
            "loading never mutates the base file"
        );
    }

    #[test]
    fn actual_exit_process_retires_resources_after_the_standard_cleanup_path() {
        let _native = NATIVE.lock().unwrap();
        let (mut machine, _) = fixture(932);
        let fallback = glyph(&mut machine);
        assert_eq!(
            invoke(&mut machine, "AddFontResourceA", Some("Fonts/fixture.ttf")),
            1
        );
        assert_eq!(glyph(&mut machine).metrics.advance_x, 20);
        let exit = machine
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        let mut code = Asm::new(CODE);
        code.push_imm(23).call(exit);
        machine.cpu.write(CODE, &code.bytes).unwrap();
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(23)
        ));
        assert!(machine.ext.get::<Fonts>().resources.is_empty());
        assert_eq!(machine.ext.get::<Fonts>().text.resource_stats(), (0, 0));
        assert_eq!(glyph(&mut machine).data, fallback.data);
    }
}

// spec: WIN32-GDI-15
#[test]
fn memory_fonts_register_copies_under_distinct_handles_until_removed() {
    use adv_cpu::{
        JitCpu,
        backend::{Guest, GuestExt, Prot},
    };
    use adv_machine::{Clock, Machine, layout::PARK_ADDR};
    const FONT: &[u8] =
        include_bytes!("../../../../../adv-device/tests/fixtures/font-resource.ttf");
    const COLLECTION: &[u8] =
        include_bytes!("../../../../../adv-device/tests/fixtures/font-resource.ttc");
    const DATA: u32 = 0x410000;
    const COUNT: u32 = 0x40f000;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/memory-font-tests/{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", root.join("data")),
    )
    .unwrap();
    machine.spawn("fonts", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.map(COUNT, 0x1000, Prot::RW).unwrap();
    machine.cpu.map(DATA, 0x2000, Prot::RW).unwrap();
    fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
        Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        }
    }
    let add = |machine: &mut Machine<JitCpu>, bytes: &[u8], count: u32| {
        machine.cpu.write(DATA, bytes).unwrap();
        machine.cpu.write_u32(COUNT, 0xdead).unwrap();
        let handle = add_memory(
            &mut env(machine),
            GuestPtr::new(DATA),
            bytes.len() as u32,
            GuestPtr::default(),
            GuestPtr::new(count),
        );
        (handle.0, machine.cpu.read_u32(COUNT).unwrap())
    };
    let (single, faces) = add(&mut machine, FONT, COUNT);
    assert_ne!(single, 0);
    assert_eq!(faces, 1);
    let (collection, faces) = add(&mut machine, COLLECTION, COUNT);
    assert!(collection != 0 && collection != single);
    assert!(faces > 1, "{faces}");
    // A null count pointer is rejected before anything is registered.
    assert_eq!(add(&mut machine, FONT, 0), (0, 0xdead));
    assert_eq!(machine.ext.get::<Fonts>().memory.len(), 2);
    assert_eq!(remove_memory(&mut env(&mut machine), Handle(single)), 1);
    assert_eq!(remove_memory(&mut env(&mut machine), Handle(single)), 0);
    assert_eq!(machine.ext.get::<Fonts>().memory.len(), 1);
    super::super::detach_process(&mut env(&mut machine)).unwrap();
    assert!(machine.ext.get::<Fonts>().memory.is_empty());
    drop(machine);
    let _ = std::fs::remove_dir_all(root);
}
