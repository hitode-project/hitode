use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::sync::atomic::{AtomicUsize, Ordering};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUTPUT: u32 = DATA + 0x1000;
const LENGTH: u32 = DATA + 0x1200;
const HASH: u32 = DATA + 0x1300;
const BAD_HASH: u32 = DATA + 0x1340;
const READ_ONLY: u32 = 0x430000;
const MARKER: u32 = 0x12345678;
fn profile<B: Backend>(cpu: B) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "hitode-msi-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("資材.bin"), b"abc").unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(&directory, "fixture.exe", directory.join("data")),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write(READ_ONLY, &[0x5a; 20]).unwrap();
    m.cpu.write(OUTPUT, &[0xa5; 512]).unwrap();
    m.cpu.write_u32(LENGTH, 64).unwrap();
    m.cpu.write(HASH, &[0xcd; 20]).unwrap();
    m.cpu.write_u32(HASH, 20).unwrap();
    m.cpu.write(BAD_HASH, &[0xcd; 20]).unwrap();
    m.cpu.write_u32(BAD_HASH, 16).unwrap();
    for (index, wide) in [false, true].into_iter().enumerate() {
        for (offset, text) in [
            (0x800, "{01234567-89AB-CDEF-0123-456789ABCDEF}"),
            (0x900, "invalid-guid"),
            (0xa00, "ProductName"),
            (0xb00, "C:\\Game\\資材.bin"),
            (0xc00, "C:\\Game\\missing.bin"),
        ] {
            let bytes = if wide {
                text.encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
            } else {
                let mut bytes = encoding_rs::SHIFT_JIS.encode(text).0.into_owned();
                bytes.push(0);
                bytes
            };
            m.cpu
                .write(DATA + offset + index as u32 * 0x80, &bytes)
                .unwrap();
        }
    }
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    let mut call = |name: &str, args: &[u32], value| {
        code.push_imm(MARKER).call(set_error);
        for arg in args.iter().rev() {
            code.push_imm(*arg);
        }
        let function = m.resolve_import("msi.dll", name).unwrap();
        let row = DATA + expected.len() as u32 * 8;
        code.call(function)
            .mov_mem_eax(row)
            .call(get_error)
            .mov_mem_eax(row + 4);
        expected.push((name.to_owned(), value));
    };
    for (index, suffix) in ["A", "W"].into_iter().enumerate() {
        let guid = DATA + 0x800 + index as u32 * 0x80;
        let bad = DATA + 0x900 + index as u32 * 0x80;
        let feature = DATA + 0xa00 + index as u32 * 0x80;
        let path = DATA + 0xb00 + index as u32 * 0x80;
        let missing = DATA + 0xc00 + index as u32 * 0x80;
        let name = |base: &str| format!("{base}{suffix}");
        call(&name("MsiQueryProductState"), &[guid], u32::MAX);
        call(&name("MsiQueryProductState"), &[bad], (-2i32) as u32);
        call(&name("MsiQueryProductState"), &[0], (-2i32) as u32);
        call(&name("MsiQueryFeatureState"), &[guid, feature], u32::MAX);
        call(&name("MsiQueryFeatureState"), &[guid, 0], (-2i32) as u32);
        for base in ["MsiEnumProducts", "MsiEnumComponents"] {
            call(&name(base), &[0, OUTPUT], 259);
            call(&name(base), &[17, OUTPUT], 259);
            call(&name(base), &[0, 0], 87);
        }
        call(
            &name("MsiEnumFeatures"),
            &[guid, 0, OUTPUT, OUTPUT + 128],
            1605,
        );
        call(&name("MsiEnumClients"), &[guid, 0, OUTPUT], 1607);
        call(
            &name("MsiGetComponentPath"),
            &[guid, guid, OUTPUT, LENGTH],
            u32::MAX,
        );
        call(&name("MsiGetComponentPath"), &[guid, guid, 0, 0], u32::MAX);
        call(
            &name("MsiGetComponentPath"),
            &[guid, bad, OUTPUT, LENGTH],
            (-2i32) as u32,
        );
        call(&name("MsiGetProductCode"), &[guid, OUTPUT], 1607);
        call(
            &name("MsiGetProductInfo"),
            &[guid, feature, OUTPUT, LENGTH],
            1605,
        );
        call(&name("MsiGetProductInfo"), &[guid, feature, 0, 0], 1605);
        call(&name("MsiGetProductInfo"), &[guid, feature, OUTPUT, 0], 87);
        call(&name("MsiIsProductElevated"), &[guid, OUTPUT], 1605);
        call(
            &name("MsiGetFeatureUsage"),
            &[guid, feature, OUTPUT, LENGTH],
            1605,
        );
        call(&name("MsiGetFileHash"), &[path, 0, BAD_HASH], 87);
        call(&name("MsiGetFileHash"), &[path, 0, READ_ONLY], 87);
        call(&name("MsiGetFileHash"), &[path, 1, HASH], 87);
        call(&name("MsiGetFileHash"), &[missing, 0, HASH], 2);
        call(&name("MsiGetFileHash"), &[path, 0, HASH], 0);
    }
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("installer discovery", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, (name, value)) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 8).unwrap(),
            value,
            "{name}"
        );
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(),
            MARKER,
            "{name} LastError"
        );
    }
    let mut output = [0; 512];
    m.cpu.read(OUTPUT, &mut output).unwrap();
    assert_eq!(output, [0xa5; 512]);
    assert_eq!(m.cpu.read_u32(LENGTH).unwrap(), 64);
    let mut hash = [0; 20];
    m.cpu.read(HASH, &mut hash).unwrap();
    assert_eq!(&hash[..4], &20u32.to_le_bytes());
    assert_eq!(
        &hash[4..],
        &[
            0x90, 1, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1, 0x7f,
            0x72
        ]
    );
    m.cpu.read(BAD_HASH, &mut hash).unwrap();
    assert_eq!(&hash[..4], &16u32.to_le_bytes());
    assert_eq!(&hash[4..], &[0xcd; 16]);
    assert_eq!(m.cpu.read_u32(READ_ONLY).unwrap(), 0x5a5a5a5a);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict) && d.warnings.is_empty())
    );
    std::fs::remove_dir_all(directory).unwrap();
}
#[test]
// spec: PERS-INSTALL-1, PERS-INSTALL-2
fn empty_installer_queries_and_vfs_hashes_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        profile(cpu);
    }
    profile(UnicornCpu::new(PARK_ADDR).unwrap());
}
