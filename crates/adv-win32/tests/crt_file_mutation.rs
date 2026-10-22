//! Authored CRT file operations share the Windows VFS and real error cells.
use adv_abi::{AnsiStr, Conv, GuestPtr, Registry, WideStr, signature};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::Heaps;
use adv_win32::{Win32, Win32Config};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
static NEXT: AtomicUsize = AtomicUsize::new(0);
const CASES: [(u32, u32); 6] = [(1, 73), (0, 5), (0, 183), (0, 1113), (0, u32::MAX), (0, 0)];
fn mutation(name: &str) -> bool {
    matches!(
        name.strip_prefix("_o_").unwrap_or(name),
        "remove" | "_unlink" | "_wremove" | "_wunlink" | "rename" | "_wrename"
    )
}
fn root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "hitode-crt-mutations-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    root
}
fn machine(jit: bool, root: &std::path::Path, cp: u32, provider: bool) -> Machine<JitCpu> {
    configured_machine(jit, root, cp, provider, false)
}
fn configured_machine(
    jit: bool,
    root: &std::path::Path,
    cp: u32,
    provider: bool,
    fail_allocation: bool,
) -> Machine<JitCpu> {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    adv_win32::msvcrt::register(&mut r);
    if provider {
        for (name, argc) in [
            ("DeleteFileA", 1),
            ("DeleteFileW", 1),
            ("MoveFileA", 2),
            ("MoveFileW", 2),
            ("MoveFileExA", 3),
            ("MoveFileExW", 3),
        ] {
            r.add("kernel32.dll", name, Conv::Stdcall { argc }, provided);
        }
    }
    assert!(r.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    let mut config = Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    config.os.codepage = cp;
    Win32::install(&mut m, config).unwrap();
    if fail_allocation {
        allocation_result(&mut m, 0);
    }
    m.cpu.map(CODE, 65536, Prot::RX).unwrap();
    m.cpu.map(DATA, 65536, Prot::RW).unwrap();
    m.ext.insert(Provider::default());
    m
}
fn allocation_result(m: &mut Machine<JitCpu>, value: u32) {
    let (win32, modules) = m.ext.get2_mut::<Win32, adv_os::Modules>();
    let mut loader = adv_os::Loader::new(&mut m.cpu, &mut m.sys, modules, &win32.vfs);
    let id = loader.load("ntdll.dll").unwrap();
    let entry = loader
        .resolve_dynamic(
            id,
            &adv_pe::ImportName::Name {
                hint: 0,
                name: "RtlAllocateHeap".into(),
            },
        )
        .unwrap()
        .0;
    let mut bytes = vec![0xb8];
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes.extend_from_slice(&[0xc2, 12, 0]);
    m.cpu.write(entry, &bytes).unwrap();
}

fn invoke(
    m: &mut Machine<JitCpu>,
    a: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    output: u32,
) {
    for &arg in args.iter().rev() {
        a.push_imm(arg);
    }
    a.call(m.resolve_import(module, name).unwrap());
    if !args.is_empty() {
        a.add_esp(args.len() as u8 * 4);
    }
    a.mov_mem_eax(output);
}
fn finish(m: &mut Machine<JitCpu>, mut a: Asm) {
    a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("CRT mutation fixture", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(10_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}
fn text(m: &mut Machine<JitCpu>, pointer: u32, value: &str, wide: bool) {
    let bytes = if wide {
        value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        let mut bytes = m.ext.get::<Win32>().codec.encode(value);
        bytes.push(0);
        bytes
    };
    m.cpu.write(pointer, &bytes).unwrap();
}
fn owner(module: &str) -> &str {
    if module.starts_with("api-ms-win-crt-") {
        "ucrtbase.dll"
    } else {
        module
    }
}
fn reset_errors(m: &mut Machine<JitCpu>, a: &mut Asm, module: &str) {
    for (name, pointer, value) in [
        ("_errno", DATA + 0x7000, 71u32),
        ("__doserrno", DATA + 0x7004, 83),
    ] {
        invoke(m, a, owner(module), name, &[], pointer);
        a.bytes.extend_from_slice(&[0xc7, 0]);
        a.bytes.extend_from_slice(&value.to_le_bytes());
    }
    a.push_imm(73)
        .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
}
fn collect_errors(m: &mut Machine<JitCpu>, a: &mut Asm, output: u32) {
    for (pointer, offset) in [(DATA + 0x7000, 4), (DATA + 0x7004, 8)] {
        a.mov_eax_mem(pointer)
            .mov_eax_ptr_eax()
            .mov_mem_eax(output + offset);
    }
    a.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
        .mov_mem_eax(output + 12);
}

// spec: CRT-LIB-22, CRT-LIB-24
#[test]
fn cataloged_mutations_share_real_overlay_operations_in_both_filename_codepages() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| mutation(s.name))
        .collect();
    assert_eq!(declarations.len(), 78);
    let modules: BTreeSet<_> = declarations.iter().map(|s| s.module).collect();
    for jit in [false, true] {
        for cp in [932, 936] {
            for module in &modules {
                let root = root();
                let mut m = machine(jit, &root, cp, false);
                let mut a = Asm::new(CODE);
                let mut expected = Vec::new();
                for (i, s) in declarations
                    .iter()
                    .filter(|s| s.module == *module)
                    .enumerate()
                {
                    let name = s.name.strip_prefix("_o_").unwrap_or(s.name);
                    let wide = name.starts_with("_w");
                    let rename = name.ends_with("rename");
                    let source = format!("保存{i}.dat");
                    let target = format!("移動{i}.dat");
                    std::fs::write(root.join("game").join(&source), b"authored CRT data").unwrap();
                    let input = DATA + i as u32 * 256;
                    text(&mut m, input, &source, wide);
                    text(&mut m, input + 128, &target, wide);
                    reset_errors(&mut m, &mut a, module);
                    let args = if rename {
                        vec![input, input + 128]
                    } else {
                        vec![input]
                    };
                    invoke(
                        &mut m,
                        &mut a,
                        module,
                        s.name,
                        &args,
                        DATA + 0x4000 + i as u32 * 16,
                    );
                    collect_errors(&mut m, &mut a, DATA + 0x4000 + i as u32 * 16);
                    expected.push((source, target, rename));
                }
                finish(&mut m, a);
                for (i, (source, target, rename)) in expected.into_iter().enumerate() {
                    let output = DATA + 0x4000 + i as u32 * 16;
                    assert_eq!(
                        m.cpu.read_u32(output).unwrap(),
                        0,
                        "{module} {cp} {jit} {i}"
                    );
                    assert_eq!(m.cpu.read_u32(output + 4).unwrap(), 71);
                    assert_eq!(m.cpu.read_u32(output + 8).unwrap(), 83);
                    assert_eq!(m.cpu.read_u32(output + 12).unwrap(), 73);
                    let vfs = &m.ext.get::<Win32>().vfs;
                    assert!(vfs.lookup(&source).is_none(), "{module} {source}");
                    if rename {
                        assert_eq!(
                            std::fs::read(vfs.lookup(&target).unwrap()).unwrap(),
                            b"authored CRT data"
                        );
                    } else {
                        assert!(vfs.lookup(&target).is_none());
                    }
                    assert!(root.join("game").join(source).exists());
                }
                drop(m);
                std::fs::remove_dir_all(root).unwrap();
            }
        }
    }
}

#[derive(Default)]
struct Provider {
    forced: Option<(u32, u32)>,
    seen: Vec<(String, Vec<Option<String>>, Vec<u32>)>,
}
fn provided(env: &mut Env<'_>) -> Outcome {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    let wide = name.ends_with('W');
    let rename = name.starts_with("Move");
    let mut pointers = vec![env.arg(0)];
    if rename {
        pointers.push(env.arg(1));
    }
    let paths = pointers
        .iter()
        .map(|&p| {
            if p == 0 {
                None
            } else if wide {
                Some(
                    String::from_utf16(&WideStr(GuestPtr::new(p)).read(env.cpu, 32768).unwrap())
                        .unwrap(),
                )
            } else {
                Some(
                    env.ext
                        .get::<Win32>()
                        .codec
                        .decode(&AnsiStr(GuestPtr::new(p)).read(env.cpu, 32768).unwrap()),
                )
            }
        })
        .collect();
    if name.contains("Ex") {
        assert_eq!(env.arg(2), 2);
    }
    let state = env.ext.get_mut::<Provider>();
    let index = state.seen.len() % CASES.len();
    state.seen.push((name, paths, pointers));
    let (value, error) = state.forced.unwrap_or(CASES[index]);
    adv_win32::util::set_last_error(env, error);
    Outcome::Return(value)
}

// spec: CRT-LIB-22, CRT-LIB-23, CRT-LIB-24
#[test]
fn live_windows_results_control_errno_aliases_and_versioned_zero_error_policy() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| mutation(s.name))
        .collect();
    let modules: BTreeSet<_> = declarations.iter().map(|s| s.module).collect();
    for jit in [false, true] {
        for module in &modules {
            let root = root();
            let mut m = machine(jit, &root, 936, true);
            text(&mut m, DATA, "保存.dat", false);
            text(&mut m, DATA + 128, "移動.dat", false);
            text(&mut m, DATA + 256, "保存.dat", true);
            text(&mut m, DATA + 384, "移動.dat", true);
            let modern = owner(module) == "ucrtbase.dll";
            let mut a = Asm::new(CODE);
            let mut expected = Vec::new();
            for s in declarations.iter().filter(|s| s.module == *module) {
                let name = s.name.strip_prefix("_o_").unwrap_or(s.name);
                let input_wide = name.starts_with("_w");
                let wide = input_wide || modern;
                let rename = name.ends_with("rename");
                let api = match (wide, rename, modern) {
                    (false, false, _) => "DeleteFileA",
                    (true, false, _) => "DeleteFileW",
                    (false, true, _) => "MoveFileA",
                    (true, true, false) => "MoveFileW",
                    (true, true, true) => "MoveFileExW",
                };
                for (case, (value, error)) in CASES.into_iter().enumerate() {
                    let mut args = vec![if case == 5 {
                        0
                    } else if input_wide {
                        DATA + 256
                    } else {
                        DATA
                    }];
                    if rename {
                        args.push(if input_wide { DATA + 384 } else { DATA + 128 });
                    }
                    reset_errors(&mut m, &mut a, module);
                    let output = DATA + 0x4000 + expected.len() as u32 * 16;
                    invoke(&mut m, &mut a, module, s.name, &args, output);
                    collect_errors(&mut m, &mut a, output);
                    let (result, errno, dos) = if value != 0 || (!modern && error == 0) {
                        (0, 71, 83)
                    } else {
                        (
                            u32::MAX,
                            match error {
                                5 => 13,
                                183 => 17,
                                1113 if modern => 42,
                                _ => 22,
                            },
                            error,
                        )
                    };
                    expected.push((api, case == 5, rename, result, errno, dos, error));
                }
            }
            finish(&mut m, a);
            assert_eq!(m.ext.get::<Provider>().seen.len(), expected.len());
            for (i, (api, null, rename, result, errno, dos, error)) in
                expected.into_iter().enumerate()
            {
                let output = DATA + 0x4000 + i as u32 * 16;
                assert_eq!(
                    m.cpu.read_u32(output).unwrap(),
                    result,
                    "{module} {jit} {i}"
                );
                assert_eq!(m.cpu.read_u32(output + 4).unwrap(), errno);
                assert_eq!(m.cpu.read_u32(output + 8).unwrap(), dos);
                assert_eq!(m.cpu.read_u32(output + 12).unwrap(), error);
                let (name, paths, pointers) = &m.ext.get::<Provider>().seen[i];
                assert_eq!(name, api);
                assert_eq!(
                    paths[0],
                    if null {
                        None
                    } else {
                        Some("保存.dat".into())
                    }
                );
                if rename {
                    assert_eq!(paths[1], Some("移動.dat".into()));
                }
                if modern {
                    for &p in pointers {
                        if p != 0 && !(DATA..DATA + 65536).contains(&p) {
                            assert!(m.ext.get::<Heaps>().owner(&m.cpu, p).is_none());
                        }
                    }
                }
            }
            drop(m);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

// spec: CRT-LIB-23
#[test]
fn dosmaperr_versions_retain_first_match_ranges_unknowns_and_last_error() {
    let cases = [
        (0, 22),
        (1, 22),
        (2, 2),
        (3, 2),
        (4, 24),
        (5, 13),
        (6, 9),
        (7, 12),
        (8, 12),
        (9, 12),
        (10, 7),
        (11, 8),
        (12, 22),
        (13, 22),
        (15, 2),
        (16, 13),
        (17, 18),
        (18, 2),
        (19, 13),
        (33, 13),
        (36, 13),
        (37, 22),
        (53, 2),
        (65, 13),
        (67, 2),
        (80, 17),
        (82, 13),
        (83, 13),
        (87, 22),
        (89, 11),
        (108, 13),
        (109, 32),
        (112, 28),
        (114, 9),
        (128, 10),
        (129, 10),
        (130, 9),
        (131, 22),
        (132, 13),
        (145, 41),
        (158, 13),
        (161, 2),
        (164, 11),
        (167, 13),
        (183, 17),
        (187, 22),
        (188, 8),
        (202, 8),
        (203, 22),
        (206, 2),
        (215, 11),
        (1113, 22),
        (1816, 12),
        (u32::MAX, 22),
    ];
    let modules: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| s.name == "_dosmaperr")
        .map(|s| s.module)
        .collect();
    assert_eq!(modules.len(), 5);
    for jit in [false, true] {
        for module in &modules {
            let root = root();
            let mut m = machine(jit, &root, 932, false);
            let mut a = Asm::new(CODE);
            reset_errors(&mut m, &mut a, module);
            for (i, (code, _)) in cases.iter().enumerate() {
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_dosmaperr",
                    &[*code],
                    DATA + 0x4000 + i as u32 * 16,
                );
                collect_errors(&mut m, &mut a, DATA + 0x4000 + i as u32 * 16);
            }
            finish(&mut m, a);
            for (i, (code, errno)) in cases.iter().enumerate() {
                let output = DATA + 0x4000 + i as u32 * 16;
                assert_eq!(
                    m.cpu.read_u32(output + 4).unwrap(),
                    *errno,
                    "{module} {jit} {code}"
                );
                assert_eq!(m.cpu.read_u32(output + 8).unwrap(), *code);
                assert_eq!(m.cpu.read_u32(output + 12).unwrap(), 73);
            }
            drop(m);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

// spec: CRT-LIB-24
#[test]
fn malformed_modern_paths_fail_before_windows_mutation_and_without_validation_callbacks() {
    for jit in [false, true] {
        for cp in [932, 936] {
            let root = root();
            let mut m = machine(jit, &root, cp, true);
            m.cpu.write(DATA, &[0x81, 0]).unwrap();
            text(&mut m, DATA + 128, "保存.dat", false);
            let mut callback = Asm::new(CODE + 0xf000);
            callback.bytes.extend_from_slice(&[0xff, 0x05]);
            callback
                .bytes
                .extend_from_slice(&(DATA + 0x8000).to_le_bytes());
            callback.ret();
            m.cpu.write(CODE + 0xf000, &callback.bytes).unwrap();
            let mut a = Asm::new(CODE);
            invoke(
                &mut m,
                &mut a,
                "ucrtbase.dll",
                "_set_invalid_parameter_handler",
                &[CODE + 0xf000],
                DATA + 0x6000,
            );
            let cases = [
                ("remove", vec![DATA]),
                ("_unlink", vec![DATA]),
                ("rename", vec![DATA, DATA + 128]),
                ("rename", vec![DATA + 128, DATA]),
                ("_o_remove", vec![DATA]),
            ];
            for (i, (name, args)) in cases.iter().enumerate() {
                reset_errors(&mut m, &mut a, "ucrtbase.dll");
                invoke(
                    &mut m,
                    &mut a,
                    "ucrtbase.dll",
                    name,
                    args,
                    DATA + 0x4000 + i as u32 * 16,
                );
                collect_errors(&mut m, &mut a, DATA + 0x4000 + i as u32 * 16);
            }
            finish(&mut m, a);
            for i in 0..cases.len() as u32 {
                assert_eq!(m.cpu.read_u32(DATA + 0x4000 + i * 16).unwrap(), u32::MAX);
                assert_eq!(m.cpu.read_u32(DATA + 0x4004 + i * 16).unwrap(), 42);
                assert_eq!(m.cpu.read_u32(DATA + 0x4008 + i * 16).unwrap(), 1113);
                assert_eq!(m.cpu.read_u32(DATA + 0x400c + i * 16).unwrap(), 1113);
            }
            assert!(m.ext.get::<Provider>().seen.is_empty());
            assert_eq!(m.cpu.read_u32(DATA + 0x8000).unwrap(), 0);
            drop(m);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}

// spec: CRT-LIB-24
#[test]
fn modern_conversion_allocation_failure_preserves_dos_error_and_never_mutates_a_file() {
    for jit in [false, true] {
        let root = root();
        let mut m = configured_machine(jit, &root, 936, true, true);
        text(&mut m, DATA, "保存.dat", false);
        text(&mut m, DATA + 128, "移動.dat", false);
        m.cpu.write(DATA + 256, &[0x81, 0]).unwrap();
        let mut a = Asm::new(CODE);
        for (i, (name, args)) in [
            ("remove", vec![DATA]),
            ("rename", vec![DATA, DATA + 128]),
            ("rename", vec![DATA, DATA + 256]),
        ]
        .into_iter()
        .enumerate()
        {
            reset_errors(&mut m, &mut a, "ucrtbase.dll");
            invoke(
                &mut m,
                &mut a,
                "ucrtbase.dll",
                name,
                &args,
                DATA + 0x4000 + i as u32 * 16,
            );
            collect_errors(&mut m, &mut a, DATA + 0x4000 + i as u32 * 16);
        }
        finish(&mut m, a);
        for i in 0..3 {
            assert_eq!(m.cpu.read_u32(DATA + 0x4000 + i * 16).unwrap(), u32::MAX);
            assert_eq!(m.cpu.read_u32(DATA + 0x4004 + i * 16).unwrap(), 12);
            assert_eq!(m.cpu.read_u32(DATA + 0x4008 + i * 16).unwrap(), 83);
            assert_eq!(m.cpu.read_u32(DATA + 0x400c + i * 16).unwrap(), 73);
        }
        assert!(m.ext.get::<Provider>().seen.is_empty());
        drop(m);
        std::fs::remove_dir_all(root).unwrap();
    }
}

// spec: CRT-LIB-24, PERS-ABI-1
#[test]
fn a_heap_provider_cannot_turn_a_readonly_conversion_buffer_into_writable_storage() {
    for jit in [false, true] {
        let root = root();
        let mut m = machine(jit, &root, 936, true);
        let buffer = DATA + 0x9000;
        m.cpu.write(buffer, &[0xaa; 32]).unwrap();
        m.cpu.protect(buffer, 4096, Prot::READ).unwrap();
        allocation_result(&mut m, buffer);
        text(&mut m, DATA, "保存.dat", false);
        let mut a = Asm::new(CODE);
        reset_errors(&mut m, &mut a, "ucrtbase.dll");
        invoke(
            &mut m,
            &mut a,
            "ucrtbase.dll",
            "remove",
            &[DATA],
            DATA + 0x4000,
        );
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        m.cpu.write(CODE, &a.bytes).unwrap();
        m.spawn("protected CRT conversion", CODE, &[], 65536)
            .unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Failed(_)), "{jit}: {result:?}");
        assert!(m.ext.get::<Provider>().seen.is_empty());
        assert_eq!(m.cpu.read_u32(buffer).unwrap(), 0xaaaaaaaa);
        drop(m);
        std::fs::remove_dir_all(root).unwrap();
    }
}
fn protect_errors(env: &mut Env<'_>) -> Outcome {
    let pointer = env.cpu.read_u32(DATA + 0x7000).unwrap();
    env.cpu.protect(pointer & !4095, 4096, Prot::READ).unwrap();
    Outcome::Return(0)
}
// spec: CRT-LIB-22, CRT-LIB-23, PERS-ABI-1
#[test]
fn dos_translation_obeys_guest_protection_instead_of_publishing_through_controller_writes() {
    for jit in [false, true] {
        let root = root();
        let mut m = machine(jit, &root, 936, true);
        m.ext.get_mut::<Provider>().forced = Some((0, 5));
        m.sys.thunks.resolver_mut::<Registry>().unwrap().add(
            "authored-crt-mutation",
            "protect-errors",
            Conv::Cdecl { argc: 0 },
            protect_errors,
        );
        text(&mut m, DATA, "保存.dat", false);
        let mut a = Asm::new(CODE);
        reset_errors(&mut m, &mut a, "msvcr90.dll");
        a.call(
            m.resolve_import("authored-crt-mutation", "protect-errors")
                .unwrap(),
        );
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "remove",
            &[DATA],
            DATA + 0x4000,
        );
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        m.cpu.write(CODE, &a.bytes).unwrap();
        m.spawn("protected CRT errors", CODE, &[], 65536).unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Failed(_)), "{jit}: {result:?}");
        let pointer = m.cpu.read_u32(DATA + 0x7000).unwrap();
        assert_eq!(m.cpu.read_u32(pointer).unwrap(), 71);
        assert_eq!(m.cpu.read_u32(pointer + 4).unwrap(), 83);
        drop(m);
        std::fs::remove_dir_all(root).unwrap();
    }
}
