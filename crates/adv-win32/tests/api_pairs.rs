//! Paired exports share identities and process-owned state, including cleanup
//! after an unavailable facility and GDI save/modify/restore sequences.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_machine::{Clock, Env, Machine, Outcome, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const DATA: u32 = 0x410000;

fn machine() -> Machine<JitCpu> {
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
        Win32Config::new(".", "api-pairs.exe", "target/scratch/api-pairs"),
    )
    .unwrap();
    machine.spawn("API pairs", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn call(machine: &mut Machine<JitCpu>, module: &str, name: &str, args: &[u32]) -> u32 {
    let entry = machine.resolve_import(module, name).unwrap();
    let index = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let function = machine.sys.thunks.get(index).f;
    machine.cpu.set_reg(Reg::Esp, DATA + 2048);
    for (i, &arg) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 2052 + i as u32 * 4, arg)
            .unwrap();
    }
    let outcome = (function.handler)(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: index,
    });
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("{module}!{name} must return"),
    }
}

fn write_wide(machine: &mut Machine<JitCpu>, pointer: u32, units: &[u16]) {
    let bytes: Vec<_> = units
        .iter()
        .copied()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(pointer, &bytes).unwrap();
}

#[test]
fn wide_strings_keep_code_units_packed_characters_and_native_traversal_boundaries() {
    let mut m = machine();
    write_wide(&mut m, DATA, &[b'a' as u16, 0x301, 0x302, 0xd800, 0x62]);
    assert_eq!(call(&mut m, "user32.dll", "CharNextW", &[DATA]), DATA + 8);
    assert_eq!(
        call(&mut m, "user32.dll", "CharNextW", &[DATA + 6]),
        DATA + 8
    );
    assert_eq!(
        call(&mut m, "user32.dll", "CharNextW", &[DATA + 10]),
        DATA + 10
    );
    assert_eq!(call(&mut m, "user32.dll", "CharUpperW", &[0xe9]), 0xc9);
    assert_eq!(call(&mut m, "user32.dll", "CharUpperW", &[DATA]), DATA);
    assert_eq!(m.cpu.read_u16(DATA).unwrap(), 0x41);
    assert_eq!(m.cpu.read_u16(DATA + 6).unwrap(), 0xd800);
    assert_eq!(call(&mut m, "user32.dll", "CharLowerW", &[DATA]), DATA);
    m.cpu.write(DATA + 100, &[0xcc; 20]).unwrap();
    assert_eq!(
        call(&mut m, "kernel32.dll", "lstrcpynW", &[DATA + 100, 0, 0]),
        DATA + 100
    );
    assert_eq!(m.cpu.read_u16(DATA + 100).unwrap(), 0xcccc);
    assert_eq!(
        call(&mut m, "kernel32.dll", "lstrcpynW", &[DATA + 100, DATA, 4]),
        DATA + 100
    );
    assert_eq!(m.cpu.read_u16(DATA + 104).unwrap(), 0x302);
    assert_eq!(m.cpu.read_u16(DATA + 106).unwrap(), 0);
    assert_eq!(
        call(&mut m, "kernel32.dll", "lstrcpynW", &[DATA + 4094, DATA, 4]),
        0
    );
    assert_eq!(m.cpu.read_u16(DATA + 4094).unwrap(), 0x61);
    write_wide(&mut m, DATA + 200, &[0xc9, 0x41]);
    write_wide(&mut m, DATA + 300, &[0xe9, 0x61]);
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "lstrcmpiW",
            &[DATA + 200, DATA + 300]
        ),
        0
    );
    m.ext.get_mut::<Win32>().codec = adv_os::Codec::new(936);
    m.cpu.write(DATA, &[0xd6, 0xd0, 0]).unwrap();
    assert_eq!(
        call(&mut m, "user32.dll", "OemToCharW", &[DATA, DATA + 100]),
        1
    );
    assert_eq!(m.cpu.read_u16(DATA + 100).unwrap(), 0x4e2d);
}

#[test]
fn console_reads_and_codepage_setters_preserve_the_no_console_boundary() {
    let mut m = machine();
    m.cpu.write(DATA, &[0xaa; 32]).unwrap();
    for name in ["ReadConsoleA", "ReadConsoleW"] {
        assert_eq!(
            call(&mut m, "kernel32.dll", name, &[0, DATA, 10, DATA + 16, 0]),
            0
        );
        assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0xaaaaaaaa);
        assert_eq!(call(&mut m, "kernel32.dll", "GetLastError", &[]), 6);
    }
    let acp = m.ext.get::<Win32>().codec.acp;
    for name in ["GetConsoleCP", "GetConsoleOutputCP"] {
        assert_eq!(call(&mut m, "kernel32.dll", name, &[]), 0);
        assert_eq!(call(&mut m, "kernel32.dll", "GetLastError", &[]), 6);
        assert_eq!(m.ext.get::<Win32>().codec.acp, acp);
    }
    for name in ["SetConsoleCP", "SetConsoleOutputCP"] {
        assert_eq!(call(&mut m, "kernel32.dll", name, &[936]), 0);
        assert_eq!(call(&mut m, "kernel32.dll", "GetLastError", &[]), 6);
        assert_eq!(m.ext.get::<Win32>().codec.acp, acp);
    }
}

#[test]
fn computer_name_and_date_time_pairs_measure_the_callers_encoding() {
    let mut m = machine();
    m.cpu.write(DATA, &[0xaa; 64]).unwrap();
    m.cpu.write_u32(DATA + 100, 7).unwrap();
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "GetComputerNameW",
            &[DATA, DATA + 100]
        ),
        0
    );
    assert_eq!(m.cpu.read_u32(DATA + 100).unwrap(), 8);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0xaaaaaaaa);
    m.cpu.write_u32(DATA + 100, 8).unwrap();
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "GetComputerNameW",
            &[DATA, DATA + 100]
        ),
        1
    );
    assert_eq!(m.cpu.read_u32(DATA + 100).unwrap(), 7);
    assert_eq!(m.cpu.read_u16(DATA + 14).unwrap(), 0);
    let st = [2024u16, 2, 4, 29, 12, 34, 56, 0];
    m.cpu
        .write(
            DATA + 200,
            &st.into_iter()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write(DATA + 300, b"yyyy/MM/dd\0").unwrap();
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "GetDateFormatA",
            &[0x409, 0, DATA + 200, DATA + 300, 0, 0]
        ),
        11
    );
    m.cpu.write(DATA, &[0xcc; 64]).unwrap();
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "GetDateFormatA",
            &[0x409, 0, DATA + 200, DATA + 300, DATA, 10]
        ),
        0
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0xcccccccc);
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "GetDateFormatA",
            &[0x409, 0, DATA + 200, DATA + 300, DATA, 11]
        ),
        11
    );
    assert_eq!(m.cpu.read_cstr(DATA, 64).unwrap(), b"2024/02/29");
    m.cpu.write(DATA + 300, b"HH:mm:ss\0").unwrap();
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "GetTimeFormatA",
            &[0x409, 0, DATA + 200, DATA + 300, DATA, 9]
        ),
        9
    );
    assert_eq!(m.cpu.read_cstr(DATA, 64).unwrap(), b"12:34:56");
}

#[test]
fn create_font_w_keeps_the_original_face_and_shared_object_lifetime() {
    let mut m = machine();
    write_wide(&mut m, DATA, &[0x4e2d, 0x6587, 0xe9]);
    let font = call(
        &mut m,
        "gdi32.dll",
        "CreateFontW",
        &[18, 2, 0, 0, 700, 1, 0, 0, 134, 0, 0, 0, 0, DATA],
    );
    assert_ne!(font, 0);
    assert_eq!(
        call(&mut m, "gdi32.dll", "GetObjectW", &[font, 92, DATA + 200]),
        92
    );
    assert_eq!(m.cpu.read_u32(DATA + 200).unwrap(), 18);
    assert_eq!(m.cpu.read_u16(DATA + 228).unwrap(), 0x4e2d);
    assert_eq!(m.cpu.read_u16(DATA + 232).unwrap(), 0xe9);
    assert_eq!(call(&mut m, "gdi32.dll", "DeleteObject", &[font]), 1);
    assert_eq!(
        call(&mut m, "gdi32.dll", "GetObjectW", &[font, 92, DATA + 200]),
        0
    );
}

#[test]
fn wide_no_device_queries_and_property_sheets_fail_without_publishing_owners() {
    let mut m = machine();
    let before = m.sys.objects.object_count();
    m.cpu.write(DATA, &[0xaa; 100]).unwrap();
    for name in ["waveOutGetDevCapsW", "midiOutGetDevCapsW"] {
        assert_eq!(call(&mut m, "winmm.dll", name, &[0, DATA, 84]), 2);
        assert_eq!(call(&mut m, "winmm.dll", name, &[u32::MAX, DATA, 84]), 6);
        assert_eq!(call(&mut m, "winmm.dll", name, &[0, 0, 0]), 0);
        assert_eq!(call(&mut m, "winmm.dll", name, &[0, DATA + 4094, 84]), 11);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0xaaaaaaaa);
    }
    assert_eq!(
        call(&mut m, "winmm.dll", "mciSendCommandW", &[0, 0x803, 0, 0]),
        297
    );
    m.cpu.write(DATA, &[0; 20]).unwrap();
    write_wide(&mut m, DATA + 100, &[0x4e2d]);
    m.cpu.write_u32(DATA + 8, DATA + 100).unwrap();
    assert_eq!(
        call(
            &mut m,
            "winmm.dll",
            "mciSendCommandW",
            &[0, 0x803, 0x2000, DATA]
        ),
        306
    );
    assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
    assert_eq!(
        call(
            &mut m,
            "winmm.dll",
            "mciGetErrorStringW",
            &[306, DATA + 200, 4]
        ),
        1
    );
    assert_eq!(m.cpu.read_u16(DATA + 206).unwrap(), 0);
    m.cpu.write(DATA, &[0; 56]).unwrap();
    m.cpu.write_u32(DATA, 56).unwrap();
    assert_eq!(
        call(&mut m, "comctl32.dll", "CreatePropertySheetPageW", &[DATA]),
        0
    );
    assert_eq!(call(&mut m, "kernel32.dll", "GetLastError", &[]), 120);
    m.cpu.write_u32(DATA, 52).unwrap();
    assert_eq!(
        call(&mut m, "comctl32.dll", "PropertySheetW", &[DATA]),
        u32::MAX
    );
    assert_eq!(m.sys.objects.object_count(), before);
}

#[test]
fn wide_string_resources_keep_embedded_nuls_and_return_the_original_resource_pointer() {
    use adv_os::module::{Lifetime, Module, ModuleKind, Modules};
    use adv_pe::{ResId, ResourceData, ResourceDir, ResourceEntry, ResourceNode};
    let mut m = machine();
    let base = 0x500000;
    m.cpu.map(base, 4096, Prot::READ).unwrap();
    let units = [0u16, 4, 0x4e2d, 0, 0xd800, 0x61];
    let bytes: Vec<_> = units.into_iter().flat_map(u16::to_le_bytes).collect();
    m.cpu.write(base + 0x200, &bytes).unwrap();
    m.cpu.write_u32(base + 0x100, 0x200).unwrap();
    m.cpu.write_u32(base + 0x104, bytes.len() as u32).unwrap();
    let data = ResourceData {
        entry_rva: 0x100,
        rva: 0x200,
        size: bytes.len() as u32,
        codepage: 0,
    };
    let directory = ResourceDir {
        entries: vec![ResourceEntry {
            id: ResId::Id(6),
            node: ResourceNode::Dir(ResourceDir {
                entries: vec![ResourceEntry {
                    id: ResId::Id(1),
                    node: ResourceNode::Dir(ResourceDir {
                        entries: vec![ResourceEntry {
                            id: ResId::Id(0x409),
                            node: ResourceNode::Data(data),
                        }],
                    }),
                }],
            }),
        }],
    };
    m.ext.get_mut::<Modules>().add(Module {
        name: "strings.dll".into(),
        path: "strings.dll".into(),
        kind: ModuleKind::Image,
        base,
        size: 4096,
        is_dll: true,
        entry: 0,
        stack_reserve: 0,
        exports_by_name: Default::default(),
        exports_by_ordinal: Default::default(),
        resources: Some(directory),
        data_file: None,
        load_count: 1,
        ldr_entry: 0,
        ldr_size: 0,
        lifetime: Lifetime::default(),
    });
    assert_ne!(
        call(
            &mut m,
            "kernel32.dll",
            "FindResourceExW",
            &[base, 6, 1, 0x409]
        ),
        0
    );
    assert_eq!(
        call(&mut m, "user32.dll", "LoadStringW", &[base, 1, DATA, 0]),
        4
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), base + 0x204);
    assert_eq!(
        call(&mut m, "user32.dll", "LoadStringW", &[base, 1, DATA, 4]),
        3
    );
    assert_eq!(m.cpu.read_u16(DATA).unwrap(), 0x4e2d);
    assert_eq!(m.cpu.read_u16(DATA + 2).unwrap(), 0);
    assert_eq!(m.cpu.read_u16(DATA + 4).unwrap(), 0xd800);
    assert_eq!(m.cpu.read_u16(DATA + 6).unwrap(), 0);
    m.cpu.write(DATA, &[0xaa; 8]).unwrap();
    assert_eq!(
        call(&mut m, "user32.dll", "LoadStringW", &[base, 17, DATA, 4]),
        0
    );
    assert_eq!(m.cpu.read_u16(DATA).unwrap(), 0);
    assert_eq!(m.cpu.read_u16(DATA + 2).unwrap(), 0xaaaa);
}

#[test]
fn wide_class_registration_and_unregistration_share_the_existing_class_owner() {
    let mut m = machine();
    write_wide(&mut m, DATA + 100, &[0x4e2d, 0x6587]);
    m.cpu.write(DATA, &[0; 40]).unwrap();
    m.cpu.write_u32(DATA + 4, 0x123456).unwrap();
    m.cpu.write_u32(DATA + 16, 0x400000).unwrap();
    m.cpu.write_u32(DATA + 36, DATA + 100).unwrap();
    let atom = call(&mut m, "user32.dll", "RegisterClassW", &[DATA]);
    assert_ne!(atom, 0);
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "UnregisterClassW",
            &[DATA + 100, 0x400000]
        ),
        1
    );
    assert_eq!(
        call(&mut m, "user32.dll", "UnregisterClassW", &[atom, 0x400000]),
        0
    );
    assert_eq!(call(&mut m, "kernel32.dll", "GetLastError", &[]), 1411);
}

#[test]
fn wide_profile_writes_and_reads_share_guest_acp_bytes_in_the_write_overlay() {
    let mut m = machine();
    for (offset, value) in [
        (0, "日"),
        (100, "key"),
        (200, "文字"),
        (300, r"C:\pairs-profile.ini"),
    ] {
        write_wide(
            &mut m,
            DATA + offset,
            &value.encode_utf16().collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "WritePrivateProfileStringW",
            &[DATA, DATA + 100, DATA + 200, DATA + 300]
        ),
        1
    );
    assert_eq!(
        call(
            &mut m,
            "kernel32.dll",
            "GetPrivateProfileStringW",
            &[DATA, DATA + 100, 0, DATA + 400, 8, DATA + 300]
        ),
        2
    );
    assert_eq!(m.cpu.read_u16(DATA + 400).unwrap(), 0x6587);
    assert_eq!(m.cpu.read_u16(DATA + 402).unwrap(), 0x5b57);
    let path = m
        .ext
        .get::<Win32>()
        .vfs
        .lookup(r"C:\pairs-profile.ini")
        .unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let text = m.ext.get::<Win32>().codec.decode(&bytes);
    assert!(text.contains("[日]\r\nkey=文字"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn the_new_pairs_are_public_registry_entries_before_process_installation() {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    for (module, names) in [
        (
            "kernel32.dll",
            &[
                "lstrcpynW",
                "lstrcmpiW",
                "GetDateFormatA",
                "GetTimeFormatA",
                "FindResourceExW",
                "GetComputerNameW",
                "ReadConsoleA",
                "SearchPathW",
                "WritePrivateProfileStringW",
                "SetConsoleCP",
                "SetConsoleOutputCP",
            ][..],
        ),
        (
            "user32.dll",
            &[
                "CharNextW",
                "CharUpperW",
                "CharLowerW",
                "OemToCharW",
                "LoadStringW",
                "UnregisterClassW",
                "GetClassLongW",
                "SetClassLongW",
                "SendMessageTimeoutW",
            ][..],
        ),
        (
            "gdi32.dll",
            &[
                "CreateFontW",
                "GetCharWidth32A",
                "GetKerningPairsA",
                "GetOutlineTextMetricsW",
            ][..],
        ),
        (
            "winmm.dll",
            &[
                "waveOutGetDevCapsW",
                "midiOutGetDevCapsW",
                "mciSendCommandW",
                "mciGetErrorStringW",
            ][..],
        ),
        (
            "comctl32.dll",
            &["CreatePropertySheetPageW", "PropertySheetW"][..],
        ),
        (
            "comdlg32.dll",
            &["GetOpenFileNameW", "GetSaveFileNameW"][..],
        ),
        (
            "shell32.dll",
            &[
                "SHGetFileInfoW",
                "SHGetPathFromIDListW",
                "SHBrowseForFolderW",
            ][..],
        ),
        (
            "shlwapi.dll",
            &[
                "PathCombineW",
                "PathAppendW",
                "PathRemoveFileSpecW",
                "PathRemoveExtensionW",
                "PathRenameExtensionW",
                "PathAddExtensionW",
                "PathAddBackslashW",
                "PathStripPathW",
                "PathUnquoteSpacesW",
                "PathIsDirectoryW",
                "PathMatchSpecA",
            ][..],
        ),
    ] {
        for name in names {
            assert!(r.dynamic_available(module, name), "{module}!{name}");
        }
    }
}

#[test]
fn shell_path_pairs_preserve_relative_syntax_dbcs_characters_and_output_limits() {
    let mut m = machine();
    write_wide(&mut m, DATA, &r"a\b".encode_utf16().collect::<Vec<_>>());
    write_wide(
        &mut m,
        DATA + 100,
        &r"..\文字".encode_utf16().collect::<Vec<_>>(),
    );
    assert_eq!(
        call(
            &mut m,
            "shlwapi.dll",
            "PathCombineW",
            &[DATA + 200, DATA, DATA + 100]
        ),
        DATA + 200
    );
    let text: Vec<_> = (0..4)
        .map(|i| m.cpu.read_u16(DATA + 200 + i * 2).unwrap())
        .collect();
    assert_eq!(text, "a\\文字".encode_utf16().collect::<Vec<_>>());
    assert_eq!(
        call(&mut m, "shlwapi.dll", "PathAddBackslashW", &[DATA + 200]),
        DATA + 210
    );
    assert_eq!(
        call(&mut m, "shlwapi.dll", "PathRemoveFileSpecW", &[DATA + 200]),
        1
    );
    assert_eq!(m.cpu.read_u16(DATA + 208).unwrap(), 0);
    assert_eq!(
        call(&mut m, "shlwapi.dll", "PathStripPathW", &[DATA + 200]),
        0
    );
    assert_eq!(m.cpu.read_u16(DATA + 200).unwrap(), 0x6587);
    write_wide(&mut m, DATA, &"file".encode_utf16().collect::<Vec<_>>());
    assert_eq!(
        call(&mut m, "shlwapi.dll", "PathAddExtensionW", &[DATA, 0]),
        1
    );
    assert_eq!(
        call(&mut m, "shlwapi.dll", "PathRemoveExtensionW", &[DATA]),
        0
    );
    assert_eq!(m.cpu.read_u16(DATA + 8).unwrap(), 0);
    write_wide(
        &mut m,
        DATA,
        &"x".repeat(259).encode_utf16().collect::<Vec<_>>(),
    );
    assert_eq!(call(&mut m, "shlwapi.dll", "PathAddBackslashW", &[DATA]), 0);
    assert_eq!(m.cpu.read_u16(DATA + 516).unwrap(), 0x78);
    assert_eq!(m.cpu.read_u16(DATA + 518).unwrap(), 0);
    m.cpu.write(DATA, &[b'a', b'\\', 0x95, 0x5c, 0]).unwrap();
    m.cpu.write(DATA + 100, b"b\0").unwrap();
    assert_eq!(
        call(
            &mut m,
            "shlwapi.dll",
            "PathCombineA",
            &[DATA + 200, DATA, DATA + 100]
        ),
        DATA + 200
    );
    assert_eq!(
        m.cpu.read_cstr(DATA + 200, 100).unwrap(),
        [b'a', b'\\', 0x95, 0x5c, b'\\', b'b']
    );
    assert_eq!(
        call(
            &mut m,
            "shlwapi.dll",
            "PathMatchSpecA",
            &[DATA + 200, DATA + 100]
        ),
        0
    );
    m.cpu.write(DATA + 100, b"*\\?\\b\0").unwrap();
    assert_eq!(
        call(
            &mut m,
            "shlwapi.dll",
            "PathMatchSpecA",
            &[DATA + 200, DATA + 100]
        ),
        1
    );
}

#[test]
fn class_scalar_pairs_share_one_class_and_refuse_unavailable_procedure_thunks() {
    use adv_win32::user32::window::ClassKey;
    let mut m = machine();
    write_wide(
        &mut m,
        DATA + 100,
        &"ClassPair".encode_utf16().collect::<Vec<_>>(),
    );
    write_wide(&mut m, DATA + 200, &"日".encode_utf16().collect::<Vec<_>>());
    m.cpu.write(DATA, &[0; 40]).unwrap();
    m.cpu.write_u32(DATA + 4, 0x123456).unwrap();
    m.cpu.write_u32(DATA + 16, 0x400000).unwrap();
    m.cpu.write_u32(DATA + 32, DATA + 200).unwrap();
    m.cpu.write_u32(DATA + 36, DATA + 100).unwrap();
    let atom = call(&mut m, "user32.dll", "RegisterClassW", &[DATA]);
    assert_ne!(atom, 0);
    let hwnd = m.ext.get::<Win32>().user.desktop;
    let index = m
        .ext
        .get::<Win32>()
        .user
        .find_class(&ClassKey::Atom(atom as u16), 0x400000)
        .unwrap();
    let old = m
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&m.sys.objects, &hwnd)
        .unwrap()
        .class;
    m.ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &hwnd)
        .unwrap()
        .class = index;
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "SetClassLongW",
            &[hwnd, (-26i32) as u32, 0x42]
        ),
        0
    );
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "GetClassLongA",
            &[hwnd, (-26i32) as u32]
        ),
        0x42
    );
    let menu = call(
        &mut m,
        "user32.dll",
        "GetClassLongW",
        &[hwnd, (-8i32) as u32],
    );
    assert_ne!(menu, DATA + 200);
    assert_eq!(m.cpu.read_u16(menu).unwrap(), 0x65e5);
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "GetClassLongW",
            &[hwnd, (-8i32) as u32]
        ),
        menu
    );
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "GetClassLongA",
            &[hwnd, (-24i32) as u32]
        ),
        0
    );
    assert_eq!(call(&mut m, "kernel32.dll", "GetLastError", &[]), 50);
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "SetClassLongW",
            &[hwnd, (-24i32) as u32, 0x234567]
        ),
        0x123456
    );
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "SetClassLongA",
            &[hwnd, (-24i32) as u32, 0x345678]
        ),
        0
    );
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "GetClassLongW",
            &[hwnd, (-24i32) as u32]
        ),
        0x234567
    );
    assert_eq!(
        call(
            &mut m,
            "user32.dll",
            "CallWindowProcA",
            &[0x123456, hwnd, 0x8000, 0, 0]
        ),
        0
    );
    assert_eq!(call(&mut m, "kernel32.dll", "GetLastError", &[]), 50);
    m.ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut m.sys.objects, &hwnd)
        .unwrap()
        .class = old;
    assert_eq!(
        call(&mut m, "user32.dll", "UnregisterClassW", &[atom, 0x400000]),
        1
    );
}

#[test]
fn picker_pairs_report_initialization_failure_and_keep_errors_bound_to_the_thread() {
    let mut m = machine();
    let count = m.sys.objects.object_count();
    m.cpu.write(DATA, &[0xcc; 100]).unwrap();
    assert_eq!(call(&mut m, "comdlg32.dll", "CommDlgExtendedError", &[]), 0);
    for name in [
        "GetOpenFileNameA",
        "GetOpenFileNameW",
        "GetSaveFileNameA",
        "GetSaveFileNameW",
    ] {
        assert_eq!(call(&mut m, "comdlg32.dll", name, &[DATA]), 0);
        assert_eq!(call(&mut m, "comdlg32.dll", "CommDlgExtendedError", &[]), 2);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0xcccccccc);
    }
    for (name, args) in [
        ("SHBrowseForFolderW", vec![DATA]),
        ("SHGetPathFromIDListW", vec![0, DATA]),
        ("SHGetFileInfoW", vec![0, 0, DATA, 100, 0]),
    ] {
        assert_eq!(call(&mut m, "shell32.dll", name, &args), 0);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0xcccccccc);
    }
    assert_eq!(m.sys.objects.object_count(), count);
    m.spawn("dialog second thread", PARK_ADDR, &[], 65536)
        .unwrap();
    let caller = m.sys.current;
    m.sys.current = 1;
    assert_eq!(call(&mut m, "comdlg32.dll", "CommDlgExtendedError", &[]), 0);
    assert_eq!(call(&mut m, "comdlg32.dll", "GetOpenFileNameW", &[0]), 0);
    assert_eq!(call(&mut m, "comdlg32.dll", "CommDlgExtendedError", &[]), 2);
    m.sys.current = caller;
    assert_eq!(call(&mut m, "comdlg32.dll", "CommDlgExtendedError", &[]), 2);
}

#[test]
fn missing_zw_pairs_are_discoverable_before_install_and_share_transport_identity() {
    let names = [
        "Continue",
        "CreateEvent",
        "CreateMutant",
        "CreateSemaphore",
        "CreateThreadEx",
        "DelayExecution",
        "FlushInstructionCache",
        "GetContextThread",
        "OpenEvent",
        "OpenMutant",
        "OpenSemaphore",
        "QueryAttributesFile",
        "QueryFullAttributesFile",
        "QueueApcThread",
        "RaiseException",
        "ReadVirtualMemory",
        "ReleaseMutant",
        "ReleaseSemaphore",
        "ResetEvent",
        "ResumeThread",
        "SetContextThread",
        "SetEvent",
        "SuspendThread",
        "TerminateThread",
        "TestAlert",
        "WaitForMultipleObjects",
        "WaitForSingleObject",
        "WriteVirtualMemory",
    ];
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for suffix in names {
        let (nt, zw) = (format!("Nt{suffix}"), format!("Zw{suffix}"));
        assert!(registry.dynamic_available("ntdll.dll", &zw), "{zw}");
        let original = registry.get("ntdll.dll", &nt).unwrap();
        let alias = registry.get("ntdll.dll", &zw).unwrap();
        assert_eq!(alias.conv, original.conv);
        assert_eq!(alias.data, original.data);
        assert!(std::ptr::fn_addr_eq(alias.handler, original.handler));
    }
    let mut machine = machine();
    for suffix in names {
        assert_eq!(
            machine
                .resolve_import("ntdll.dll", &format!("Nt{suffix}"))
                .unwrap(),
            machine
                .resolve_import("ntdll.dll", &format!("Zw{suffix}"))
                .unwrap()
        );
    }
    let event = machine.sys.objects.insert(adv_machine::Object::Event {
        manual_reset: true,
        signaled: false,
    });
    assert_eq!(
        call(&mut machine, "ntdll.dll", "ZwSetEvent", &[event, DATA]),
        0
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(
        call(&mut machine, "ntdll.dll", "NtResetEvent", &[event, DATA]),
        0
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 1);
}

#[test]
fn absent_caret_cleanup_returns_without_adding_an_owner_or_a_gap() {
    let mut machine = machine();
    let window = machine.ext.get::<Win32>().user.desktop;
    let count = machine.sys.objects.object_count();
    assert_eq!(
        call(
            &mut machine,
            "user32.dll",
            "CreateCaret",
            &[window, 0, 1, 1]
        ),
        0
    );
    for (name, args) in [
        ("DestroyCaret", vec![]),
        ("ShowCaret", vec![0]),
        ("HideCaret", vec![window]),
        ("SetCaretPos", vec![3, 4]),
    ] {
        assert_eq!(call(&mut machine, "user32.dll", name, &args), 0, "{name}");
        assert_eq!(call(&mut machine, "kernel32.dll", "GetLastError", &[]), 5);
    }
    assert_eq!(
        call(&mut machine, "user32.dll", "HideCaret", &[0x12345678]),
        0
    );
    assert_eq!(
        call(&mut machine, "kernel32.dll", "GetLastError", &[]),
        1400
    );
    machine.cpu.write(DATA, &[0xaa; 8]).unwrap();
    assert_eq!(call(&mut machine, "user32.dll", "GetCaretPos", &[DATA]), 1);
    let mut point = [1; 8];
    machine.cpu.read(DATA, &mut point).unwrap();
    assert_eq!(point, [0; 8]);
    assert_eq!(call(&mut machine, "user32.dll", "GetCaretPos", &[0]), 0);
    assert_eq!(machine.sys.objects.object_count(), count);
    assert_eq!(
        machine.ext.get::<adv_abi::ApiDiagnostics>().warnings.len(),
        1
    );
}

#[test]
fn gdi_queries_observe_the_same_saved_dc_state() {
    let mut machine = machine();
    let dc = call(&mut machine, "gdi32.dll", "CreateCompatibleDC", &[0]);
    assert_ne!(dc, 0);
    assert_eq!(call(&mut machine, "gdi32.dll", "GetTextColor", &[dc]), 0);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetBkColor", &[dc]),
        0xffffff
    );
    assert_eq!(call(&mut machine, "gdi32.dll", "GetBkMode", &[dc]), 2);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetStretchBltMode", &[dc]),
        1
    );
    let saved = call(&mut machine, "gdi32.dll", "SaveDC", &[dc]);
    for (name, args) in [
        ("SetTextColor", vec![dc, 0x123456]),
        ("SetBkColor", vec![dc, 0x654321]),
        ("SetBkMode", vec![dc, 1]),
        ("SetViewportOrgEx", vec![dc, (-7i32) as u32, 9, 0]),
    ] {
        call(&mut machine, "gdi32.dll", name, &args);
    }
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetTextColor", &[dc]),
        0x123456
    );
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetBkColor", &[dc]),
        0x654321
    );
    assert_eq!(call(&mut machine, "gdi32.dll", "GetBkMode", &[dc]), 1);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetViewportOrgEx", &[dc, DATA]),
        1
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), (-7i32) as u32);
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), 9);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "RestoreDC", &[dc, saved]),
        1
    );
    assert_eq!(call(&mut machine, "gdi32.dll", "GetTextColor", &[dc]), 0);
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetBkColor", &[dc]),
        0xffffff
    );
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetViewportOrgEx", &[dc, DATA]),
        1
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    for name in ["GetTextColor", "GetBkColor"] {
        assert_eq!(call(&mut machine, "gdi32.dll", name, &[0]), u32::MAX);
    }
    for name in ["GetBkMode", "GetStretchBltMode"] {
        assert_eq!(call(&mut machine, "gdi32.dll", name, &[0]), 0);
    }
    machine.cpu.write(DATA, &[0xaa; 8]).unwrap();
    assert_eq!(
        call(&mut machine, "gdi32.dll", "GetViewportOrgEx", &[0, DATA]),
        0
    );
    let mut unchanged = [0; 8];
    machine.cpu.read(DATA, &mut unchanged).unwrap();
    assert_eq!(unchanged, [0xaa; 8]);
}

#[cfg(windows)]
#[test]
fn no_caret_contract_matches_the_native_thread_queue() {
    #[link(name = "user32")]
    unsafe extern "system" {
        fn DestroyCaret() -> i32;
        fn ShowCaret(window: *mut core::ffi::c_void) -> i32;
        fn HideCaret(window: *mut core::ffi::c_void) -> i32;
        fn SetCaretPos(x: i32, y: i32) -> i32;
        fn GetCaretPos(point: *mut [i32; 2]) -> i32;
    }
    let mut point = [123, 456];
    unsafe {
        assert_eq!(DestroyCaret(), 0);
        assert_eq!(ShowCaret(core::ptr::null_mut()), 0);
        assert_eq!(HideCaret(core::ptr::null_mut()), 0);
        assert_eq!(SetCaretPos(7, 9), 0);
        assert_ne!(GetCaretPos(&mut point), 0);
    }
    assert_eq!(point, [0, 0]);
}
