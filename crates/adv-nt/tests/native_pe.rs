//! Differential PE calls against Wine 11 running entirely in the interpreter.
use adv_cpu::Cpu;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::dispatch::Platform;
use adv_pe::{ExportTarget, ImportName, Pe};
#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
use std::path::{Path, PathBuf};

const CODE: u32 = 0x400000;
const SLOT: u32 = 0x410000;
const ARGS: u32 = SLOT + 4;
const RESULT: u32 = SLOT + 0x40;
const DATA: u32 = 0x500000;
const LEN: u32 = 0x8000;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as u32
    }
}
fn dist() -> Option<PathBuf> {
    std::env::var_os("ADV_WINE_DIST").map(PathBuf::from)
}
fn address(pe: &Pe, name: &str) -> u32 {
    pe.exports()
        .unwrap()
        .into_iter()
        .find_map(|e| match (e.name, e.target) {
            (Some(n), ExportTarget::Rva(rva)) if n == name => Some(pe.image_base + rva),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing export {name}"))
}
fn map(m: &mut Machine<Cpu>, pe: &Pe) {
    adv_nt::image::map(
        &mut m.cpu,
        &mut m.sys.vm,
        &mut adv_nt::memory::Memory::default(),
        pe,
        pe.image_base,
    )
    .unwrap();
}
fn machine(dist: &Path, module: &str, jit: bool) -> (Machine<Cpu>, Pe) {
    let mut m = Machine::new(
        Cpu::new(PARK_ADDR, jit).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = dist.join("lib/wine/i386-windows");
    let nt = Pe::parse(&std::fs::read(root.join("ntdll.dll")).unwrap()).unwrap();
    map(&mut m, &nt);
    let pe = if module == "ntdll.dll" {
        nt.clone()
    } else {
        let pe = Pe::parse(&std::fs::read(root.join(module)).unwrap()).unwrap();
        map(&mut m, &pe);
        pe
    };
    adv_nt::identity::note_ntdll(
        &mut m,
        &root.join("ntdll.dll"),
        &std::fs::read(root.join("ntdll.dll")).unwrap(),
    );
    if module != "ntdll.dll" {
        adv_nt::identity::note_ntdll(
            &mut m,
            &root.join(module),
            &std::fs::read(root.join(module)).unwrap(),
        );
    }
    m.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    m.cpu.map(SLOT, 0x1000, Prot::RW).unwrap();
    m.cpu.map(DATA, LEN, Prot::RW).unwrap();
    m.cpu.map(0x7ffe0000, 0x1000, Prot::READ).unwrap();
    m.cpu.write(CODE + 0xf00, &[0x0f, 0x0b]).unwrap();
    if module != "ntdll.dll" {
        for imports in pe.imports().unwrap() {
            for import in imports.imports {
                let target = match import.name {
                    ImportName::Name { name, .. } => {
                        nt.exports()
                            .unwrap()
                            .into_iter()
                            .find_map(|e| match (e.name, e.target) {
                                (Some(n), ExportTarget::Rva(rva)) if n == name => {
                                    Some(nt.image_base + rva)
                                }
                                _ => None,
                            })
                    }
                    _ => None,
                }
                .unwrap_or(CODE + 0xf00);
                m.cpu
                    .write_u32(pe.image_base + import.iat_rva, target)
                    .unwrap();
            }
        }
    }
    // Wine's per-module debug channel starts disabled, as in a normal session.
    for (offset, bytes) in pe.image.windows(module.len() + 2).enumerate() {
        let stem = module.trim_end_matches(".dll");
        if bytes.starts_with(&[0xff]) && bytes[1..].starts_with(stem.as_bytes()) {
            m.cpu.write(pe.image_base + offset as u32, &[0]).unwrap();
        }
    }
    if !jit {
        m.sys.redirects.clear();
    }
    (m, pe)
}
fn prepare(m: &mut Machine<Cpu>, entry: u32, argc: usize, stdcall: bool) {
    m.cpu.write_u32(SLOT, entry).unwrap();
    let mut code = Vec::new();
    for (op, value) in [
        (0xbb, 0x12345678u32),
        (0xbe, 0x87654321),
        (0xbf, 0xabcdef12),
        (0xbd, 0x76543210),
    ] {
        code.push(op);
        code.extend_from_slice(&value.to_le_bytes());
    }
    code.extend_from_slice(&[0x89, 0x25]);
    code.extend_from_slice(&(RESULT + 4).to_le_bytes());
    for i in (0..argc).rev() {
        code.extend_from_slice(&[0xff, 0x35]);
        code.extend_from_slice(&(ARGS + 4 * i as u32).to_le_bytes());
    }
    code.extend_from_slice(&[0xff, 0x15]);
    code.extend_from_slice(&SLOT.to_le_bytes());
    if !stdcall {
        code.extend_from_slice(&[0x83, 0xc4, 4 * argc as u8]);
    }
    code.push(0xa3);
    code.extend_from_slice(&RESULT.to_le_bytes());
    for (reg, offset) in [(0x25, 8), (0x1d, 12), (0x35, 16), (0x3d, 20), (0x2d, 24)] {
        code.extend_from_slice(&[0x89, reg]);
        code.extend_from_slice(&(RESULT + offset).to_le_bytes());
    }
    code.extend_from_slice(&[0x64, 0xa1, 0x34, 0, 0, 0, 0xa3]);
    code.extend_from_slice(&(RESULT + 28).to_le_bytes());
    code.push(0xc3);
    m.cpu.write(CODE, &code).unwrap();
}
fn call(m: &mut Machine<Cpu>, data: &[u8], args: &[u32]) -> (Result<Vec<u32>, String>, Vec<u8>) {
    m.cpu.write(DATA, data).unwrap();
    for (i, arg) in args.iter().enumerate() {
        m.cpu.write_u32(ARGS + i as u32 * 4, *arg).unwrap();
    }
    let ctx = m.spawn("pe-call", CODE, &[], 0x10000).unwrap();
    m.cpu
        .write_u32(m.sys.contexts[ctx as usize].teb + 0x34, 0x1234abcd)
        .unwrap();
    let result = match m.run() {
        RunResult::Exited(_) => Ok((0..8)
            .map(|i| m.cpu.read_u32(RESULT + i * 4).unwrap())
            .collect()),
        RunResult::Failed(e) => Err(format!("{:#x}: {}", e.pc, e.message)),
        other => Err(format!("{other:?}")),
    };
    let mut output = vec![0; LEN as usize];
    m.cpu.read(DATA, &mut output).unwrap();
    (result, output)
}
fn same(
    native: &mut Machine<Cpu>,
    reference: &mut Machine<Cpu>,
    data: &[u8],
    args: &[u32],
    label: &str,
) {
    let abandons = native.sys.stats.leaf_abandons;
    let actual = call(native, data, args);
    let expected = call(reference, data, args);
    assert!(expected.0.is_ok(), "{label}: Wine failed: {:?}", expected.0);
    assert_eq!(actual.0, expected.0, "{label}: {args:x?}");
    assert_eq!(
        native.sys.stats.leaf_abandons, abandons,
        "{label}: valid call declined"
    );
    assert!(
        actual.1 == expected.1,
        "{label}: output differs, args {args:x?}; first difference {:?}",
        actual.1.iter().zip(&expected.1).position(|(a, b)| a != b)
    );
}
fn put(data: &mut [u8], at: u32, width: u32, value: u32) {
    let at = (at - DATA) as usize;
    data[at..at + width as usize].copy_from_slice(&value.to_le_bytes()[..width as usize]);
}

// spec: NAT-BODY-1, NAT-BODY-6
#[test]
fn crt_exports_match_wine() {
    let Some(dist) = dist() else {
        return;
    };
    let mut rng = Rng(0xb3f1a021d537dca9);
    for module in [
        "ntdll",
        "msvcrt",
        "ucrtbase",
        "msvcr70",
        "msvcr71",
        "msvcr80",
        "msvcr90",
        "msvcr100",
        "msvcr110",
        "msvcr120",
        "msvcr120_app",
        "msvcrt20",
        "msvcrt40",
        "msvcrtd",
    ] {
        let forwarded = Pe::parse(
            &std::fs::read(dist.join(format!("lib/wine/i386-windows/{module}.dll"))).unwrap(),
        )
        .unwrap();
        let target = match module {
            "msvcr120_app" => "msvcr120",
            "msvcrt20" | "msvcrt40" => "msvcrt",
            _ => module,
        };
        let (mut native, pe) = machine(&dist, &format!("{target}.dll"), true);
        let (mut reference, _) = machine(&dist, &format!("{target}.dll"), false);
        for name in [
            "memset", "memmove", "memcpy", "memcmp", "memchr", "strlen", "strnlen", "wcslen",
            "wcsnlen", "strcmp", "strncmp", "wcscmp", "wcsncmp", "strchr", "strrchr", "wcschr",
            "wcsrchr", "strcpy", "wcscpy", "strcat", "wcscat", "strstr", "wcsstr",
        ] {
            if !forwarded
                .exports()
                .unwrap()
                .iter()
                .any(|e| e.name.as_deref() == Some(name))
            {
                continue;
            }
            if target != module {
                let export = forwarded
                    .exports()
                    .unwrap()
                    .into_iter()
                    .find(|e| e.name.as_deref() == Some(name))
                    .unwrap();
                assert!(
                    matches!(export.target, ExportTarget::Forwarder(ref f) if f == &format!("{target}.{name}"))
                );
            }
            let entry = address(&pe, name);
            assert!(
                native.sys.redirects.contains_key(&entry)
                    || name == "memcpy"
                    || (target == "ntdll" && name == "memmove"),
                "{module}!{name}: missing redirect"
            );
            let argc = if ["strlen", "wcslen"].contains(&name) {
                1
            } else if [
                "memset", "memmove", "memcpy", "memcmp", "memchr", "strncmp", "wcsncmp",
            ]
            .contains(&name)
            {
                3
            } else {
                2
            };
            prepare(&mut native, entry, argc, false);
            prepare(&mut reference, entry, argc, false);
            let before = native.sys.stats.leaf_calls;
            for case in 0..48 {
                let mut data = vec![0; LEN as usize];
                data.iter_mut()
                    .for_each(|b| *b = (rng.next() % 255 + 1) as u8);
                let n = match case % 6 {
                    0 => 0,
                    1 => 1,
                    2 => 15,
                    3 => 16,
                    4 => 17,
                    _ => rng.next() % 2048,
                };
                let width = if name.starts_with("wcs") { 2 } else { 1 };
                let mut a = DATA + case % 16;
                let mut b = DATA + 0x3000 + (case * 7) % 16;
                let mut args = vec![a, b, n];
                if name.starts_with("mem") {
                    if ["memcpy", "memmove"].contains(&name) && case % 3 != 0 {
                        b = a + 1 + case % 63;
                        if case % 3 == 1 {
                            std::mem::swap(&mut a, &mut b);
                        }
                    }
                    if case % 8 == 7 {
                        a = DATA + LEN - n;
                    }
                    args = vec![
                        a,
                        if ["memset", "memchr"].contains(&name) {
                            rng.next()
                        } else {
                            b
                        },
                        n,
                    ];
                    if name == "memcmp" && case % 3 == 0 {
                        let source = data[(a - DATA) as usize..(a - DATA + n) as usize].to_vec();
                        data[(b - DATA) as usize..(b - DATA + n) as usize].copy_from_slice(&source);
                    }
                } else {
                    if case % 8 == 7 && !["strcat", "wcscat", "strcpy", "wcscpy"].contains(&name) {
                        a = DATA + LEN - (n + 1) * width;
                    }
                    for i in 0..n {
                        put(
                            &mut data,
                            a + i * width,
                            width,
                            rng.next() % if width == 1 { 255 } else { 65535 } + 1,
                        );
                    }
                    put(&mut data, a + n * width, width, 0);
                    let source =
                        data[(a - DATA) as usize..(a - DATA + (n + 1) * width) as usize].to_vec();
                    data[(b - DATA) as usize..(b - DATA + (n + 1) * width) as usize]
                        .copy_from_slice(&source);
                    if case % 3 == 1 {
                        put(
                            &mut data,
                            b + n / 2 * width,
                            width,
                            rng.next() & if width == 1 { 0xff } else { 0xffff },
                        );
                    }
                    args[0] = a;
                    if name.ends_with("chr") {
                        args[1] = if case % 3 == 0 {
                            0
                        } else {
                            u32::from(data[(a - DATA) as usize])
                        };
                        if width == 1 {
                            args[1] |= 0x123400;
                        }
                    } else if name.ends_with("nlen") {
                        args[1] = [0, n / 2, n, n + 5][case as usize % 4];
                    }
                    if name.ends_with("ncmp") {
                        args[2] = [0, n / 2, n, n + 5][case as usize % 4];
                    }
                    if name.ends_with("str") && case % 4 == 0 {
                        args[1] = a + (n / 2) * width;
                    }
                    if ["strcpy", "wcscpy", "strcat", "wcscat"].contains(&name) {
                        args[0] = DATA + 0x6000 + case % 16;
                        let prefix = if name.ends_with("cat") {
                            rng.next() % 17
                        } else {
                            0
                        };
                        for i in 0..prefix {
                            put(&mut data, args[0] + i * width, width, 0x58);
                        }
                        put(&mut data, args[0] + prefix * width, width, 0);
                    }
                }
                same(
                    &mut native,
                    &mut reference,
                    &data,
                    &args[..argc],
                    &format!("{module}!{name} case {case}"),
                );
            }
            assert!(
                native.sys.stats.leaf_calls > before,
                "{module}!{name} never reached a body"
            );
            eprintln!(
                "{module}!{name}: {} native calls",
                native.sys.stats.leaf_calls - before
            );
        }
    }
}

const KB: u32 = 0x7b000000;
const NT: u32 = 0x7bc00000;
const TABLES: u32 = 0x1800000;
const NORM: u32 = 0x1c00000;
const PAGES: [u32; 7] = [932, 936, 949, 950, 1252, 437, 1258];

fn nls_machine(dist: &Path, jit: bool) -> (Machine<Cpu>, Pe, Pe) {
    let (mut m, kb) = machine(dist, "kernelbase.dll", jit);
    let nt =
        Pe::parse(&std::fs::read(dist.join("lib/wine/i386-windows/ntdll.dll")).unwrap()).unwrap();
    m.cpu.map(TABLES, 0x1000, Prot::RW).unwrap();
    for (i, page) in PAGES.iter().enumerate() {
        let bytes = std::fs::read(dist.join(format!("share/wine/nls/c_{page}.nls"))).unwrap();
        let at = 0x2000000 + i as u32 * 0x40000;
        m.cpu
            .map(at, (bytes.len() as u32 + 0xfff) & !0xfff, Prot::READ)
            .unwrap();
        m.cpu.write(at, &bytes).unwrap();
        prepare(&mut m, address(&nt, "RtlInitCodePageTable"), 2, true);
        call(
            &mut m,
            &vec![0; LEN as usize],
            &[at, TABLES + i as u32 * 0x2c],
        )
        .0
        .unwrap();
    }
    let bytes = std::fs::read(dist.join("share/wine/nls/normnfc.nls")).unwrap();
    m.cpu
        .map(NORM, (bytes.len() as u32 + 0xfff) & !0xfff, Prot::READ)
        .unwrap();
    m.cpu.write(NORM, &bytes).unwrap();
    m.cpu.write_u32(KB + 0xbf45c, NORM).unwrap();
    m.cpu
        .copy_memory(TABLES + 4 * 0x2c, KB + 0xc0b00, 0x2c)
        .unwrap();
    m.cpu
        .copy_memory(TABLES + 5 * 0x2c, KB + 0xc0ac0, 0x2c)
        .unwrap();
    m.cpu
        .copy_memory(TABLES, KB + 0xbf480, PAGES.len() * 0x2c)
        .unwrap();
    m.cpu.write_u32(KB + 0xbf460, PAGES.len() as u32).unwrap();
    (m, kb, nt)
}

// spec: NAT-BODY-5
#[test]
fn code_pages_match_wine() {
    let Some(dist) = dist() else {
        return;
    };
    let (mut native, kb, nt) = nls_machine(&dist, true);
    let (mut reference, _, _) = nls_machine(&dist, false);
    let mut rng = Rng(0x61ace5ff6e379b2d);
    for name in [
        "MultiByteToWideChar",
        "WideCharToMultiByte",
        "RtlMultiByteToUnicodeN",
        "RtlUnicodeToMultiByteN",
        "RtlMultiByteToUnicodeSize",
        "RtlUnicodeToMultiByteSize",
        "RtlOemToUnicodeN",
        "RtlUnicodeToOemN",
    ] {
        let kernel = !name.starts_with("Rtl");
        let wide = name.starts_with("Wide") || name.starts_with("RtlUnicode");
        let size = name.ends_with("Size");
        let argc = if kernel {
            if wide { 8 } else { 6 }
        } else if size {
            3
        } else {
            5
        };
        let entry = address(if kernel { &kb } else { &nt }, name);
        assert!(
            native.sys.redirects.contains_key(&entry),
            "{name} not bound"
        );
        prepare(&mut native, entry, argc, true);
        prepare(&mut reference, entry, argc, true);
        for (page_index, page) in PAGES.iter().enumerate() {
            for m in [&mut native, &mut reference] {
                m.cpu
                    .copy_memory(TABLES + page_index as u32 * 0x2c, NT + 0x723e0, 0x2c)
                    .unwrap();
                m.cpu
                    .copy_memory(TABLES + page_index as u32 * 0x2c, NT + 0x723e0 + 0x2c, 0x2c)
                    .unwrap();
            }
            let before = native.sys.stats.leaf_calls;
            let flag_count = if !kernel {
                1
            } else if wide {
                64
            } else {
                16
            };
            for flag_bits in 0..flag_count {
                let flags = if wide {
                    (if flag_bits & 1 != 0 { 0x10 } else { 0 })
                        | (if flag_bits & 2 != 0 { 0x20 } else { 0 })
                        | (if flag_bits & 4 != 0 { 0x40 } else { 0 })
                        | (if flag_bits & 8 != 0 { 0x80 } else { 0 })
                        | (if flag_bits & 16 != 0 { 0x200 } else { 0 })
                        | (if flag_bits & 32 != 0 { 0x400 } else { 0 })
                } else {
                    flag_bits
                };
                for case in 0..12 {
                    let mut data = vec![0xcc; LEN as usize];
                    let src = DATA + case % 4;
                    let dst = DATA + 0x4000 + case % 4;
                    let n = if case < 4 {
                        case + 1
                    } else {
                        rng.next() % 100 + 2
                    };
                    if wide {
                        for i in 0..n {
                            put(&mut data, src + 2 * i, 2, rng.next() & 0xffff);
                        }
                        let edge: &[u16] = match case % 6 {
                            0 => &[0x65, 0x301, 0, 0xd800, 0xdc00],
                            1 => &[0x304b, 0x3099, 0x4e00, 0xffff],
                            2 => &[0x41, 0x30a, 0x65, 0x301, 0x323],
                            3 => &[0x41, 0x308, 0x1100, 0x1161],
                            4 => &[0x3f, 0x100, 0x20ac, 0xe000],
                            _ => &[0xfffd, 0xff, 0xdfff, 0x1, 0],
                        };
                        for (i, ch) in edge.iter().take(n as usize).enumerate() {
                            put(&mut data, src + i as u32 * 2, 2, u32::from(*ch));
                        }
                        put(&mut data, src + n * 2, 2, 0);
                    } else {
                        for i in 0..n {
                            put(&mut data, src + i, 1, rng.next());
                        }
                        let edge: &[u8] = match case % 6 {
                            0 => &[0x81, 0],
                            1 => &[0xff, 0x81, 0xff],
                            2 => &[0x82, 0xa0, 0x81],
                            3 => &[0, 0x81],
                            4 => &[0xa1, 0xa1, 0xff, 0xfe],
                            _ => &[0x80, 0x81, 0xff, 0x7f],
                        };
                        for (i, ch) in edge.iter().take(n as usize).enumerate() {
                            put(&mut data, src + i as u32, 1, u32::from(*ch));
                        }
                        put(
                            &mut data,
                            src + n - 1,
                            1,
                            if case % 2 == 0 { 0x81 } else { 0xff },
                        );
                        put(&mut data, src + n, 1, 0);
                    }
                    let capacity = match case % 6 {
                        0 => 0,
                        1 => 1,
                        2 => n / 2,
                        3 => n,
                        _ => n * 4 + 10,
                    };
                    let used = if case % 3 != 0 { DATA + 0x7000 } else { 0 };
                    let def = if case % 4 == 0 { 0 } else { DATA + 0x7004 };
                    put(
                        &mut data,
                        DATA + 0x7004,
                        2,
                        if case % 2 == 0 { 0x4081 } else { 0x21 },
                    );
                    let srclen = if case % 5 == 4 { u32::MAX } else { n };
                    let args = if kernel {
                        let mut args = vec![
                            *page,
                            flags,
                            src,
                            srclen,
                            if capacity == 0 { 0 } else { dst },
                            capacity,
                        ];
                        if wide {
                            args.extend_from_slice(&[def, used]);
                        }
                        args
                    } else if size {
                        vec![DATA + 0x7000, src, n * if wide { 2 } else { 1 } + case % 2]
                    } else {
                        vec![
                            dst,
                            capacity,
                            used,
                            src,
                            n * if wide { 2 } else { 1 } + case % 2,
                        ]
                    };
                    same(
                        &mut native,
                        &mut reference,
                        &data,
                        &args,
                        &format!("{name} cp{page} flags{flags:x} case{case}"),
                    );
                }
            }
            assert!(
                native.sys.stats.leaf_calls > before,
                "{name} cp{page} never accepted a call"
            );
        }
    }
}

// spec: NAT-BODY-5
#[test]
fn nls_layout_matches_dwarf() {
    let Some(dist) = dist() else {
        return;
    };
    let source = include_str!("../src/native/nls.rs");
    for module in ["ntdll", "kernelbase"] {
        let dll = dist
            .with_file_name("wine11-sse-dist")
            .join(format!("usr/local/lib/wine/i386-windows/{module}.dll"));
        let pe = Pe::parse(&std::fs::read(&dll).unwrap()).unwrap();
        let dump = std::process::Command::new("dwarfdump")
            .arg("--debug-info")
            .arg(dll)
            .output()
            .unwrap();
        assert!(dump.status.success());
        let text = String::from_utf8(dump.stdout).unwrap();
        let entries: Vec<_> = text.split("\n0x").collect();
        let structure = |name: &str| {
            let start = entries
                .iter()
                .position(|e| {
                    e.contains(":   DW_TAG_structure_type")
                        && e.contains(&format!("DW_AT_name\t(\"{name}\")"))
                })
                .unwrap();
            entries[start + 1..]
                .iter()
                .take_while(|e| !e.contains(":     NULL"))
                .copied()
                .collect::<Vec<_>>()
        };
        let variable = |name: &str| {
            let entry = entries
                .iter()
                .find(|e| {
                    e.contains(":   DW_TAG_variable")
                        && e.contains(&format!("DW_AT_name\t(\"{name}\")"))
                })
                .unwrap();
            let addr = entry
                .split("DW_OP_addr 0x")
                .nth(1)
                .unwrap()
                .split(')')
                .next()
                .unwrap();
            u32::from_str_radix(addr, 16).unwrap() - pe.image_base
        };
        for (block, name) in [
            ("cp", "_CPTABLEINFO"),
            ("norm", "norm_table"),
            ("critical", "_RTL_CRITICAL_SECTION"),
            ("kb", ""),
        ] {
            if block == "kb" && module != "kernelbase" {
                continue;
            }
            let fields = if block == "kb" {
                Vec::new()
            } else {
                structure(name)
            };
            let constants = source
                .split(&format!("mod {block} {{"))
                .nth(1)
                .unwrap()
                .split('}')
                .next()
                .unwrap();
            for line in constants.lines().filter(|l| l.contains("pub const")) {
                let field = line
                    .split("pub const ")
                    .nth(1)
                    .unwrap()
                    .split(':')
                    .next()
                    .unwrap();
                let value = line.split("= ").nth(1).unwrap().trim_end_matches(';');
                let expected = if let Some(hex) = value.strip_prefix("0x") {
                    u32::from_str_radix(hex, 16).unwrap()
                } else {
                    value.parse().unwrap()
                };
                let actual = if block == "kb" {
                    variable(field)
                } else if field == "SIZE" {
                    let entry = entries
                        .iter()
                        .find(|e| {
                            e.contains(":   DW_TAG_structure_type")
                                && e.contains(&format!("DW_AT_name\t(\"{name}\")"))
                        })
                        .unwrap();
                    u32::from_str_radix(
                        entry
                            .split("DW_AT_byte_size\t(0x")
                            .nth(1)
                            .unwrap()
                            .split(')')
                            .next()
                            .unwrap(),
                        16,
                    )
                    .unwrap()
                } else {
                    let member = fields
                        .iter()
                        .find(|e| e.contains(&format!("DW_AT_name\t(\"{field}\")")))
                        .unwrap();
                    u32::from_str_radix(
                        member
                            .split("DW_AT_data_member_location\t(0x")
                            .nth(1)
                            .unwrap()
                            .split(')')
                            .next()
                            .unwrap(),
                        16,
                    )
                    .unwrap()
                };
                assert_eq!(actual, expected, "{module}: {name}.{field}");
            }
        }
        if module == "ntdll" {
            let constant = |name: &str| {
                let value = source
                    .split(&format!("const {name}: u32 = 0x"))
                    .nth(1)
                    .unwrap()
                    .split(';')
                    .next()
                    .unwrap();
                u32::from_str_radix(value, 16).unwrap()
            };
            assert_eq!(variable("nls_info"), constant("NLS_INFO"));
            for (name, field, offset) in [
                ("_NLSTABLEINFO", "AnsiTableInfo", constant("ANSI_TABLE")),
                ("_NLSTABLEINFO", "OemTableInfo", 0),
                ("_TEB", "LastErrorValue", constant("LAST_ERROR")),
            ] {
                let fields = structure(name);
                let member = fields
                    .iter()
                    .find(|e| e.contains(&format!("DW_AT_name\t(\"{field}\")")))
                    .unwrap();
                let actual = member
                    .split("DW_AT_data_member_location\t(0x")
                    .nth(1)
                    .unwrap()
                    .split(')')
                    .next()
                    .unwrap();
                assert_eq!(
                    u32::from_str_radix(actual, 16).unwrap(),
                    offset,
                    "{name}.{field}"
                );
            }
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-6
#[test]
fn crt_faults_resume_wine_before_writes() {
    let Some(dist) = dist() else {
        return;
    };
    for module in ["ntdll.dll", "msvcrt.dll"] {
        for name in [
            "memset", "memmove", "memcpy", "memcmp", "memchr", "strlen", "strnlen", "wcslen",
            "wcsnlen", "strcmp", "strncmp", "wcscmp", "wcsncmp", "strchr", "strrchr", "wcschr",
            "wcsrchr", "strcpy", "wcscpy", "strcat", "wcscat", "strstr", "wcsstr",
        ] {
            let start = u32::from(name == "memset");
            for fault in start
                ..if [
                    "memset", "memmove", "memcpy", "strcpy", "wcscpy", "strcat", "wcscat",
                ]
                .contains(&name)
                {
                    3
                } else {
                    1
                }
            {
                let (mut native, pe) = machine(&dist, module, true);
                let (mut reference, _) = machine(&dist, module, false);
                let argc = if ["strlen", "wcslen"].contains(&name) {
                    1
                } else if [
                    "memset", "memmove", "memcpy", "memcmp", "memchr", "strncmp", "wcsncmp",
                ]
                .contains(&name)
                {
                    3
                } else {
                    2
                };
                let entry = address(&pe, name);
                prepare(&mut native, entry, argc, false);
                prepare(&mut reference, entry, argc, false);
                let mut data = vec![0x41; LEN as usize];
                let width = if name.starts_with("wcs") { 2 } else { 1 };
                for i in [4, 0x1004] {
                    put(&mut data, DATA + i, width, 0);
                }
                let warm = [
                    DATA,
                    if ["strnlen", "wcsnlen"].contains(&name) {
                        8
                    } else if name.ends_with("chr") {
                        0xff
                    } else {
                        DATA + 0x1000
                    },
                    8,
                ];
                for _ in 0..4 {
                    same(&mut native, &mut reference, &data, &warm[..argc], name);
                }
                data.fill(0x41);
                let end = DATA + LEN - 4;
                let b = DATA + 0x1000;
                let dst = DATA + 0x6000;
                let mut args = [end, b, 8];
                if ["memmove", "memcpy", "strcpy", "wcscpy"].contains(&name) {
                    args = if fault == 0 {
                        [dst, end, 8]
                    } else {
                        [if fault == 1 { end } else { dst }, b, 8]
                    };
                    if fault != 0 {
                        put(&mut data, b + 8 * width, width, 0);
                    }
                } else if ["strcat", "wcscat"].contains(&name) {
                    args = [if fault == 2 { dst } else { end }, b, 8];
                    if fault != 0 {
                        put(&mut data, args[0] + width, width, 0);
                    }
                    put(&mut data, b + 8 * width, width, 0);
                } else if name == "memset" {
                    args = [if fault == 2 { dst } else { end }, 0xff, 8];
                } else if name.ends_with("chr") {
                    args[1] = 0xff;
                } else if name.ends_with("nlen") {
                    args[1] = 8;
                } else if name.ends_with("str") {
                    put(&mut data, b + 8 * width, width, 0);
                }
                if fault == 2 {
                    native.cpu.protect(dst, 0x1000, Prot::READ).unwrap();
                    reference.cpu.protect(dst, 0x1000, Prot::READ).unwrap();
                }
                let before = native.sys.stats.leaf_abandons;
                let actual = call(&mut native, &data, &args[..argc]);
                let expected = call(&mut reference, &data, &args[..argc]);
                assert!(
                    expected.0.is_err(),
                    "{module}!{name} fault {fault}: {:?}",
                    expected.0
                );
                assert_eq!(actual.0, expected.0, "{module}!{name} fault {fault}");
                assert!(
                    actual.1 == expected.1,
                    "{module}!{name} fault {fault}: partial writes differ"
                );
                assert!(
                    native.sys.stats.leaf_abandons > before,
                    "{module}!{name} fault {fault}: did not decline"
                );
            }
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-5
#[test]
fn code_page_faults_resume_wine_before_writes() {
    let Some(dist) = dist() else {
        return;
    };
    for name in [
        "MultiByteToWideChar",
        "WideCharToMultiByte",
        "RtlMultiByteToUnicodeN",
        "RtlUnicodeToMultiByteN",
        "RtlMultiByteToUnicodeSize",
        "RtlUnicodeToMultiByteSize",
        "RtlOemToUnicodeN",
        "RtlUnicodeToOemN",
    ] {
        for fault in 0..if name.ends_with("Size") { 2 } else { 3 } {
            let (mut native, kb, nt) = nls_machine(&dist, true);
            let (mut reference, _, _) = nls_machine(&dist, false);
            let kernel = !name.starts_with("Rtl");
            let wide = name.starts_with("Wide") || name.starts_with("RtlUnicode");
            let size = name.ends_with("Size");
            for m in [&mut native, &mut reference] {
                m.cpu.copy_memory(TABLES, NT + 0x723e0, 0x2c).unwrap();
                m.cpu
                    .copy_memory(TABLES, NT + 0x723e0 + 0x2c, 0x2c)
                    .unwrap();
            }
            let data = vec![0x41; LEN as usize];
            let argc = if kernel {
                if wide { 8 } else { 6 }
            } else if size {
                3
            } else {
                5
            };
            let entry = address(if kernel { &kb } else { &nt }, name);
            prepare(&mut native, entry, argc, true);
            prepare(&mut reference, entry, argc, true);
            let args = if kernel {
                let mut args = vec![932, 0, DATA, 12, DATA + 0x4000, 32];
                if wide {
                    args.extend_from_slice(&[0, DATA + 0x7000]);
                }
                args
            } else if size {
                vec![DATA + 0x7000, DATA, 12]
            } else {
                vec![DATA + 0x4000, 32, DATA + 0x7000, DATA, 12]
            };
            for _ in 0..4 {
                same(&mut native, &mut reference, &data, &args, name);
            }
            let before = native.sys.stats.leaf_abandons;
            let mut args = args;
            if fault == 0 {
                args[if kernel {
                    2
                } else if size {
                    1
                } else {
                    3
                }] = DATA + LEN - 4;
            } else if size {
                args[0] = DATA + LEN - 2;
            } else if fault == 1 {
                args[if kernel { 4 } else { 0 }] = DATA + LEN - 2;
            } else if kernel && !wide {
                native
                    .cpu
                    .protect(DATA + 0x4000, 0x1000, Prot::READ)
                    .unwrap();
                reference
                    .cpu
                    .protect(DATA + 0x4000, 0x1000, Prot::READ)
                    .unwrap();
            } else {
                args[if kernel { 7 } else { 2 }] = DATA + LEN - 2;
            }
            let actual = call(&mut native, &data, &args);
            let expected = call(&mut reference, &data, &args);
            assert!(
                expected.0.is_err(),
                "{name} fault {fault}: {:?}",
                expected.0
            );
            assert_eq!(actual.0, expected.0, "{name} fault {fault}");
            assert!(
                actual.1 == expected.1,
                "{name} fault {fault}: partial writes differ"
            );
            assert!(
                native.sys.stats.leaf_abandons > before,
                "{name} fault {fault}: did not decline"
            );
        }
    }
}

// spec: NAT-BODY-5
#[test]
fn code_page_parameters_match_wine() {
    let Some(dist) = dist() else {
        return;
    };
    let (mut native, kb, _) = nls_machine(&dist, true);
    let (mut reference, _, _) = nls_machine(&dist, false);
    for m in [&mut native, &mut reference] {
        m.cpu.write_u32(KB + 0x871b8, 932).unwrap();
    }
    for name in ["MultiByteToWideChar", "WideCharToMultiByte"] {
        let wide = name.starts_with("Wide");
        let argc = if wide { 8 } else { 6 };
        prepare(&mut native, address(&kb, name), argc, true);
        prepare(&mut reference, address(&kb, name), argc, true);
        let mut data = vec![0x41; LEN as usize];
        put(&mut data, DATA + 24, if wide { 2 } else { 1 }, 0);
        for case in 0..24 {
            let mut args = [932, 0, DATA, 12, DATA + 0x4000, 32, 0, DATA + 0x7000];
            match case % 12 {
                0 => args[2] = 0,
                1 => args[3] = 0,
                2 => args[4] = 0,
                3 => args[5] = u32::MAX,
                4 => args[1] = 0x10000,
                5 => args[3] = u32::MAX - 1,
                6 => {
                    args[4] = 0;
                    args[5] = 0;
                }
                7 => args[0] = 0,
                8 => args[0] = 1,
                9 => args[0] = 65010,
                _ => {}
            }
            same(&mut native, &mut reference, &data, &args[..argc], name);
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-5
#[test]
fn cached_code_pages_resume_wine_with_owned_lock() {
    let Some(dist) = dist() else {
        return;
    };
    let (mut native, kb, _) = nls_machine(&dist, true);
    let (mut reference, _, _) = nls_machine(&dist, false);
    for name in ["MultiByteToWideChar", "WideCharToMultiByte"] {
        let wide = name.starts_with("Wide");
        let argc = if wide { 8 } else { 6 };
        let entry = address(&kb, name);
        let args = [932, 0, DATA, 12, DATA + 0x4000, 32, 0, DATA + 0x7000];
        let data = vec![0x41; LEN as usize];
        for m in [&mut native, &mut reference] {
            m.cpu.write_u32(KB + 0x871a4, u32::MAX).unwrap();
            m.cpu.write_u32(KB + 0x871a8, 0).unwrap();
            m.cpu.write_u32(KB + 0x871ac, 0).unwrap();
            prepare(m, entry, argc, true);
        }
        for _ in 0..4 {
            same(&mut native, &mut reference, &data, &args[..argc], name);
        }
        for m in [&mut native, &mut reference] {
            let mut original = [0; 256];
            m.cpu.read(CODE, &mut original).unwrap();
            // The caller owns the locale lock. Wine reenters it recursively.
            let mut code = vec![0x64, 0xa1, 0x24, 0, 0, 0, 0xa3];
            code.extend_from_slice(&(KB + 0x871ac).to_le_bytes());
            for (at, value) in [(KB + 0x871a4, 0u32), (KB + 0x871a8, 1)] {
                code.extend_from_slice(&[0xc7, 0x05]);
                code.extend_from_slice(&at.to_le_bytes());
                code.extend_from_slice(&value.to_le_bytes());
            }
            code.extend_from_slice(&original);
            m.cpu.write(CODE, &code).unwrap();
        }
        let before = native.sys.stats.leaf_abandons;
        for _ in 0..4 {
            let actual = call(&mut native, &data, &args[..argc]);
            let expected = call(&mut reference, &data, &args[..argc]);
            assert!(expected.0.is_ok(), "{name}: {:?}", expected.0);
            assert_eq!(actual.0, expected.0, "{name}");
            assert!(actual.1 == expected.1, "{name}: bytes differ");
            for m in [&mut native, &mut reference] {
                assert_eq!(m.cpu.read_u32(KB + 0x871a4).unwrap(), 0);
                assert_eq!(m.cpu.read_u32(KB + 0x871a8).unwrap(), 1);
            }
        }
        assert!(
            native.sys.stats.leaf_abandons > before,
            "{name}: locked lookup did not decline"
        );
    }
}

struct CountingGuest<'a> {
    cpu: &'a mut dyn Guest,
    calls: std::cell::Cell<usize>,
    pages: std::cell::RefCell<std::collections::BTreeSet<u32>>,
    stores: usize,
}
impl CountingGuest<'_> {
    fn touch(&self, at: u32, len: usize) {
        self.calls.set(self.calls.get() + 1);
        if len != 0 {
            let page = adv_machine::layout::PAGE;
            self.pages
                .borrow_mut()
                .extend(at / page..=(at + len as u32 - 1) / page);
        }
    }
}
impl Guest for CountingGuest<'_> {
    fn reg(&self, r: adv_cpu::backend::Reg) -> u32 {
        self.cpu.reg(r)
    }
    fn set_reg(&mut self, r: adv_cpu::backend::Reg, value: u32) {
        self.cpu.set_reg(r, value);
    }
    fn read(&self, at: u32, data: &mut [u8]) -> Result<(), adv_cpu::backend::MemFault> {
        self.touch(at, data.len());
        self.cpu.read(at, data)
    }
    fn write(&mut self, at: u32, data: &[u8]) -> Result<(), adv_cpu::backend::MemFault> {
        self.touch(at, data.len());
        self.stores += 1;
        self.cpu.write(at, data)
    }
    fn accessible(&self, at: u32, len: u32, prot: Prot) -> bool {
        self.touch(at, len as usize);
        self.cpu.accessible(at, len, prot)
    }
    fn shared_bytes(&self, at: u32, len: usize) -> Option<&[u8]> {
        self.touch(at, len);
        // Wine's NLS file mappings require a bulk read instead of a borrow.
        if !(DATA..DATA + LEN).contains(&at) {
            return None;
        }
        self.cpu.shared_bytes(at, len)
    }
    fn copy_memory(
        &mut self,
        src: u32,
        dst: u32,
        len: usize,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        self.touch(src, len);
        self.touch(dst, len);
        self.stores += 1;
        self.cpu.copy_memory(src, dst, len)
    }
    fn fill_memory(
        &mut self,
        dst: u32,
        byte: u8,
        len: usize,
    ) -> Result<(), adv_cpu::backend::MemFault> {
        self.touch(dst, len);
        self.stores += 1;
        self.cpu.fill_memory(dst, byte, len)
    }
    fn map(&mut self, at: u32, len: u32, prot: Prot) -> Result<(), adv_cpu::backend::MemFault> {
        self.cpu.map(at, len, prot)
    }
    fn protect(&mut self, at: u32, len: u32, prot: Prot) -> Result<(), adv_cpu::backend::MemFault> {
        self.cpu.protect(at, len, prot)
    }
    fn unmap(&mut self, at: u32, len: u32) -> Result<(), adv_cpu::backend::MemFault> {
        self.cpu.unmap(at, len)
    }
}

fn counted_body(m: &mut Machine<Cpu>, name: &str, context: u32, args: &[u32]) {
    let thunk = m
        .sys
        .thunks
        .published_index(adv_nt::native::MODULE, name)
        .unwrap();
    let body = m.sys.thunks.get(thunk).f.env_leaf.unwrap();
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, SLOT);
    for (i, arg) in args.iter().enumerate() {
        m.cpu.write_u32(ARGS + i as u32 * 4, *arg).unwrap();
    }
    let mut cpu = CountingGuest {
        cpu: &mut m.cpu,
        calls: std::cell::Cell::new(0),
        pages: std::cell::RefCell::new(std::collections::BTreeSet::new()),
        stores: 0,
    };
    let mut env = adv_machine::Env {
        context,
        cpu: &mut cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    };
    assert!(body(&mut env).is_some(), "{name} declined");
    let pages = cpu.pages.borrow().len();
    assert!(
        cpu.calls.get() <= 8 * pages + 16,
        "{name}: {} calls for {pages} pages",
        cpu.calls.get()
    );
    assert!(
        cpu.stores <= if name.starts_with("kernelbase") { 3 } else { 1 },
        "{name}: {} stores",
        cpu.stores
    );
}

// spec: NAT-BODY-5, NAT-BODY-6, NAT-BODY-7
#[test]
fn pe_bodies_access_memory_by_page() {
    let Some(dist) = dist() else {
        return;
    };
    for module in ["ntdll.dll", "msvcrt.dll", "ucrtbase.dll"] {
        let (mut m, _) = machine(&dist, module, true);
        let context = m.spawn("counted-body", CODE, &[], 0x10000).unwrap();
        for name in [
            "memset", "memmove", "memcmp", "memchr", "strlen", "strnlen", "wcslen", "wcsnlen",
            "strcmp", "strncmp", "wcscmp", "wcsncmp", "strchr", "strrchr", "wcschr", "wcsrchr",
            "strcpy", "wcscpy", "strcat", "wcscat", "strstr", "wcsstr",
        ] {
            // ntdll's memmove jumps to its existing memcpy body.
            let export = if module == "ntdll.dll" && name == "memmove" {
                "memcpy"
            } else {
                name
            };
            for offset in [0x100, 0xf01] {
                let (a, b) = (DATA + offset, DATA + 0x4000);
                let width = if name.starts_with("wcs") { 2 } else { 1 };
                let mut data = vec![0; LEN as usize];
                for at in [a, b] {
                    for i in 0..1000 {
                        put(&mut data, at + i * width, width, 0x41);
                    }
                }
                m.cpu.write(DATA, &data).unwrap();
                let args = if name == "memset" {
                    vec![a, 0x41, 1000]
                } else if name.ends_with("nlen") {
                    vec![a, 1000]
                } else if name.ends_with("chr") {
                    vec![a, 0x7f, 1000]
                } else {
                    vec![a, b, 1000]
                };
                counted_body(&mut m, &format!("{module}!{export}"), context, &args);
            }
        }
    }
    let (mut m, _, _) = nls_machine(&dist, true);
    let context = m.spawn("counted-conversion", CODE, &[], 0x10000).unwrap();
    for code in [1252, 932] {
        for flags in [0, 2, 8] {
            for len in [1000, u32::MAX] {
                m.cpu.write(DATA, &[0x41; 1000]).unwrap();
                m.cpu.write(DATA + 1000, &[0]).unwrap();
                counted_body(
                    &mut m,
                    "kernelbase.dll!MultiByteToWideChar",
                    context,
                    &[code, flags, DATA, len, DATA + 0x4000, 2000],
                );
            }
        }
        for flags in [0, 0x200, 0x400] {
            for len in [1000, u32::MAX] {
                for i in 0..1000 {
                    m.cpu.write(DATA + i * 2, &0x41u16.to_le_bytes()).unwrap();
                }
                m.cpu.write(DATA + 2000, &[0, 0]).unwrap();
                counted_body(
                    &mut m,
                    "kernelbase.dll!WideCharToMultiByte",
                    context,
                    &[
                        code,
                        flags,
                        DATA,
                        len,
                        DATA + 0x4000,
                        2000,
                        0,
                        DATA + 0x7000,
                    ],
                );
            }
        }
    }
    // Exercise bulk shared reads and outputs that exceed the stack buffers.
    let large = 0x900000;
    m.cpu.map(large, 0x50000, Prot::RW).unwrap();
    m.cpu.fill_memory(large, 0x41, 0x50000).unwrap();
    counted_body(
        &mut m,
        "kernelbase.dll!MultiByteToWideChar",
        context,
        &[1252, 0, large, 5000, DATA + 0x4000, 5000],
    );
    let mut output = [0; 10000];
    m.cpu.read(DATA + 0x4000, &mut output).unwrap();
    assert!(output.as_chunks::<2>().0.iter().all(|ch| *ch == [0x41, 0]));
    counted_body(
        &mut m,
        "kernelbase.dll!MultiByteToWideChar",
        context,
        &[1252, 0, large, 0x50000, DATA + 0x4000, 1000],
    );
}
