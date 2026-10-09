//! Authored native file I/O and queries; x86 oracles live under target.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU32, Ordering},
};

use adv_abi::{ApiDiagnostics, Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{FileObj, Win32, Win32Config};

#[path = "nt_file/write_tests.rs"]
mod write_tests;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const SOURCE: u32 = 0x430000;
const MARKER: u32 = 0x12345678;
const INFORMATION: u32 = 0xabcdef01;
static NEXT: AtomicU32 = AtomicU32::new(0);

fn fixture<B: Backend>(cpu: B) -> (Machine<B>, PathBuf) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/nt-file-tests/{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(root.join("game/dir")).unwrap();
    std::fs::create_dir_all(root.join("resources")).unwrap();
    std::fs::write(root.join("game/present.txt"), b"base").unwrap();
    std::fs::write(root.join("resources/present.txt"), b"replacement!").unwrap();
    std::fs::write(root.join("game/字🌻.dat"), b"counted").unwrap();
    let mut config = Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    config.os.resource_dir = Some(root.join("resources"));
    Win32::install(&mut m, config).unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.map(SOURCE, 0x10000, Prot::RW).unwrap();
    (m, root)
}

fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("NT file", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

fn input<B: Backend>(m: &mut Machine<B>, base: u32, source: u32, path: &str) {
    let mut bytes: Vec<u8> = path.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let length = bytes.len() as u16;
    // No NUL terminator: the suffix must not become part of the counted name.
    bytes.extend_from_slice(&[b'X', 0, b'X', 0]);
    m.cpu.write(source, &bytes).unwrap();
    m.cpu.write(base, &[0xcd; 128]).unwrap();
    m.cpu.write_u32(base, MARKER).unwrap();
    m.cpu.write_u32(base + 8, MARKER).unwrap();
    m.cpu.write_u32(base + 12, INFORMATION).unwrap();
    let oa = [24u32, 0, base + 64, 0x40, 0, 0];
    m.cpu
        .write(
            base + 32,
            &oa.into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write(base + 64, &length.to_le_bytes()).unwrap();
    m.cpu.write(base + 66, &(length + 4).to_le_bytes()).unwrap();
    m.cpu.write_u32(base + 68, source).unwrap();
}

fn create<B: Backend>(m: &mut Machine<B>, code: &mut Asm, args: [u32; 11], result: u32) {
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let open = m.resolve_import("ntdll.dll", "ZwCreateFile").unwrap();
    code.push_imm(MARKER).call(set);
    for value in args.into_iter().rev() {
        code.push_imm(value);
    }
    code.call(open).mov_mem_eax(result);
    code.call(get).mov_mem_eax(result + 4);
}

fn arguments(base: u32) -> [u32; 11] {
    [
        base,
        0x80100080,
        base + 32,
        base + 8,
        0,
        0x80,
        1,
        1,
        0x60,
        0,
        0,
    ]
}

fn lifecycle<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    input(&mut m, DATA, SOURCE, r"\??\C:\Game\PRESENT.txt");
    input(&mut m, DATA + 128, SOURCE + 512, r"\??\C:\Game\字🌻.dat");
    let read = m.resolve_import("kernel32.dll", "ReadFile").unwrap();
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let mut code = Asm::new(CODE);
    for index in 0..2u32 {
        let base = DATA + index * 128;
        create(&mut m, &mut code, arguments(base), base + 16);
        code.push_imm(0)
            .push_imm(base + 24)
            .push_imm(12)
            .push_imm(DATA + 0x800 + index * 32)
            .mov_eax_mem(base)
            .push_eax()
            .call(read)
            .mov_mem_eax(base + 28);
        code.mov_eax_mem(base)
            .push_eax()
            .call(close)
            .mov_mem_eax(base + 72);
    }
    run(&mut m, code);
    for (index, expected) in [b"replacement!".as_slice(), b"counted".as_slice()]
        .into_iter()
        .enumerate()
    {
        let base = DATA + index as u32 * 128;
        assert_ne!(m.cpu.read_u32(base).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 8).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 12).unwrap(), 1);
        assert_eq!(m.cpu.read_u32(base + 16).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 20).unwrap(), MARKER);
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), expected.len() as u32);
        assert_eq!(m.cpu.read_u32(base + 28).unwrap(), 1);
        assert_eq!(m.cpu.read_u32(base + 72).unwrap(), 1);
        let mut bytes = vec![0; expected.len()];
        m.cpu
            .read(DATA + 0x800 + index as u32 * 32, &mut bytes)
            .unwrap();
        assert_eq!(bytes, expected);
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    assert_eq!(
        std::fs::read(root.join("game/present.txt")).unwrap(),
        b"base"
    );
    assert_eq!(
        std::fs::read(root.join("resources/present.txt")).unwrap(),
        b"replacement!"
    );
    assert!(
        !root.join("data/overlay").exists(),
        "read-only opens never create overlay directories"
    );
}

fn attributes<B: Backend>(cpu: B) {
    let (mut m, _root) = fixture(cpu);
    // The native x86 oracle opens files case-insensitively for Attributes 0,
    // OBJ_OPENIF and OBJ_CASE_INSENSITIVE | OBJ_OPENIF; a missing name keeps IOSB.
    let cases = [
        (0x00, r"\??\C:\Game\PRESENT.TXT", 0),
        (0x80, r"\??\C:\Game\Present.Txt", 0),
        (0xc0, r"\??\C:\GAME\present.txt", 0),
        (0x00, r"\??\C:\Game\missing.txt", 0xc0000034),
        (0x80, r"\??\C:\Game\missing.txt", 0xc0000034),
    ];
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let mut code = Asm::new(CODE);
    for (index, (attributes, path, status)) in cases.iter().enumerate() {
        let base = DATA + index as u32 * 128;
        input(&mut m, base, SOURCE + index as u32 * 512, path);
        m.cpu.write_u32(base + 44, *attributes).unwrap();
        create(&mut m, &mut code, arguments(base), base + 16);
        if *status == 0 {
            code.mov_eax_mem(base)
                .push_eax()
                .call(close)
                .mov_mem_eax(base + 72);
        }
    }
    run(&mut m, code);
    for (index, (attributes, _, status)) in cases.into_iter().enumerate() {
        let base = DATA + index as u32 * 128;
        let label = format!("{attributes:#x} case {index}");
        assert_eq!(m.cpu.read_u32(base + 16).unwrap(), status, "{label}");
        assert_eq!(m.cpu.read_u32(base + 20).unwrap(), MARKER, "{label}");
        if status == 0 {
            assert_ne!(m.cpu.read_u32(base).unwrap(), 0, "{label}");
            assert_eq!(m.cpu.read_u32(base + 8).unwrap(), 0, "{label}");
            assert_eq!(m.cpu.read_u32(base + 12).unwrap(), 1, "{label}");
            assert_eq!(m.cpu.read_u32(base + 72).unwrap(), 1, "{label}");
        } else {
            assert_eq!(m.cpu.read_u32(base).unwrap(), 0, "{label}");
            assert_eq!(m.cpu.read_u32(base + 8).unwrap(), MARKER, "{label}");
            assert_eq!(m.cpu.read_u32(base + 12).unwrap(), INFORMATION, "{label}");
        }
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn default_case_and_open_if_attributes_match_native_file_opens() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        attributes(cpu);
    }
    attributes(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn native_handles_read_counted_unicode_names_and_share_the_win32_owner() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        lifecycle(cpu);
    }
    lifecycle(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn failures<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    // Ordinary argument/name results are compared with the native x86 oracle.
    // Additional cases protect unsupported behavior and guest memory ranges.
    let cases = [
        (
            "missing file",
            r"\??\C:\Game\missing.txt",
            0xc0000034,
            false,
        ),
        (
            "missing parent",
            r"\??\C:\Game\missing\file",
            0xc000003a,
            false,
        ),
        ("directory", r"\??\C:\Game\dir", 0xc00000ba, false),
        (
            "no synchronize",
            r"\??\C:\Game\present.txt",
            0xc000000d,
            false,
        ),
        (
            "bad disposition",
            r"\??\C:\Game\present.txt",
            0xc000000d,
            false,
        ),
        (
            "both synchronous",
            r"\??\C:\Game\present.txt",
            0xc000000d,
            false,
        ),
        ("bad share", r"\??\C:\Game\present.txt", 0xc000000d, false),
        (
            "bad OA length",
            r"\??\C:\Game\present.txt",
            0xc000000d,
            true,
        ),
        ("odd length", r"\??\C:\Game\present.txt", 0xc0000033, false),
        (
            "read-only output",
            r"\??\C:\Game\present.txt",
            0xc0000005,
            true,
        ),
        (
            "read-only IOSB",
            r"\??\C:\Game\present.txt",
            0xc0000005,
            true,
        ),
        (
            "source overrun",
            r"\??\C:\Game\present.txt",
            0xc0000005,
            false,
        ),
        (
            "object security",
            r"\??\C:\Game\present.txt",
            0xc00000bb,
            false,
        ),
        (
            "inheritable handle",
            r"\??\C:\Game\present.txt",
            0xc00000bb,
            false,
        ),
        ("supersede", r"\??\C:\Game\new.txt", 0xc00000bb, false),
        (
            "unsupported access",
            r"\??\C:\Game\present.txt",
            0xc00000bb,
            false,
        ),
        ("async open", r"\??\C:\Game\present.txt", 0xc00000bb, false),
        (
            "native namespace",
            r"\Device\HarddiskVolume1\file",
            0xc00000bb,
            false,
        ),
        (
            "stream",
            r"\??\C:\Game\present.txt:stream",
            0xc00000bb,
            false,
        ),
    ];
    let mut code = Asm::new(CODE);
    for (index, (label, path, _, _)) in cases.iter().enumerate() {
        let base = DATA + index as u32 * 128;
        input(&mut m, base, SOURCE + index as u32 * 512, path);
        let mut args = arguments(base);
        match *label {
            "no synchronize" => args[1] &= !0x100000,
            "bad disposition" => args[7] = 6,
            "both synchronous" => args[8] = 0x70,
            "bad share" => args[6] = 8,
            "bad OA length" => m.cpu.write_u32(base + 32, 20).unwrap(),
            "odd length" => {
                let v = m.cpu.read_u32(base + 64).unwrap();
                m.cpu.write_u32(base + 64, v + 1).unwrap();
            }
            "read-only output" => args[0] = READ_ONLY,
            "read-only IOSB" => args[3] = READ_ONLY,
            "source overrun" => m.cpu.write_u32(base + 68, SOURCE + 0xffff).unwrap(),
            "object security" => m.cpu.write_u32(base + 48, SOURCE).unwrap(),
            "inheritable handle" => m.cpu.write_u32(base + 44, 0x42).unwrap(),
            "supersede" => args[7] = 0,
            "unsupported access" => args[1] |= 0x20000000,
            "async open" => args[8] = 0x40,
            _ => {}
        }
        create(&mut m, &mut code, args, base + 16);
    }
    run(&mut m, code);
    for (index, (label, _, status, preserve_handle)) in cases.into_iter().enumerate() {
        let base = DATA + index as u32 * 128;
        assert_eq!(m.cpu.read_u32(base + 16).unwrap(), status, "{label}");
        assert_eq!(
            m.cpu.read_u32(base + 20).unwrap(),
            MARKER,
            "LastError: {label}"
        );
        assert_eq!(
            m.cpu.read_u32(base).unwrap(),
            if preserve_handle { MARKER } else { 0 },
            "FileHandle: {label}"
        );
        assert_eq!(
            m.cpu.read_u32(base + 8).unwrap(),
            MARKER,
            "IOSB.Status: {label}"
        );
        assert_eq!(
            m.cpu.read_u32(base + 12).unwrap(),
            INFORMATION,
            "IOSB.Information: {label}"
        );
    }
    let mut bytes = [0; 32];
    m.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 32]);
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    let diagnostics = m.ext.get::<ApiDiagnostics>();
    assert!(
        diagnostics
            .warnings
            .keys()
            .any(|(api, reason)| api.ends_with("ZwCreateFile")
                && reason.contains("OBJ_CASE_INSENSITIVE"))
    );
    assert!(!root.join("game/new.txt").exists());
    assert!(!root.join("data/overlay").exists());
}

#[test]
fn failures_preserve_native_iosb_last_error_and_prevent_filesystem_side_effects() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        failures(cpu);
    }
    failures(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn sharing<B: Backend>(cpu: B) {
    let (mut m, _) = fixture(cpu);
    let mut code = Asm::new(CODE);
    for index in 0..3u32 {
        input(
            &mut m,
            DATA + index * 128,
            SOURCE + index * 512,
            r"\??\C:\Game\present.txt",
        );
    }
    let mut args = arguments(DATA);
    args[6] = 0;
    create(&mut m, &mut code, args, DATA + 16);
    create(&mut m, &mut code, arguments(DATA + 128), DATA + 144);
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    code.mov_eax_mem(DATA).push_eax().call(close);
    // FILE_READ_DATA instead of GENERIC_READ selects the same read-only core.
    let mut args = arguments(DATA + 256);
    args[1] = 0x100001;
    create(&mut m, &mut code, args, DATA + 272);
    code.mov_eax_mem(DATA + 256).push_eax().call(close);
    run(&mut m, code);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 144).unwrap(), 0xc0000043);
    assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 136).unwrap(), MARKER);
    assert_eq!(m.cpu.read_u32(DATA + 272).unwrap(), 0);
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn incompatible_sharing_fails_until_the_shared_file_object_is_closed() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        sharing(cpu);
    }
    sharing(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn native_file_aliases_are_projected_with_only_their_exact_sdk_stack_contracts() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert_eq!(
        registry.get("ntdll.dll", "ZwCreateFile").unwrap().conv,
        Conv::Stdcall { argc: 11 }
    );
    assert_eq!(
        registry.level("ntdll.dll", "ZwCreateFile"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("ntdll.dll", "ZwCreateFile"));
    assert_eq!(
        registry
            .get("ntdll.dll", "ZwQueryInformationFile")
            .unwrap()
            .conv,
        Conv::Stdcall { argc: 5 }
    );
    assert_eq!(
        registry.level("ntdll.dll", "ZwQueryInformationFile"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("ntdll.dll", "ZwQueryInformationFile"));
    assert_eq!(
        registry
            .get("ntdll.dll", "ZwSetInformationFile")
            .unwrap()
            .conv,
        Conv::Stdcall { argc: 5 }
    );
    assert_eq!(
        registry.level("ntdll.dll", "ZwSetInformationFile"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("ntdll.dll", "ZwSetInformationFile"));
    assert_eq!(
        registry.get("ntdll.dll", "ZwReadFile").unwrap().conv,
        Conv::Stdcall { argc: 9 }
    );
    assert_eq!(
        registry.level("ntdll.dll", "ZwReadFile"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("ntdll.dll", "ZwReadFile"));
    assert_eq!(
        registry.get("ntdll.dll", "ZwWriteFile").unwrap().conv,
        Conv::Stdcall { argc: 9 }
    );
    assert_eq!(
        registry.level("ntdll.dll", "ZwWriteFile"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("ntdll.dll", "ZwWriteFile"));
    assert_eq!(
        registry.get("ntdll.dll", "ZwClose").unwrap().conv,
        Conv::Stdcall { argc: 1 }
    );
    assert_eq!(
        registry.level("ntdll.dll", "ZwClose"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("ntdll.dll", "ZwClose"));
    assert_eq!(
        registry.get("ntdll.dll", "NtClose").unwrap().conv,
        Conv::Stdcall { argc: 1 }
    );
    assert!(registry.dynamic_available("ntdll.dll", "NtClose"));
    for (name, argc) in [
        ("NtCreateFile", 11),
        ("NtReadFile", 9),
        ("NtWriteFile", 9),
        ("NtQueryInformationFile", 5),
        ("NtSetInformationFile", 5),
    ] {
        assert_eq!(
            registry.get("ntdll.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
        assert_eq!(
            registry.level("ntdll.dll", name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available("ntdll.dll", name));
    }
    assert!(registry.signature_conflicts().is_empty());
}

fn query<B: Backend>(m: &mut Machine<B>, code: &mut Asm, args: [u32; 5], result: u32) {
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let query = m
        .resolve_import("ntdll.dll", "ZwQueryInformationFile")
        .unwrap();
    code.push_imm(MARKER).call(set);
    for value in args.into_iter().rev() {
        code.push_imm(value);
    }
    code.call(query).mov_mem_eax(result);
    code.call(get).mov_mem_eax(result + 4);
}

fn query_position<B: Backend>(cpu: B) {
    let (mut m, _) = fixture(cpu);
    input(&mut m, DATA, SOURCE, r"\??\C:\Game\present.txt");
    let mut code = Asm::new(CODE);
    create(&mut m, &mut code, arguments(DATA), DATA + 16);
    run(&mut m, code);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    let handle = m.cpu.read_u32(DATA).unwrap();
    let alias = m.sys.objects.duplicate(handle).unwrap();
    // The shared synthetic-file owner is also seekable. A query does not
    // require FILE_READ_DATA access on native Windows, checked by the oracle.
    let memory = m.sys.objects.insert_host(FileObj {
        file: adv_os::vfs::FileStream::read_only(std::sync::Arc::from(*b"memory")),
        guest_path: r"C:\Windows\fixture.dll".into(),
        access: 0x80,
        share: 7,
    });
    let read = m.resolve_import("kernel32.dll", "ReadFile").unwrap();
    let seek = m
        .resolve_import("kernel32.dll", "SetFilePointerEx")
        .unwrap();
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let mut code = Asm::new(CODE);
    for index in 0..7u32 {
        m.cpu.write(DATA + 0x100 + index * 64, &[0xcd; 64]).unwrap();
    }
    for (index, queried) in [handle, handle, alias, handle, alias, memory, alias]
        .into_iter()
        .enumerate()
    {
        match index {
            1 => {
                code.push_imm(0)
                    .push_imm(DATA + 0x700)
                    .push_imm(5)
                    .push_imm(DATA + 0x710)
                    .push_imm(handle)
                    .call(read);
            }
            3 => {
                code.push_imm(0)
                    .push_imm(DATA + 0x730)
                    .push_imm(1)
                    .push_imm(3)
                    .push_imm(handle)
                    .call(seek);
            }
            5 => {
                code.push_imm(0)
                    .push_imm(DATA + 0x730)
                    .push_imm(1)
                    .push_imm(4)
                    .push_imm(memory)
                    .call(seek);
            }
            6 => {
                code.push_imm(handle).call(close);
            }
            _ => {}
        }
        let base = DATA + 0x100 + index as u32 * 64;
        query(
            &mut m,
            &mut code,
            [queried, base, base + 8, 12, 14],
            base + 24,
        );
    }
    code.push_imm(alias)
        .call(close)
        .push_imm(memory)
        .call(close);
    run(&mut m, code);
    for (index, expected) in [
        0u64,
        5,
        5,
        0x100000003,
        0x100000003,
        0x100000004,
        0x100000003,
    ]
    .into_iter()
    .enumerate()
    {
        let base = DATA + 0x100 + index as u32 * 64;
        assert_eq!(m.cpu.read_u32(base).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 4).unwrap(), 8);
        let mut position = [0; 8];
        m.cpu.read(base + 8, &mut position).unwrap();
        assert_eq!(u64::from_le_bytes(position), expected);
        assert_eq!(
            m.cpu.read_u32(base + 16).unwrap(),
            0xcdcdcdcd,
            "oversized buffer tail stays untouched"
        );
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 28).unwrap(), MARKER);
    }
    assert_eq!(
        m.sys.objects.hosts::<FileObj>().count(),
        0,
        "all shared aliases and memory streams released"
    );
}

#[test]
fn native_position_queries_follow_reads_seeks_duplicate_handles_and_virtual_streams() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        query_position(cpu);
    }
    query_position(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn query_failures<B: Backend>(cpu: B) {
    let (mut m, _) = fixture(cpu);
    let file = m.sys.objects.insert_host(FileObj {
        file: adv_os::vfs::FileStream::read_only(std::sync::Arc::from(*b"memory")),
        guest_path: r"C:\Game\fixture.dat".into(),
        access: 0x80000000,
        share: 7,
    });
    let other = m.sys.objects.insert_host(String::from("non-file object"));
    let closed = m.sys.objects.duplicate(file).unwrap();
    m.sys.objects.close(closed).unwrap();
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    let cases = [
        ("short", 0xc0000004),
        ("invalid", 0xc0000008),
        ("non-file", 0xc0000024),
        ("process", 0xc0000024),
        ("closed", 0xc0000008),
        ("class zero", 0xc0000002),
        ("other class", 0xc00000bb),
        ("null IOSB", 0xc0000005),
        ("null output", 0xc0000005),
        ("read-only IOSB", 0xc0000005),
        ("read-only output", 0xc0000005),
        ("address overflow", 0xc0000005),
        ("length overflow", 0xc0000005),
    ];
    let mut code = Asm::new(CODE);
    for (index, (label, _)) in cases.iter().enumerate() {
        let base = DATA + index as u32 * 64;
        m.cpu.write(base, &[0xcd; 64]).unwrap();
        let mut args = [file, base, base + 8, 8, 14];
        match *label {
            "short" => args[3] = 7,
            "invalid" => args[0] = 0x13572468,
            "non-file" => args[0] = other,
            "process" => args[0] = 0xffffffff,
            "closed" => args[0] = closed,
            "class zero" => args[4] = 0,
            "other class" => args[4] = 6,
            "null IOSB" => args[1] = 0,
            "null output" => args[2] = 0,
            "read-only IOSB" => args[1] = READ_ONLY,
            "read-only output" => args[2] = READ_ONLY,
            "address overflow" => args[2] = 0xfffffffc,
            "length overflow" => args[3] = 0xffffffff,
            _ => unreachable!(),
        }
        query(&mut m, &mut code, args, base + 24);
    }
    run(&mut m, code);
    for (index, (label, expected)) in cases.into_iter().enumerate() {
        let base = DATA + index as u32 * 64;
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), expected, "{label}");
        assert_eq!(
            m.cpu.read_u32(base + 28).unwrap(),
            MARKER,
            "LastError: {label}"
        );
        let mut bytes = [0; 24];
        m.cpu.read(base, &mut bytes).unwrap();
        assert_eq!(bytes, [0xcd; 24], "outputs: {label}");
    }
    let mut bytes = [0; 32];
    m.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 32]);
    assert_eq!(
        m.sys
            .objects
            .host::<FileObj>(file)
            .unwrap()
            .file
            .stream_position()
            .unwrap(),
        0
    );
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api.ends_with("ZwQueryInformationFile"))
    );
    m.sys.objects.close(file);
    m.sys.objects.close(other);
}

#[test]
fn failed_queries_preserve_all_outputs_last_error_and_the_shared_file_cursor() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        query_failures(cpu);
    }
    query_failures(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn query_standard<B: Backend>(cpu: B) {
    use adv_os::vfs::{FileStream, StandardInformation};
    use std::io::{Seek, SeekFrom};
    let (mut m, root) = fixture(cpu);
    let path = root.join("resources/present.txt");
    std::fs::hard_link(&path, root.join("linked.txt")).unwrap();
    let mut physical = FileStream::from(std::fs::File::open(&path).unwrap());
    physical.seek(SeekFrom::Start(5)).unwrap();
    let physical_expected = physical.standard_information().unwrap();
    let file = m.sys.objects.insert_host(FileObj {
        file: physical,
        guest_path: r"C:\Game\present.txt".into(),
        access: 0x80,
        share: 7,
    });
    let alias = m.sys.objects.duplicate(file).unwrap();
    let memory = m.sys.objects.insert_host(FileObj {
        file: FileStream::read_only(std::sync::Arc::from(*b"memory")),
        guest_path: r"C:\Windows\fixture.dll".into(),
        access: 0x80,
        share: 7,
    });
    let image = root.join("fixture.iso");
    std::fs::write(
        &image,
        include_bytes!("../../adv-os/tests/fixtures/optical.iso"),
    )
    .unwrap();
    let optical = {
        let vfs = &mut m.ext.get_mut::<Win32>().vfs;
        vfs.mount_iso('d', &image).unwrap();
        vfs.open_optical_file("D:/ReadMe.txt").unwrap() // privacy: allow authored guest optical path
    };
    let optical_length = optical.metadata().unwrap().len();
    let disc = m.sys.objects.insert_host(FileObj {
        file: optical,
        guest_path: "D:/ReadMe.txt".into(), // privacy: allow authored guest optical path
        access: 0x80,
        share: 7,
    });
    let virtual_expected = |length| StandardInformation {
        allocation_size: length,
        end_of_file: length,
        number_of_links: 1,
        delete_pending: false,
        directory: false,
    };
    let cases = [
        (file, physical_expected),
        (alias, physical_expected),
        (memory, virtual_expected(6)),
        (disc, virtual_expected(optical_length)),
    ];
    let mut code = Asm::new(CODE);
    for (index, (handle, _)) in cases.iter().enumerate() {
        let base = DATA + index as u32 * 64;
        m.cpu.write(base, &[0xcd; 64]).unwrap();
        query(
            &mut m,
            &mut code,
            [*handle, base, base + 8, 28, 5],
            base + 48,
        );
    }
    run(&mut m, code);
    for (index, (handle, expected)) in cases.into_iter().enumerate() {
        let base = DATA + index as u32 * 64;
        let mut information = [0; 24];
        m.cpu.read(base + 8, &mut information).unwrap();
        assert_eq!(
            u64::from_le_bytes(information[..8].try_into().unwrap()),
            expected.allocation_size
        );
        assert_eq!(
            u64::from_le_bytes(information[8..16].try_into().unwrap()),
            expected.end_of_file
        );
        assert_eq!(
            u32::from_le_bytes(information[16..20].try_into().unwrap()),
            expected.number_of_links
        );
        assert_eq!(information[20], u8::from(expected.delete_pending));
        assert_eq!(information[21], u8::from(expected.directory));
        assert_eq!(m.cpu.read_u32(base).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 4).unwrap(), 24);
        assert_eq!(m.cpu.read_u32(base + 32).unwrap(), 0xcdcdcdcd);
        assert_eq!(m.cpu.read_u32(base + 48).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(base + 52).unwrap(), MARKER);
        assert_eq!(
            m.sys
                .objects
                .host::<FileObj>(handle)
                .unwrap()
                .file
                .stream_position()
                .unwrap(),
            if index < 2 { 5 } else { 0 }
        );
        m.sys.objects.close(handle);
    }
}

#[test]
fn standard_queries_report_opened_instances_and_defined_virtual_metadata() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        query_standard(cpu);
    }
    query_standard(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn query_standard_failures<B: Backend>(cpu: B) {
    let (mut m, _) = fixture(cpu);
    let file = m.sys.objects.insert_host(FileObj {
        file: adv_os::vfs::FileStream::read_only(std::sync::Arc::from(*b"memory")),
        guest_path: r"C:\Windows\fixture.dll".into(),
        access: 0x80,
        share: 7,
    });
    let other = m.sys.objects.insert_host(String::from("non-file object"));
    let cases = [
        ("short invalid handle", 0xc0000004),
        ("invalid handle", 0xc0000008),
        ("wrong type", 0xc0000024),
        ("null IOSB", 0xc0000005),
        ("null output", 0xc0000005),
        ("readonly IOSB", 0xc0000005),
        ("readonly output", 0xc0000005),
        ("output tail overrun", 0xc0000005),
        ("length overflow", 0xc0000005),
    ];
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    m.cpu.write(SOURCE + 0xffe8, &[0x77; 24]).unwrap();
    let mut code = Asm::new(CODE);
    for (index, (label, _)) in cases.iter().enumerate() {
        let base = DATA + index as u32 * 64;
        m.cpu.write(base, &[0xcd; 64]).unwrap();
        let mut args = [file, base, base + 8, 24, 5];
        match *label {
            "short invalid handle" => {
                args[0] = 0x13572468;
                args[3] = 23;
            }
            "invalid handle" => args[0] = 0x13572468,
            "wrong type" => args[0] = other,
            "null IOSB" => args[1] = 0,
            "null output" => args[2] = 0,
            "readonly IOSB" => args[1] = READ_ONLY,
            "readonly output" => args[2] = READ_ONLY,
            "output tail overrun" => {
                args[2] = SOURCE + 0xffe8;
                args[3] = 28;
            }
            "length overflow" => args[3] = u32::MAX,
            _ => unreachable!(),
        }
        query(&mut m, &mut code, args, base + 48);
    }
    run(&mut m, code);
    for (index, (label, expected)) in cases.into_iter().enumerate() {
        let base = DATA + index as u32 * 64;
        assert_eq!(m.cpu.read_u32(base + 48).unwrap(), expected, "{label}");
        assert_eq!(m.cpu.read_u32(base + 52).unwrap(), MARKER, "{label}");
        let mut bytes = [0; 32];
        m.cpu.read(base, &mut bytes).unwrap();
        assert_eq!(bytes, [0xcd; 32], "{label}");
    }
    let mut bytes = [0; 32];
    m.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 32]);
    let mut tail = [0; 24];
    m.cpu.read(SOURCE + 0xffe8, &mut tail).unwrap();
    assert_eq!(tail, [0x77; 24]);
    assert_eq!(
        m.sys
            .objects
            .host::<FileObj>(file)
            .unwrap()
            .file
            .stream_position()
            .unwrap(),
        0
    );
    m.sys.objects.close(file);
    m.sys.objects.close(other);
}

#[test]
fn standard_query_failures_probe_complete_outputs_before_handle_lookup() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        query_standard_failures(cpu);
    }
    query_standard_failures(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn set_position<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    input(&mut m, DATA, SOURCE, r"\??\C:\Game\present.txt");
    let mut code = Asm::new(CODE);
    create(&mut m, &mut code, arguments(DATA), DATA + 16);
    run(&mut m, code);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    let handle = m.cpu.read_u32(DATA).unwrap();
    let alias = m.sys.objects.duplicate(handle).unwrap();
    let setter = m
        .resolve_import("ntdll.dll", "ZwSetInformationFile")
        .unwrap();
    let nt_setter = m
        .resolve_import("ntdll.dll", "NtSetInformationFile")
        .unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let cases = [
        ("actual zero", 0i64, 0u32, 0u64),
        ("duplicate 64-bit", 0x100000003, 0, 0x100000003),
        ("negative", -1, 0xc000000d, 0x100000003),
        ("short", 7, 0xc0000004, 0x100000003),
        ("null IOSB", 7, 0xc0000005, 0x100000003),
        ("input overrun", 7, 0xc0000005, 0x100000003),
        ("readonly input", 9, 0, 9),
        ("readonly IOSB", 7, 0xc0000005, 7),
        ("invalid handle", 0, 0xc0000008, 7),
    ];
    let mut readonly = [0x77; 32];
    readonly[..8].copy_from_slice(&9i64.to_le_bytes());
    m.cpu.write(READ_ONLY, &readonly).unwrap();
    let mut code = Asm::new(CODE);
    for (index, (label, value, _, _)) in cases.iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        m.cpu.write(base, &[0xcd; 128]).unwrap();
        m.cpu.write(base + 8, &value.to_le_bytes()).unwrap();
        let mut args = [handle, base, base + 8, 8, 14];
        match *label {
            "duplicate 64-bit" => {
                args[0] = alias;
                args[3] = 12;
            }
            "short" => args[3] = 7,
            "null IOSB" => args[1] = 0,
            "input overrun" => args[2] = SOURCE + 0xffff,
            "readonly input" => args[2] = READ_ONLY,
            "readonly IOSB" => args[1] = READ_ONLY + 16,
            "invalid handle" => args[0] = 0x13572468,
            _ => {}
        }
        code.push_imm(MARKER).call(set_error);
        for value in args.into_iter().rev() {
            code.push_imm(value);
        }
        code.call(if index % 2 == 0 { nt_setter } else { setter })
            .mov_mem_eax(base + 24);
        code.call(get_error).mov_mem_eax(base + 28);
        // Query through the original alias observes the setter's shared seek,
        // including native completion failure after a mapped readonly IOSB.
        query(
            &mut m,
            &mut code,
            [handle, base + 40, base + 48, 8, 14],
            base + 32,
        );
    }
    code.push_imm(alias)
        .call(close)
        .push_imm(handle)
        .call(close);
    run(&mut m, code);
    for (index, (label, value, status, position)) in cases.into_iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), status, "{label}");
        assert_eq!(
            m.cpu.read_u32(base + 28).unwrap(),
            MARKER,
            "setter LastError: {label}"
        );
        let mut bytes = [0; 8];
        m.cpu.read(base + 8, &mut bytes).unwrap();
        assert_eq!(bytes, value.to_le_bytes(), "input unchanged: {label}");
        m.cpu.read(base, &mut bytes).unwrap();
        assert_eq!(
            bytes,
            if status == 0 { [0; 8] } else { [0xcd; 8] },
            "IOSB: {label}"
        );
        assert_eq!(
            m.cpu.read_u32(base + 32).unwrap(),
            0,
            "position query: {label}"
        );
        m.cpu.read(base + 48, &mut bytes).unwrap();
        assert_eq!(
            u64::from_le_bytes(bytes),
            position,
            "shared cursor: {label}"
        );
    }
    let mut bytes = [0; 32];
    m.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(
        bytes, readonly,
        "neither input nor readonly completion storage is written"
    );
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    assert_eq!(
        std::fs::read(root.join("resources/present.txt")).unwrap(),
        b"replacement!"
    );
    assert!(
        !root.join("data/overlay").exists(),
        "seeking never creates or extends files"
    );
}

#[test]
fn position_setter_shares_alias_cursor_and_preserves_native_failure_ordering() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        set_position(cpu);
    }
    set_position(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn read_call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, args: [u32; 9], result: u32) {
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let read = m.resolve_import("ntdll.dll", "ZwReadFile").unwrap();
    code.push_imm(MARKER).call(set);
    for value in args.into_iter().rev() {
        code.push_imm(value);
    }
    code.call(read).mov_mem_eax(result);
    code.call(get).mov_mem_eax(result + 4);
}

fn read_positions<B: Backend>(cpu: B) {
    let (mut m, root) = fixture(cpu);
    std::fs::write(root.join("game/nt-read.txt"), b"abcdefgh").unwrap();
    input(&mut m, DATA, SOURCE, r"\??\C:\Game\nt-read.txt");
    let mut code = Asm::new(CODE);
    create(&mut m, &mut code, arguments(DATA), DATA + 16);
    run(&mut m, code);
    let handle = m.cpu.read_u32(DATA).unwrap();
    let alias = m.sys.objects.duplicate(handle).unwrap();
    let seek = m
        .resolve_import("kernel32.dll", "SetFilePointerEx")
        .unwrap();
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let cases = [
        (5u32, None, b"abcde".as_slice(), 0u32, 5u64),
        (3, Some(2i64), b"cde".as_slice(), 0, 5),
        (8, Some(7), b"h".as_slice(), 0, 8),
        (1, None, b"".as_slice(), 0xc0000011, 8),
        (3, Some(20), b"".as_slice(), 0xc0000011, 8),
        (0, None, b"".as_slice(), 0, 8),
        (0, Some(2), b"".as_slice(), 0, 8),
        (2, Some(-2), b"".as_slice(), 0xc0000011, 8),
        (2, Some(-2), b"cd".as_slice(), 0, 4),
    ];
    let mut code = Asm::new(CODE);
    for (index, (length, offset, _, _, _)) in cases.iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        m.cpu.write(base, &[0xcd; 128]).unwrap();
        if index == 8 {
            code.push_imm(0)
                .push_imm(DATA + 0xe00)
                .push_imm(0)
                .push_imm(2)
                .push_imm(alias)
                .call(seek);
        }
        let offset_pointer = if let Some(value) = offset {
            m.cpu.write(base + 32, &value.to_le_bytes()).unwrap();
            base + 32
        } else {
            0
        };
        read_call(
            &mut m,
            &mut code,
            [
                alias,
                0,
                0,
                0,
                base,
                if *length == 0 { 0 } else { base + 8 },
                *length,
                offset_pointer,
                0,
            ],
            base + 24,
        );
        query(
            &mut m,
            &mut code,
            [handle, base + 40, base + 48, 8, 14],
            base + 56,
        );
    }
    code.push_imm(alias)
        .call(close)
        .push_imm(handle)
        .call(close);
    run(&mut m, code);
    for (index, (_, _, expected, status, position)) in cases.into_iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), status);
        assert_eq!(m.cpu.read_u32(base + 28).unwrap(), MARKER);
        assert_eq!(m.cpu.read_u32(base).unwrap(), status);
        assert_eq!(m.cpu.read_u32(base + 4).unwrap(), expected.len() as u32);
        let mut bytes = [0xcd; 16];
        m.cpu.read(base + 8, &mut bytes).unwrap();
        assert_eq!(&bytes[..expected.len()], expected);
        assert!(bytes[expected.len()..].iter().all(|byte| *byte == 0xcd));
        let mut cursor = [0; 8];
        m.cpu.read(base + 48, &mut cursor).unwrap();
        assert_eq!(u64::from_le_bytes(cursor), position);
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    assert_eq!(
        std::fs::read(root.join("game/nt-read.txt")).unwrap(),
        b"abcdefgh"
    );
}

#[test]
fn synchronous_reads_use_shared_cursor_and_match_native_explicit_eof_and_zero_rules() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        read_positions(cpu);
    }
    read_positions(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn read_coherence_and_guards<B: Backend>(cpu: B) {
    const VIEW: u32 = 0x450000;
    let (mut m, root) = fixture(cpu);
    std::fs::write(root.join("game/nt-read.txt"), b"abcdefgh").unwrap();
    input(&mut m, DATA, SOURCE, r"\??\C:\Game\nt-read.txt");
    let mut code = Asm::new(CODE);
    create(&mut m, &mut code, arguments(DATA), DATA + 16);
    run(&mut m, code);
    let handle = m.cpu.read_u32(DATA).unwrap();
    let alias = m.sys.objects.duplicate(handle).unwrap();
    // A real backend file page is shared with this file identity, and is dirty
    // before it is flushed to disk. Raw FileStream reads still return abcdefgh.
    let backing = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("game/nt-read.txt"))
        .unwrap();
    let section = m
        .ext
        .get::<Win32>()
        .vfs
        .sections
        .file(backing, 8, true)
        .unwrap();
    let page = section.page(0);
    m.cpu
        .map_file_pages(VIEW, &[page.clone()], Prot::RW, false)
        .unwrap();
    m.cpu.write(READ_ONLY, &[0x77; 32]).unwrap();
    let seek = m
        .resolve_import("kernel32.dll", "SetFilePointerEx")
        .unwrap();
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    let mut code = Asm::new(CODE);
    code.mov_mem_imm(VIEW, 0x34333231);
    let failures = [
        ("readonly IOSB", 0xc0000005),
        ("readonly buffer", 0xc0000005),
        ("negative", 0xc000000d),
        ("invalid", 0xc0000008),
        ("range", 0xc0000005),
        ("event", 0xc00000bb),
        ("APC", 0xc00000bb),
    ];
    for (index, (label, _)) in failures.iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        m.cpu.write(base, &[0xcd; 128]).unwrap();
        let mut args = [alias, 0, 0, 0, base, base + 8, 2, 0, 0];
        match *label {
            "readonly IOSB" => args[4] = READ_ONLY,
            "readonly buffer" => args[5] = READ_ONLY,
            "negative" => {
                args[7] = base + 32;
                m.cpu.write(base + 32, &(-1i64).to_le_bytes()).unwrap();
            }
            "invalid" => args[0] = 0x13572468,
            "range" => args[5] = SOURCE + 0xffff,
            "event" => args[1] = 1,
            "APC" => args[2] = CODE,
            _ => unreachable!(),
        }
        read_call(&mut m, &mut code, args, base + 24);
        query(
            &mut m,
            &mut code,
            [handle, base + 40, base + 48, 8, 14],
            base + 56,
        );
    }
    let nt = DATA + 0x900;
    m.cpu.write(nt, &[0xcd; 128]).unwrap();
    read_call(
        &mut m,
        &mut code,
        [alias, 0, 0, 0, nt, nt + 8, 4, 0, 0],
        nt + 24,
    );
    query(
        &mut m,
        &mut code,
        [handle, nt + 40, nt + 48, 8, 14],
        nt + 56,
    );
    code.push_imm(0)
        .push_imm(DATA + 0xf00)
        .push_imm(0)
        .push_imm(0)
        .push_imm(handle)
        .call(seek);
    let ordinary = m.resolve_import("kernel32.dll", "ReadFile").unwrap();
    code.push_imm(0)
        .push_imm(DATA + 0xa10)
        .push_imm(4)
        .push_imm(DATA + 0xa00)
        .push_imm(handle)
        .call(ordinary)
        .mov_mem_eax(DATA + 0xa14);
    code.push_imm(alias)
        .call(close)
        .push_imm(handle)
        .call(close);
    run(&mut m, code);
    for (index, (_, status)) in failures.into_iter().enumerate() {
        let base = DATA + 0x100 + index as u32 * 128;
        assert_eq!(m.cpu.read_u32(base + 24).unwrap(), status);
        assert_eq!(m.cpu.read_u32(base + 28).unwrap(), MARKER);
        let mut bytes = [0; 24];
        m.cpu.read(base, &mut bytes).unwrap();
        assert_eq!(bytes, [0xcd; 24]);
        m.cpu.read(base + 48, &mut bytes[..8]).unwrap();
        assert_eq!(&bytes[..8], &[0; 8]);
    }
    assert_eq!(m.cpu.read_u32(nt + 24).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(nt + 4).unwrap(), 4);
    assert_eq!(m.cpu.read_u32(nt + 8).unwrap(), 0x34333231);
    assert_eq!(m.cpu.read_u32(nt + 48).unwrap(), 4);
    assert_eq!(m.cpu.read_u32(DATA + 0xa14).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(DATA + 0xa00).unwrap(), 0x34333231);
    assert_eq!(
        std::fs::read(root.join("game/nt-read.txt")).unwrap(),
        b"abcdefgh",
        "dirty mapping has not been flushed to disk"
    );
    assert!(page.dirty());
    let mut bytes = [0; 32];
    m.cpu.read(READ_ONLY, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 32]);
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api.ends_with("ZwReadFile"))
    );
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
}

#[test]
fn native_read_guards_preserve_cursor_and_reads_merge_actual_dirty_mapped_content() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        read_coherence_and_guards(cpu);
    }
    read_coherence_and_guards(UnicornCpu::new(PARK_ADDR).unwrap());
}

fn close_call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, handle: u32, result: u32) {
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let close = m.resolve_import("ntdll.dll", "ZwClose").unwrap();
    code.push_imm(MARKER)
        .call(set)
        .push_imm(handle)
        .call(close)
        .mov_mem_eax(result);
    code.call(get).mov_mem_eax(result + 4);
}

fn close_lifecycle<B: Backend>(cpu: B) {
    let (mut m, _) = fixture(cpu);
    input(&mut m, DATA, SOURCE, r"\??\C:\Game\present.txt");
    m.cpu.write(DATA + 0x1000, b"Close-mapping\0").unwrap();
    let mapping = m
        .resolve_import("kernel32.dll", "CreateFileMappingA")
        .unwrap();
    let map = m.resolve_import("kernel32.dll", "MapViewOfFile").unwrap();
    let event = m.resolve_import("kernel32.dll", "CreateEventA").unwrap();
    let mut code = Asm::new(CODE);
    create(&mut m, &mut code, arguments(DATA), DATA + 16);
    code.push_imm(DATA + 0x1000)
        .push_imm(0)
        .push_imm(0)
        .push_imm(2)
        .push_imm(0)
        .mov_eax_mem(DATA)
        .push_eax()
        .call(mapping)
        .mov_mem_eax(DATA + 0x80);
    code.push_imm(8)
        .push_imm(0)
        .push_imm(0)
        .push_imm(4)
        .mov_eax_mem(DATA + 0x80)
        .push_eax()
        .call(map)
        .mov_mem_eax(DATA + 0x84);
    code.push_imm(0)
        .push_imm(0)
        .push_imm(0)
        .push_imm(0)
        .call(event)
        .mov_mem_eax(DATA + 0x88);
    run(&mut m, code);
    let file = m.cpu.read_u32(DATA).unwrap();
    let alias = m.sys.objects.duplicate(file).unwrap();
    let section = m.cpu.read_u32(DATA + 0x80).unwrap();
    let section_alias = m.sys.objects.duplicate(section).unwrap();
    let view = m.cpu.read_u32(DATA + 0x84).unwrap();
    let event = m.cpu.read_u32(DATA + 0x88).unwrap();
    assert_ne!(section, 0);
    assert_ne!(view, 0);
    assert_ne!(event, 0);
    let mut code = Asm::new(CODE);
    close_call(&mut m, &mut code, file, DATA + 0x100);
    read_call(
        &mut m,
        &mut code,
        [alias, 0, 0, 0, DATA + 0x200, DATA + 0x208, 2, 0, 0],
        DATA + 0x220,
    );
    query(
        &mut m,
        &mut code,
        [alias, DATA + 0x228, DATA + 0x230, 8, 14],
        DATA + 0x238,
    );
    close_call(&mut m, &mut code, alias, DATA + 0x108);
    close_call(&mut m, &mut code, section, DATA + 0x110);
    // The duplicated section handle still owns its backing; views stay valid.
    code.mov_eax_mem(view).mov_mem_eax(DATA + 0x240);
    close_call(&mut m, &mut code, section_alias, DATA + 0x118);
    code.mov_eax_mem(view).mov_mem_eax(DATA + 0x244);
    close_call(&mut m, &mut code, event, DATA + 0x120);
    for (index, handle) in [alias, 0, 0x13572468, 3, 7, 11, 0xffffffff, 0xfffffffe]
        .into_iter()
        .enumerate()
    {
        close_call(&mut m, &mut code, handle, DATA + 0x128 + index as u32 * 8);
    }
    run(&mut m, code);
    for index in 0..5u32 {
        assert_eq!(m.cpu.read_u32(DATA + 0x100 + index * 8).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + 0x104 + index * 8).unwrap(), MARKER);
    }
    for index in 0..8u32 {
        assert_eq!(
            m.cpu.read_u32(DATA + 0x128 + index * 8).unwrap(),
            if index < 6 { 0xc0000008 } else { 0 }
        );
        assert_eq!(m.cpu.read_u32(DATA + 0x12c + index * 8).unwrap(), MARKER);
    }
    assert_eq!(m.cpu.read_u32(DATA + 0x220).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 0x204).unwrap(), 2);
    let mut bytes = [0; 2];
    m.cpu.read(DATA + 0x208, &mut bytes).unwrap();
    assert_eq!(&bytes, b"re");
    assert_eq!(
        m.cpu.read_u32(DATA + 0x230).unwrap(),
        2,
        "the surviving alias kept the shared stream cursor"
    );
    assert_eq!(
        m.cpu.read_u32(DATA + 0x240).unwrap(),
        u32::from_le_bytes(*b"repl")
    );
    assert_eq!(
        m.cpu.read_u32(DATA + 0x244).unwrap(),
        u32::from_le_bytes(*b"repl")
    );
    for handle in [file, alias, section, section_alias, event] {
        assert!(m.sys.objects.get(handle).is_none());
    }
    assert_eq!(m.sys.objects.hosts::<FileObj>().count(), 0);
    assert!(
        m.cpu.accessible(view, 8, Prot::READ),
        "closing a section never unmaps a retained view"
    );
    // Pruned names can be opened only while section handles exist. Unmapping
    // the final view releases its owner and guest VM allocation independently.
    let open = m
        .resolve_import("kernel32.dll", "OpenFileMappingA")
        .unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let unmap = m.resolve_import("kernel32.dll", "UnmapViewOfFile").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(DATA + 0x1000)
        .push_imm(0)
        .push_imm(4)
        .call(open)
        .mov_mem_eax(DATA + 0x280);
    code.call(get)
        .mov_mem_eax(DATA + 0x284)
        .push_imm(view)
        .call(unmap)
        .mov_mem_eax(DATA + 0x288);
    run(&mut m, code);
    assert_eq!(m.cpu.read_u32(DATA + 0x280).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 0x284).unwrap(), 2);
    assert_eq!(m.cpu.read_u32(DATA + 0x288).unwrap(), 1);
    assert!(!m.cpu.accessible(view, 8, Prot::READ));
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
fn nt_close_releases_real_aliases_preserves_last_error_and_retains_mapping_views() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        close_lifecycle(cpu);
    }
    close_lifecycle(UnicornCpu::new(PARK_ADDR).unwrap());
}
