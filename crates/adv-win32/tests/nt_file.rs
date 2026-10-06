//! Authored native FILE_OPEN regressions; the x86 oracle lives under target.
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
            "no case-insensitive flag",
            r"\??\C:\Game\present.txt",
            0xc00000bb,
            false,
        ),
        ("create file", r"\??\C:\Game\new.txt", 0xc00000bb, false),
        (
            "write access",
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
            "no case-insensitive flag" => m.cpu.write_u32(base + 44, 0).unwrap(),
            "create file" => args[7] = 2,
            "write access" => args[1] |= 0x40000000,
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
fn only_the_actual_called_nt_alias_is_projected_with_its_sdk_stack_contract() {
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
    for name in ["NtCreateFile", "ZwReadFile", "ZwClose"] {
        assert!(!registry.dynamic_available("ntdll.dll", name));
    }
    assert!(registry.signature_conflicts().is_empty());
}
