//! Authored x86 calls cover Unicode version resources and SDK-only VCM failures.
use adv_abi::{ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUTPUT: u32 = DATA + 0x2000;

#[test]
fn nt_process_debug_queries_match_the_virtual_process() {
    let mut m = machine(false, "nt-debug-query");
    m.cpu.write_u32(DATA, 0x12345678).unwrap();
    let mut code = Asm::new(CODE);
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtQueryInformationProcess",
        &[u32::MAX, 30, DATA, 4, DATA + 16],
        0,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtQueryInformationProcess",
        &[u32::MAX, 7, DATA + 4, 4, 0],
        4,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "ZwQueryInformationProcess",
        &[u32::MAX, 31, DATA + 8, 4, 0],
        8,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtQueryInformationProcess",
        &[u32::MAX, 7, DATA + 4, 3, 0],
        12,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtQueryInformationProcess",
        &[0, 7, DATA + 4, 4, 0],
        16,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtQueryInformationProcess",
        &[u32::MAX, 7, DATA + 0x4000, 4, 0],
        20,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtSetInformationThread",
        &[u32::MAX - 1, 17, 0, 0],
        24,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtSetInformationThread",
        &[0, 17, 0, 0],
        28,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtQuerySystemInformation",
        &[35, DATA + 24, 2, DATA + 28],
        32,
    );
    call(
        &mut m,
        &mut code,
        "ntdll.dll",
        "NtQuerySystemInformation",
        &[35, DATA + 24, 1, DATA + 32],
        36,
    );
    run(&mut m, code);
    for (offset, expected) in [
        (0, 0x12345678),
        (4, 0),
        (8, 1),
        (16, 4),
        (24, 256),
        (28, 2),
        (32, 2),
    ] {
        assert_eq!(m.cpu.read_u32(DATA + offset).unwrap(), expected);
    }
    for (offset, expected) in [
        (0, 0xc0000353),
        (4, 0),
        (8, 0),
        (12, 0xc0000004),
        (16, 0xc0000008),
        (20, 0xc0000005),
        (24, 0),
        (28, 0xc0000008),
        (32, 0),
        (36, 0xc0000004),
    ] {
        assert_eq!(m.cpu.read_u32(OUTPUT + offset).unwrap(), expected);
    }
}

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}
fn block(key: &str, text: bool, value: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = vec![0; 6];
    bytes.extend(wide(key));
    bytes.resize(bytes.len().next_multiple_of(4), 0);
    bytes.extend(value);
    for child in children {
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        bytes.extend(child);
    }
    let length = bytes.len() as u16;
    put16(&mut bytes, 0, length);
    put16(
        &mut bytes,
        2,
        if text {
            value.len() as u16 / 2
        } else {
            value.len() as u16
        },
    );
    put16(&mut bytes, 4, u16::from(text));
    bytes
}
fn resource() -> Vec<u8> {
    let name = block("ProductName", true, &wide("向日葵🌻"), &[]);
    let strings = block(
        "StringFileInfo",
        true,
        &[],
        &[block("041104b0", true, &[], &[name])],
    );
    let translation = block(
        "VarFileInfo",
        true,
        &[],
        &[block("Translation", false, &[0x11, 4, 0xb0, 4], &[])],
    );
    let mut fixed = vec![0; 52];
    put32(&mut fixed, 0, 0xfeef04bd);
    block("VS_VERSION_INFO", false, &fixed, &[strings, translation])
}
fn pe(raw: &[u8]) -> Vec<u8> {
    let mut file = vec![0; 0x600];
    file[..2].copy_from_slice(b"MZ");
    put32(&mut file, 0x3c, 0x80);
    file[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut file, 0x84, 0x14c);
    put16(&mut file, 0x86, 1);
    put16(&mut file, 0x94, 224);
    let o = 0x98;
    put16(&mut file, o, 0x10b);
    put32(&mut file, o + 28, CODE);
    put32(&mut file, o + 32, 0x1000);
    put32(&mut file, o + 36, 0x200);
    put32(&mut file, o + 56, 0x2000);
    put32(&mut file, o + 60, 0x200);
    put32(&mut file, o + 92, 16);
    put32(&mut file, o + 112, 0x1000);
    put32(&mut file, o + 116, 0x400);
    let s = o + 224;
    file[s..s + 5].copy_from_slice(b".rsrc");
    put32(&mut file, s + 8, 0x400);
    put32(&mut file, s + 12, 0x1000);
    put32(&mut file, s + 16, 0x400);
    put32(&mut file, s + 20, 0x200);
    put32(&mut file, s + 36, 0x40000040);
    for (d, id, next) in [(0, 16, 0x80000020), (0x20, 1, 0x80000040), (0x40, 0, 0x60)] {
        put16(&mut file, 0x200 + d + 14, 1);
        put32(&mut file, 0x200 + d + 16, id);
        put32(&mut file, 0x200 + d + 20, next);
    }
    put32(&mut file, 0x260, 0x1080);
    put32(&mut file, 0x264, raw.len() as u32);
    file[0x280..0x280 + raw.len()].copy_from_slice(raw);
    file
}
fn machine(jit: bool, label: &str) -> Machine<JitCpu> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/bootstrap-core-tests")
        .join(format!("{}-{label}-{jit}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("版本.dll"), pe(&resource())).unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::compat::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.level("msvfw32.dll", "ICInstall"),
        Some(ImplementationLevel::SafeFail)
    );
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(root.canonicalize().unwrap(), "probe.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    machine.cpu.map(DATA + 0x4000, 4096, Prot::READ).unwrap();
    machine
}
fn call(m: &mut Machine<JitCpu>, code: &mut Asm, dll: &str, name: &str, args: &[u32], slot: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(m.resolve_import(dll, name).unwrap())
        .mov_mem_eax(OUTPUT + slot);
}
fn run(m: &mut Machine<JitCpu>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("bootstrap core", CODE, &[], 0x10000).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

#[test]
fn unicode_file_version_queries_return_original_utf16_and_binary_values() {
    for jit in [false, true] {
        let mut m = machine(jit, "version");
        let raw = resource();
        m.cpu.write(DATA, &wide("版本.dll")).unwrap();
        for (at, path) in [
            (DATA + 256, "\\"),
            (DATA + 512, "\\StringFileInfo\\041104b0\\ProductName"),
            (DATA + 768, "\\VarFileInfo\\Translation"),
            (DATA + 1024, "\\Absent"),
        ] {
            m.cpu.write(at, &wide(path)).unwrap();
        }
        let mut code = Asm::new(CODE);
        call(
            &mut m,
            &mut code,
            "version.dll",
            "GetFileVersionInfoSizeW",
            &[DATA, DATA + 1200],
            0,
        );
        call(
            &mut m,
            &mut code,
            "version.dll",
            "GetFileVersionInfoW",
            &[DATA, 0, 2048, DATA + 0x1000],
            4,
        );
        for (index, path) in [DATA + 256, DATA + 512, DATA + 768, DATA + 1024]
            .into_iter()
            .enumerate()
        {
            call(
                &mut m,
                &mut code,
                "version.dll",
                "VerQueryValueW",
                &[
                    DATA + 0x1000,
                    path,
                    OUTPUT + 128 + index as u32 * 8,
                    OUTPUT + 132 + index as u32 * 8,
                ],
                8 + index as u32 * 4,
            );
        }
        run(&mut m, code);
        assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), raw.len() as u32 * 2 + 4);
        for slot in [4, 8, 12, 16] {
            assert_eq!(m.cpu.read_u32(OUTPUT + slot).unwrap(), 1);
        }
        assert_eq!(m.cpu.read_u32(OUTPUT + 20).unwrap(), 0);
        let fixed = m.cpu.read_u32(OUTPUT + 128).unwrap();
        assert_eq!(m.cpu.read_u32(fixed).unwrap(), 0xfeef04bd);
        assert_eq!(m.cpu.read_u32(OUTPUT + 132).unwrap(), 52);
        let text = m.cpu.read_u32(OUTPUT + 136).unwrap();
        let expected = wide("向日葵🌻");
        let mut actual = vec![0; expected.len()];
        m.cpu.read(text, &mut actual).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(
            m.cpu.read_u32(OUTPUT + 140).unwrap(),
            expected.len() as u32 / 2
        );
        let translation = m.cpu.read_u32(OUTPUT + 144).unwrap();
        assert_eq!(m.cpu.read_u32(translation).unwrap(), 0x04b00411);
        assert_eq!(m.cpu.read_u32(OUTPUT + 148).unwrap(), 4);
    }
}

#[test]
fn malformed_version_outputs_and_vcm_installation_have_explicit_failures() {
    for jit in [false, true] {
        let mut m = machine(jit, "failures");
        m.cpu.write(DATA, &wide("\\")).unwrap();
        m.cpu.write_u32(DATA + 128, 0x12345678).unwrap();
        m.cpu.write(DATA + 256, &[0xff, 0xff, 0, 0, 0, 0]).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut m,
            &mut code,
            "version.dll",
            "VerQueryValueW",
            &[DATA + 256, DATA, DATA + 128, DATA + 0x4000],
            0,
        );
        call(&mut m, &mut code, "kernel32.dll", "GetLastError", &[], 4);
        call(
            &mut m,
            &mut code,
            "version.dll",
            "VerQueryValueW",
            &[DATA + 256, DATA, OUTPUT + 128, OUTPUT + 132],
            8,
        );
        call(
            &mut m,
            &mut code,
            "msvfw32.dll",
            "ICInstall",
            &[0x63646976, 0x74736574, CODE, 0, 1],
            12,
        );
        call(&mut m, &mut code, "kernel32.dll", "GetLastError", &[], 16);
        call(
            &mut m,
            &mut code,
            "msvfw32.dll",
            "ICRemove",
            &[0x63646976, 0x74736574, 0],
            20,
        );
        call(&mut m, &mut code, "kernel32.dll", "GetLastError", &[], 24);
        call(
            &mut m,
            &mut code,
            "ole32.dll",
            "CoFreeUnusedLibraries",
            &[],
            28,
        );
        run(&mut m, code);
        for slot in [0, 8, 12, 20] {
            assert_eq!(m.cpu.read_u32(OUTPUT + slot).unwrap(), 0);
        }
        assert_eq!(m.cpu.read_u32(OUTPUT + 4).unwrap(), 998);
        assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0x12345678);
        assert_eq!(m.cpu.read_u32(OUTPUT + 16).unwrap(), 120);
        assert_eq!(m.cpu.read_u32(OUTPUT + 24).unwrap(), 120);
    }
}

#[test]
fn debugger_query_reports_the_guest_process_and_validates_outputs() {
    for jit in [false, true] {
        let mut m = machine(jit, "debugger-query");
        m.cpu.write_u32(DATA, 0xdeadbeef).unwrap();
        m.cpu.write_u32(DATA + 4, 0x12345678).unwrap();
        let mut code = Asm::new(CODE);
        call(
            &mut m,
            &mut code,
            "kernel32.dll",
            "SetLastError",
            &[0x55],
            0,
        );
        call(
            &mut m,
            &mut code,
            "kernel32.dll",
            "CheckRemoteDebuggerPresent",
            &[u32::MAX, DATA],
            4,
        );
        call(&mut m, &mut code, "kernel32.dll", "GetLastError", &[], 8);
        call(
            &mut m,
            &mut code,
            "kernel32.dll",
            "CheckRemoteDebuggerPresent",
            &[0, DATA + 4],
            12,
        );
        call(&mut m, &mut code, "kernel32.dll", "GetLastError", &[], 16);
        call(
            &mut m,
            &mut code,
            "kernel32.dll",
            "CheckRemoteDebuggerPresent",
            &[u32::MAX, 0],
            20,
        );
        call(&mut m, &mut code, "kernel32.dll", "GetLastError", &[], 24);
        call(
            &mut m,
            &mut code,
            "kernel32.dll",
            "CheckRemoteDebuggerPresent",
            &[u32::MAX, DATA + 0x4000],
            28,
        );
        call(&mut m, &mut code, "kernel32.dll", "GetLastError", &[], 32);
        run(&mut m, code);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0x12345678);
        for (offset, expected) in [
            (4, 1),
            (8, 0x55),
            (12, 0),
            (16, 6),
            (20, 0),
            (24, 87),
            (28, 0),
            (32, 998),
        ] {
            assert_eq!(m.cpu.read_u32(OUTPUT + offset).unwrap(), expected);
        }
    }
}
