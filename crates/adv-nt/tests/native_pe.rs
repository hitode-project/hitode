//! Differential PE calls against Wine 11 running entirely in the interpreter.
use adv_cpu::Cpu;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::dispatch::Platform;
use adv_pe::{ExportTarget, ImportName, Pe};
#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
use std::path::{Path, PathBuf};

#[path = "native_pe/deflate.rs"]
mod deflate;

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

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
    wine::dist()
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
        let stem = if module.starts_with("d3dx9_") {
            "d3dx"
        } else {
            module.trim_end_matches(".dll")
        };
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
    let abandons = adv_nt::native::declines(&native.ext);
    let actual = call(native, data, args);
    let mut expected = call(reference, data, args);
    // Windows 11 x86 differs from the pinned Wine in these results.
    if label.starts_with("ntdll!wcscmp ")
        && let Ok(words) = &mut expected.0
    {
        words[0] = (words[0] as i32).signum() as u32;
    }
    if label.contains("!wcsstr ") || label.starts_with("ntdll!strstr ") {
        let width = if label.contains("!wcsstr ") { 2 } else { 1 };
        let empty = |at: u32| {
            data[(at - DATA) as usize..(at - DATA) as usize + width]
                .iter()
                .all(|&byte| byte == 0)
        };
        if empty(args[0])
            && empty(args[1])
            && let Ok(words) = &mut expected.0
        {
            words[0] = args[0];
        }
    }
    assert!(
        expected.0.is_ok(),
        "{label}: Wine failed: {:?}, stack {:x?}",
        expected.0,
        (0..4)
            .map(|i| reference
                .cpu
                .read_u32(reference.cpu.reg(adv_cpu::backend::Reg::Esp) + i * 4))
            .collect::<Vec<_>>()
    );
    assert_eq!(actual.0, expected.0, "{label}: {args:x?}");
    assert_eq!(
        adv_nt::native::declines(&native.ext),
        abandons,
        "{label}: valid call declined"
    );
    assert!(
        actual.1 == expected.1,
        "{label}: output differs, args {args:x?}; first difference {:?}",
        actual
            .1
            .iter()
            .zip(&expected.1)
            .enumerate()
            .find(|(_, (a, b))| a != b)
    );
}
fn put(data: &mut [u8], at: u32, width: u32, value: u32) {
    let at = (at - DATA) as usize;
    data[at..at + width as usize].copy_from_slice(&value.to_le_bytes()[..width as usize]);
}

// spec: NAT-BODY-1, NAT-BODY-10
#[test]
fn embedded_adler32_matches_wine() {
    let Some(dist) = dist() else { return };
    for module in [
        "cabinet.dll",
        "dbghelp.dll",
        "opcservices.dll",
        "user32.dll",
        "wininet.dll",
        "windowscodecs.dll",
    ] {
        let (mut native, pe) = machine(&dist, module, true);
        let (mut reference, _) = machine(&dist, module, false);
        let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
            .table()
            .unwrap();
        let entry = pe.image_base + table.named("adler32_z").unwrap().rva;
        assert!(native.sys.redirects.contains_key(&entry));
        for m in [&mut native, &mut reference] {
            prepare(m, entry, 3, false);
        }
        let mut data = vec![0; LEN as usize];
        let mut rng = Rng(0x165667b19e3779f9);
        for byte in &mut data {
            *byte = rng.next() as u8;
        }
        for len in [0, 1, 2, 15, 16, 17, 5551, 5552, 5553, LEN] {
            for seed in [0, 1, 0xfff0fff0, 0xffffffff, rng.next()] {
                same(
                    &mut native,
                    &mut reference,
                    &data,
                    &[seed, DATA, len],
                    module,
                );
            }
        }
        data.fill(255);
        same(
            &mut native,
            &mut reference,
            &data,
            &[u32::MAX, DATA, LEN],
            module,
        );
        for len in [0, 2, 32768] {
            same(
                &mut native,
                &mut reference,
                &data,
                &[u32::MAX, 0, len],
                module,
            );
        }
        // The pinned zlib tests len=1 before the null-buffer initializer.
        // Its byte read faults; direct bodies report that access at the entry.
        let before = adv_nt::native::declines(&native.ext);
        let actual = call(&mut native, &data, &[1, 0, 1]);
        assert!(actual.0.is_err(), "{module}: null one-byte input");
        let fault = &native.sys.latest_cpu_fault.as_ref().unwrap().fault;
        assert_eq!(fault.pc, entry);
        assert_eq!(fault.kind, adv_cpu::backend::FaultKind::Read(0));
        assert_eq!(actual.1, data);
        assert_eq!(adv_nt::native::declines(&native.ext), before);
    }
}

// spec: NAT-BODY-1, NAT-BODY-9
#[test]
fn png_row_family_matches_wine() {
    let Some(dist) = dist() else { return };
    let (mut native, pe) = machine(&dist, "windowscodecs.dll", true);
    let (mut reference, _) = machine(&dist, "windowscodecs.dll", false);
    let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    let mut rng = Rng(0x27d4eb2f165667c5);
    let mut data = vec![0; LEN as usize];
    for name in [
        "png_read_filter_row_sub",
        "png_read_filter_row_up",
        "png_read_filter_row_avg",
        "png_read_filter_row_paeth_1byte_pixel",
        "png_read_filter_row_paeth_multibyte_pixel",
    ] {
        let entry = pe.image_base + table.named(name).unwrap().rva;
        assert!(native.sys.redirects.contains_key(&entry));
        for m in [&mut native, &mut reference] {
            prepare(m, entry, 3, false);
        }
        for depth in [1u32, 2, 4, 8, 16, 24, 32, 48, 64] {
            for len in [depth.div_ceil(8), 17, 10240] {
                for b in &mut data {
                    *b = rng.next() as u8;
                }
                put(&mut data, DATA + 4, 4, len);
                put(&mut data, DATA + 11, 1, depth);
                for previous in [DATA + 0x1000, DATA + 0x4000, DATA + 0x3fff, DATA + 0x4001] {
                    let args = [DATA, DATA + 0x4000, previous];
                    let before = adv_nt::native::declines(&native.ext);
                    let mut actual = call(&mut native, &data, &args);
                    let mut expected = call(&mut reference, &data, &args);
                    // These functions return void; EAX is not part of their ABI.
                    actual.0.as_mut().unwrap()[0] = 0;
                    expected.0.as_mut().unwrap()[0] = 0;
                    assert_eq!(actual, expected, "{name} depth={depth} len={len}");
                    assert_eq!(
                        adv_nt::native::declines(&native.ext),
                        before,
                        "row was not accelerated"
                    );
                }
            }
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-8
#[test]
fn d3dx_versions_share_integer_pixels() {
    let Some(dist) = dist() else { return };
    for version in 24..=43 {
        let module = format!("d3dx9_{version}.dll");
        let (mut native, pe) = machine(&dist, &module, true);
        let (mut reference, _) = machine(&dist, &module, false);
        let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
            .table()
            .unwrap();
        let entry = pe.image_base + table.named("d3dx_load_pixels_from_pixels").unwrap().rva;
        assert!(native.sys.redirects.contains_key(&entry));
        for m in [&mut native, &mut reference] {
            prepare(m, entry, 6, false);
        }
        let mut data = vec![0x39; LEN as usize];
        for (at, id, bytes, alpha) in [(DATA + 128, 1, 4, 8), (DATA + 192, 0, 3, 0)] {
            for (off, val) in [
                (0, id),
                (4, u32::from_le_bytes([alpha, 8, 8, 8])),
                (
                    8,
                    u32::from_le_bytes([if alpha == 0 { 0 } else { 24 }, 16, 8, 0]),
                ),
                (12, bytes),
                (16, 1),
                (20, 1),
                (24, bytes),
                (28, u32::from(alpha != 0)),
                (32, 1),
                (36, 0),
            ] {
                put(&mut data, at + off, 4, val);
            }
        }
        for (at, buffer, pitch) in [(DATA, DATA + 0x4000, 12), (DATA + 64, DATA + 0x1000, 8)] {
            for (off, val) in [
                (0, buffer),
                (4, pitch),
                (8, 0),
                (12, 0),
                (16, 2),
                (20, 2),
                (24, 1),
                (28, 0),
                (32, 0),
                (36, 2),
                (40, 2),
            ] {
                put(&mut data, at + off, 4, val);
            }
        }
        same(
            &mut native,
            &mut reference,
            &data,
            &[DATA, DATA + 128, DATA + 64, DATA + 192, 3, 0xff393939],
            &module,
        );
    }
}

// spec: NAT-BODY-1, NAT-BODY-8
#[test]
fn d3dx_integer_pixels_match_wine_and_preserve_hooks() {
    let Some(dist) = dist() else { return };
    let module = "d3dx9_43.dll";
    let (mut native, pe) = machine(&dist, module, true);
    let (mut reference, _) = machine(&dist, module, false);
    let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    let entry = pe.image_base + table.named("d3dx_load_pixels_from_pixels").unwrap().rva;
    assert!(native.sys.redirects.contains_key(&entry));
    for m in [&mut native, &mut reference] {
        prepare(m, entry, 6, false);
    }
    const DP: u32 = DATA;
    const SP: u32 = DATA + 64;
    const DF: u32 = DATA + 128;
    const SF: u32 = DATA + 192;
    let mut rng = Rng(0xa341316c29c865f0);
    let formats = [
        (3, [0u8, 8, 8, 8], [0u8, 16, 8, 0]),
        (4, [8, 8, 8, 8], [24, 16, 8, 0]),
        (4, [0, 8, 8, 8], [0, 16, 8, 0]),
        (4, [8, 8, 8, 8], [24, 0, 8, 16]),
        (4, [0, 8, 8, 8], [0, 0, 8, 16]),
        (2, [0, 5, 6, 5], [0, 11, 5, 0]),
        (2, [0, 5, 5, 5], [0, 10, 5, 0]),
        (2, [1, 5, 5, 5], [15, 10, 5, 0]),
        (1, [0, 3, 3, 2], [0, 5, 2, 0]),
        (2, [8, 3, 3, 2], [8, 5, 2, 0]),
        (2, [4, 4, 4, 4], [12, 8, 4, 0]),
        (2, [0, 4, 4, 4], [0, 8, 4, 0]),
        (4, [2, 10, 10, 10], [30, 20, 10, 0]),
        (4, [2, 10, 10, 10], [30, 0, 10, 20]),
    ];
    let mut data = vec![0; LEN as usize];
    for (src_id, &(src_bytes, src_bits, src_shift)) in formats.iter().enumerate() {
        for (dst_id, &(dst_bytes, dst_bits, dst_shift)) in formats.iter().enumerate() {
            for filter in 1..=4 {
                for shape in 0..3 {
                    for b in &mut data {
                        *b = rng.next() as u8;
                    }
                    for (at, id, bytes, bits, shifts) in [
                        (SF, src_id, src_bytes, src_bits, src_shift),
                        (DF, dst_id, dst_bytes, dst_bits, dst_shift),
                    ] {
                        for (off, val) in [
                            (0, id as u32),
                            (12, bytes),
                            (16, 1),
                            (20, 1),
                            (24, bytes),
                            (28, u32::from(bits[0] != 0)),
                            (32, 1),
                            (36, 0),
                        ] {
                            put(&mut data, at + off, 4, val);
                        }
                        put(&mut data, at + 4, 4, u32::from_le_bytes(bits));
                        put(&mut data, at + 8, 4, u32::from_le_bytes(shifts));
                    }
                    let width = 1 + rng.next() % 31;
                    let height = 1 + rng.next() % 7;
                    let (dw, dh) = match shape {
                        0 => (width, height),
                        1 => (width + 3, height + 2),
                        _ => (width.div_ceil(2), height.div_ceil(2)),
                    };
                    for (at, buffer, bytes, width, height) in [
                        (SP, DATA + 0x1000, src_bytes, width, height),
                        (DP, DATA + 0x4000, dst_bytes, dw, dh),
                    ] {
                        for (off, val) in [
                            (0, buffer),
                            (4, width * bytes + 7),
                            (8, 0),
                            (12, 0),
                            (16, width),
                            (20, height),
                            (24, 1),
                            (28, 0),
                            (32, 0),
                            (36, width),
                            (40, height),
                        ] {
                            put(&mut data, at + off, 4, val);
                        }
                    }
                    same(
                        &mut native,
                        &mut reference,
                        &data,
                        &[DP, DF, SP, SF, filter, 0],
                        &format!("D3DX {src_id}->{dst_id} filter={filter} shape={shape}"),
                    );
                    if src_bits.iter().all(|&n| n == 0 || n == 8) {
                        let key = 0x7f123456u32;
                        let mut keyed = data.clone();
                        let mut pixel = 0u32;
                        for i in 0..4 {
                            if src_bits[i] != 0 {
                                pixel |= ((key >> (24 - i * 8)) & 255) << src_shift[i];
                            }
                        }
                        put(&mut keyed, DATA + 0x1000, src_bytes, pixel);
                        same(
                            &mut native,
                            &mut reference,
                            &keyed,
                            &[DP, DF, SP, SF, filter, key],
                            "D3DX color key",
                        );
                    }
                }
            }
        }
    }
    let mut large = data.clone();
    for at in [SP, DP] {
        put(&mut large, at + 4, 4, 2560 * 4 + 7);
        put(&mut large, at + 16, 4, 2560);
        put(&mut large, at + 20, 4, 1);
    }
    put(&mut large, DF, 4, 12);
    put(&mut large, DF + 8, 4, u32::from_le_bytes([30, 20, 10, 0]));
    same(
        &mut native,
        &mut reference,
        &large,
        &[DP, DF, SP, SF, 3, 0],
        "D3DX wide row",
    );
    let args = [DP, DF, SP, SF, 3, 0];
    for target in [DATA + 0x1002, DATA + 0x0ffe] {
        let mut input = data.clone();
        put(&mut input, DP, 4, target);
        let before = adv_nt::native::declines(&native.ext);
        let actual = call(&mut native, &input, &args);
        let expected = call(&mut reference, &input, &args);
        assert_eq!(actual, expected, "overlapping pixels preserve traversal");
        assert!(actual.0.is_ok());
        assert_eq!(adv_nt::native::declines(&native.ext), before);
    }
    let helper = pe.image_base + table.named("make_argb_color").unwrap().rva;
    let mut original = [0; 6];
    native.cpu.read(helper, &mut original).unwrap();
    for m in [&mut native, &mut reference] {
        m.cpu
            .write(helper, &[0xb8, 0x78, 0x56, 0x34, 0x12, 0xc3])
            .unwrap();
    }
    let before = adv_nt::native::declines(&native.ext);
    let actual = call(&mut native, &data, &args);
    let expected = call(&mut reference, &data, &args);
    assert_eq!(actual, expected, "patched pixel helper");
    assert!(actual.0.is_ok());
    assert_eq!(adv_nt::native::declines(&native.ext), before);

    let slot = pe
        .imports()
        .unwrap()
        .into_iter()
        .flat_map(|m| m.imports)
        .find_map(|i| {
            matches!(i.name, ImportName::Name { ref name, .. } if name == "memcpy")
                .then_some(pe.image_base + i.iat_rva)
        })
        .unwrap();
    for m in [&mut native, &mut reference] {
        m.cpu.write(helper, &original).unwrap();
        m.cpu
            .write(
                CODE + 0x800,
                &[0x8b, 0x44, 0x24, 0x04, 0xc6, 0x00, 0x5a, 0xc3],
            )
            .unwrap();
        m.cpu.write_u32(slot, CODE + 0x800).unwrap();
    }
    let before = adv_nt::native::declines(&native.ext);
    let actual = call(&mut native, &data, &args);
    let expected = call(&mut reference, &data, &args);
    assert_eq!(actual, expected, "patched copy import");
    assert!(actual.0.is_ok());
    assert_eq!(adv_nt::native::declines(&native.ext), before);
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
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-6
#[test]
fn crt_faults_raise_at_entry_before_writes() {
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
                let before = adv_nt::native::declines(&native.ext);
                let actual = call(&mut native, &data, &args[..argc]);
                let expected = call(&mut reference, &data, &args[..argc]);
                assert!(
                    expected.0.is_err(),
                    "{module}!{name} fault {fault}: {:?}",
                    expected.0
                );
                assert!(actual.0.is_err(), "{module}!{name} fault {fault}");
                let raised = &native.sys.latest_cpu_fault.as_ref().unwrap().fault;
                let bound = if module == "ntdll.dll" && name == "memmove" {
                    address(&pe, "memcpy")
                } else if module == "msvcrt.dll" && name == "memcpy" {
                    address(&pe, "memmove")
                } else {
                    entry
                };
                assert_eq!(raised.pc, bound, "{module}!{name}");
                assert!(matches!(
                    raised.kind,
                    adv_cpu::backend::FaultKind::Read(_) | adv_cpu::backend::FaultKind::Write(_)
                ));
                assert_eq!(actual.1, data, "{module}!{name}: entry fault wrote bytes");
                assert_eq!(adv_nt::native::declines(&native.ext), before);
            }
        }
    }
}

struct CountingGuest<'a> {
    cpu: &'a mut dyn Guest,
    calls: std::cell::Cell<usize>,
    read_bytes: std::cell::Cell<usize>,
    largest_read: std::cell::Cell<usize>,
    lend_tables: bool,
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
        self.read_bytes.set(self.read_bytes.get() + data.len());
        self.largest_read
            .set(self.largest_read.get().max(data.len()));
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
        // Wine's shared NLS file mappings cannot lend a slice.
        if !self.lend_tables && !(DATA..DATA + LEN).contains(&at) {
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
    let body = m.sys.thunks.get(thunk).f.handler;
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, SLOT);
    for (i, arg) in args.iter().enumerate() {
        m.cpu.write_u32(ARGS + i as u32 * 4, *arg).unwrap();
    }
    let mut cpu = CountingGuest {
        cpu: &mut m.cpu,
        calls: std::cell::Cell::new(0),
        read_bytes: std::cell::Cell::new(0),
        largest_read: std::cell::Cell::new(0),
        lend_tables: true,
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
    assert!(
        matches!(
            body(&mut env),
            adv_machine::Outcome::Return(_) | adv_machine::Outcome::Return64(_)
        ),
        "{name} declined"
    );
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

// spec: NAT-BODY-6, NAT-BODY-15
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
}

#[test]
#[ignore = "translated-call microbenchmark"]
fn crt_zlib_translated_call_benchmark() {
    let dist = dist().unwrap();
    for (module, name, len, args) in [
        ("msvcrt.dll", "strlen", 16, vec![DATA]),
        ("msvcrt.dll", "strlen", 256, vec![DATA]),
        ("msvcrt.dll", "strcmp", 32, vec![DATA, DATA]),
        ("msvcrt.dll", "wcslen", 64, vec![DATA]),
        ("windowscodecs.dll", "adler32_z", 64, vec![1, DATA, 64]),
        (
            "windowscodecs.dll",
            "adler32_z",
            65536,
            vec![1, DATA, 65536],
        ),
    ] {
        let (mut m, pe) = machine(&dist, module, true);
        m.set_execution_budget(Some(std::time::Duration::from_secs(120)));
        m.cpu.map(DATA + LEN, 0x10000, Prot::RW).unwrap();
        m.cpu.fill_memory(DATA, 0x41, 65536).unwrap();
        let width = if name == "wcslen" { 2 } else { 1 };
        m.cpu.write(DATA + len * width, &[0, 0]).unwrap();
        let table = m.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
            .table()
            .unwrap();
        let entry = pe.image_base + table.named(name).unwrap().rva;
        m.cpu.write_u32(SLOT, entry).unwrap();
        let mut code = vec![0x57, 0xbf];
        code.extend_from_slice(&1_000_000u32.to_le_bytes());
        let begin = code.len();
        for arg in args.iter().rev() {
            code.push(0x68);
            code.extend_from_slice(&arg.to_le_bytes());
        }
        code.extend_from_slice(&[0xff, 0x15]);
        code.extend_from_slice(&SLOT.to_le_bytes());
        code.extend_from_slice(&[0x83, 0xc4, args.len() as u8 * 4, 0x4f, 0x0f, 0x85]);
        code.extend_from_slice(&((begin as i32) - (code.len() as i32 + 4)).to_le_bytes());
        code.extend_from_slice(&[0x5f, 0xc3]);
        m.cpu.write(CODE, &code).unwrap();
        for round in 0..2 {
            m.spawn("body-benchmark", CODE, &[], 0x10000).unwrap();
            let start = std::time::Instant::now();
            let result = m.run();
            assert!(matches!(result, RunResult::Exited(_)), "{result:?}");
            let elapsed = start.elapsed();
            if round == 1 {
                println!(
                    "{module}!{name} {len}: {:.3} ns/call (1M translated calls)",
                    elapsed.as_nanos() as f64 / 1_000_000.0
                );
            }
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-6
#[test]
fn crt_windows_results_and_page_ends() {
    let Some(dist) = dist() else { return };
    for module in ["ntdll.dll", "msvcrt.dll", "ucrtbase.dll"] {
        let (mut m, pe) = machine(&dist, module, true);
        // Measured on Windows 11 x86: all preserve LastError; only strncmp's
        // subtraction versus sign differs among these modules.
        for (name, expected) in [
            ("strcmp", -1i32),
            ("strncmp", if module == "ucrtbase.dll" { -1 } else { -2 }),
            ("wcscmp", -1),
            ("wcsncmp", -514),
        ] {
            let width = if name.starts_with("wcs") { 2 } else { 1 };
            let mut data = vec![0; LEN as usize];
            put(&mut data, DATA, width, if width == 1 { 65 } else { 0x141 });
            put(
                &mut data,
                DATA + 0x1000,
                width,
                if width == 1 { 67 } else { 0x343 },
            );
            let argc = if name.ends_with("ncmp") { 3 } else { 2 };
            prepare(&mut m, address(&pe, name), argc, false);
            let result = call(&mut m, &data, &[DATA, DATA + 0x1000, 3][..argc])
                .0
                .unwrap();
            assert_eq!(result[0], expected as u32, "{module}!{name}");
            assert_eq!(result[7], 0x1234abcd);
        }
        // A byte match at offset one overlaps the first valid UTF-16 match
        // at offset two; wide search must retain that aligned occurrence.
        let mut data = vec![0; LEN as usize];
        for (at, value) in [
            (DATA, 0x4142),
            (DATA + 2, 0x4141),
            (DATA + 4, 0x4141),
            (DATA + 0x1000, 0x4141),
            (DATA + 0x1002, 0x4141),
        ] {
            put(&mut data, at, 2, value);
        }
        prepare(&mut m, address(&pe, "wcsstr"), 2, false);
        assert_eq!(
            call(&mut m, &data, &[DATA, DATA + 0x1000]).0.unwrap()[0],
            DATA + 2
        );
        for width in [1, 2] {
            for offset in 0..16 {
                let mut data = vec![0x41; LEN as usize];
                let end = DATA + LEN - width - offset;
                let a = end - 64 * width;
                let b = DATA + 0x1000;
                for i in 0..64 {
                    put(
                        &mut data,
                        b + i * width,
                        width,
                        if width == 1 { 0x41 } else { 0x4141 },
                    );
                }
                put(&mut data, end, width, 0);
                put(&mut data, b + 64 * width, width, 0);
                for (name, args, expected) in if width == 1 {
                    vec![
                        ("strlen", vec![a], 64),
                        ("strnlen", vec![a, 100], 64),
                        ("strcmp", vec![a, b], 0),
                        ("strncmp", vec![a, b, 100], 0),
                        ("strchr", vec![a, 0], end),
                        ("strrchr", vec![a, 0], end),
                        ("strstr", vec![a, b], a),
                        ("strstr", vec![end, end], end),
                        ("strcpy", vec![b, a], b),
                        ("strcat", vec![b + 64, a], b + 64),
                    ]
                } else {
                    vec![
                        ("wcslen", vec![a], 64),
                        ("wcsnlen", vec![a, 100], 64),
                        ("wcscmp", vec![a, b], 0),
                        ("wcsncmp", vec![a, b, 100], 0),
                        ("wcschr", vec![a, 0], end),
                        ("wcsrchr", vec![a, 0], end),
                        ("wcsstr", vec![a, b], a),
                        ("wcsstr", vec![end, end], end),
                        ("wcscpy", vec![b, a], b),
                        ("wcscat", vec![b + 128, a], b + 128),
                    ]
                } {
                    prepare(&mut m, address(&pe, name), args.len(), false);
                    let result = call(&mut m, &data, &args);
                    let words = result
                        .0
                        .unwrap_or_else(|error| panic!("{module}!{name} offset {offset}: {error}"));
                    assert_eq!(words[0], expected, "{module}!{name} offset {offset}");
                    assert_eq!(words[7], 0x1234abcd);
                    if name.ends_with("cpy") || name.ends_with("cat") {
                        let begin = (args[0] - DATA) as usize;
                        assert_eq!(
                            &result.1[begin..begin + 65 * width as usize],
                            &data[(a - DATA) as usize..(a - DATA + 65 * width) as usize]
                        );
                    }
                }
                // Comparisons stop at the first difference before an unmapped
                // page, even when neither operand has a terminator there.
                put(&mut data, end, width, 0x41);
                put(&mut data, b + 64 * width, width, 0x43);
                let name = if width == 1 { "strcmp" } else { "wcscmp" };
                prepare(&mut m, address(&pe, name), 2, false);
                assert_eq!(call(&mut m, &data, &[a, b]).0.unwrap()[0], (-1i32) as u32);
            }
        }
    }
}

// spec: NAT-BODY-8, NAT-BODY-16
#[test]
fn d3dx_all_formats_filters_and_overlap() {
    let Some(dist) = dist() else { return };
    let (mut native, pe) = machine(&dist, "d3dx9_43.dll", true);
    let (mut reference, _) = machine(&dist, "d3dx9_43.dll", false);
    let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    let getter = table.named("get_d3dx_pixel_format_info").unwrap();
    let formats = pe
        .relocations()
        .unwrap()
        .into_iter()
        .filter(|r| *r >= getter.rva && *r < getter.rva + getter.size)
        .find_map(|r| {
            let at = native.cpu.read_u32(pe.image_base + r).ok()?;
            let mut b = [0; 40];
            native.cpu.read(at, &mut b).ok()?;
            (b[..12] == [0, 0, 0, 0, 0, 8, 8, 8, 0, 16, 8, 0]).then_some(at)
        })
        .unwrap();
    let entry = pe.image_base + table.named("d3dx_load_pixels_from_pixels").unwrap().rva;
    // The reference conversion owns temporary buffers. A guest bump allocator
    // supplies the same malloc/free contract without loading another CRT.
    const HEAP: u32 = 0x20000000;
    const BUMP: u32 = SLOT + 0x200;
    let mut crt =
        Pe::parse(&std::fs::read(dist.join("lib/wine/i386-windows/ucrtbase.dll")).unwrap())
            .unwrap();
    crt.rebase(0x30000000).unwrap();
    crt.image_base = 0x30000000;
    let mut user =
        Pe::parse(&std::fs::read(dist.join("lib/wine/i386-windows/user32.dll")).unwrap()).unwrap();
    user.rebase(0x40000000).unwrap();
    user.image_base = 0x40000000;
    for m in [&mut native, &mut reference] {
        map(m, &crt);
        map(m, &user);
        for import in pe.imports().unwrap().into_iter().flat_map(|m| m.imports) {
            if let ImportName::Name { name, .. } = import.name
                && m.cpu.read_u32(pe.image_base + import.iat_rva).unwrap() == CODE + 0xf00
                && let Some(target) =
                    [&crt, &user].into_iter().find_map(|library| {
                        library.exports().unwrap().into_iter().find_map(|e| {
                            match (e.name, e.target) {
                                (Some(n), ExportTarget::Rva(rva)) if n == name => {
                                    Some(library.image_base + rva)
                                }
                                _ => None,
                            }
                        })
                    })
            {
                m.cpu
                    .write_u32(pe.image_base + import.iat_rva, target)
                    .unwrap();
            }
        }
        m.cpu.map(HEAP, 0x100000, Prot::RW).unwrap();
        let mut malloc = vec![0xa1];
        malloc.extend_from_slice(&BUMP.to_le_bytes());
        malloc.extend_from_slice(&[
            0x8b, 0x4c, 0x24, 4, 0x83, 0xc1, 15, 0x83, 0xe1, 0xf0, 0x01, 0x0d,
        ]);
        malloc.extend_from_slice(&BUMP.to_le_bytes());
        malloc.push(0xc3);
        m.cpu.write(CODE + 0xa00, &malloc).unwrap();
        m.cpu.write(CODE + 0xa40, &[0xc3]).unwrap();
        for imported in pe.imports().unwrap().into_iter().flat_map(|m| m.imports) {
            if let ImportName::Name { name, .. } = imported.name {
                let target = match name.as_str() {
                    "malloc" => CODE + 0xa00,
                    "free" => CODE + 0xa40,
                    _ => continue,
                };
                m.cpu
                    .write_u32(pe.image_base + imported.iat_rva, target)
                    .unwrap();
            }
        }
        prepare(m, entry, 6, false);
    }
    let mut rng = Rng(0x735a4d2cba1968f0);
    for id in 0..=58 {
        for (sid, did) in [(id, 1), (1, id), (id, id)] {
            let mut sf = [0; 40];
            let mut df = [0; 40];
            native.cpu.read(formats + sid * 40, &mut sf).unwrap();
            native.cpu.read(formats + did * 40, &mut df).unwrap();
            for filter in 1..=5 {
                for shape in 0..3 {
                    let mut data: Vec<_> = (0..LEN).map(|_| rng.next() as u8).collect();
                    let dw: u32 = if shape == 0 { 4 } else { 2 };
                    for (at, f) in [(DATA + 128, sf), (DATA + 192, df)] {
                        data[(at - DATA) as usize..(at - DATA) as usize + 40].copy_from_slice(&f);
                    }
                    for (at, buffer, w, h, f) in [
                        (DATA, DATA + 0x1000, 4, 4, sf),
                        (DATA + 64, DATA + 0x4000, dw, dw, df),
                    ] {
                        let bw = u32::from_le_bytes(f[16..20].try_into().unwrap());
                        let bh = u32::from_le_bytes(f[20..24].try_into().unwrap());
                        let block = u32::from_le_bytes(f[24..28].try_into().unwrap());
                        let pitch = w.div_ceil(bw) * block + 8;
                        let depth = if shape == 2 {
                            if at == DATA { 4 } else { 2 }
                        } else {
                            1
                        };
                        for (off, value) in [
                            (0, buffer),
                            (4, pitch),
                            (8, pitch * h.div_ceil(bh)),
                            (12, DATA + 0x6000),
                            (16, w),
                            (20, h),
                            (24, depth),
                            (28, 0),
                            (32, 0),
                            (36, w),
                            (40, h),
                        ] {
                            put(&mut data, at + off, 4, value);
                        }
                    }
                    if sf[32..36] == 3u32.to_le_bytes() {
                        // Include finite, negative, infinity and NaN float encodings.
                        let bytes = u32::from_le_bytes(sf[12..16].try_into().unwrap());
                        for y in 0..4 {
                            for x in 0..4 {
                                let p = 0x1000 + y * (4 * bytes + 8) + x * bytes;
                                for c in 0..4 {
                                    let bits = sf[4 + c];
                                    if bits == 0 {
                                        continue;
                                    }
                                    let shift = sf[8 + c] as usize / 8;
                                    let values = [
                                        0u32, 0x3f000000, 0xbf800000, 0x7fc00000, 0x7f800000,
                                        0xff800000,
                                    ];
                                    let value = if bits == 16 {
                                        [0u32, 0x3800, 0xbc00, 0x7e00, 0x7c00, 0xfc00]
                                            [(x + y + c as u32) as usize % 6]
                                    } else {
                                        values[(x + y + c as u32) as usize % 6]
                                    };
                                    data[p as usize + shift
                                        ..p as usize + shift + bits as usize / 8]
                                        .copy_from_slice(&value.to_le_bytes()[..bits as usize / 8]);
                                }
                            }
                        }
                    }
                    let args = [
                        DATA + 64,
                        DATA + 192,
                        DATA,
                        DATA + 128,
                        filter,
                        if shape == 0 { 0 } else { 0x7f123456 },
                    ];
                    for m in [&mut native, &mut reference] {
                        m.cpu.write_u32(BUMP, HEAP).unwrap();
                    }
                    same(
                        &mut native,
                        &mut reference,
                        &data,
                        &args,
                        &format!("D3DX formats {sid}->{did} filter {filter} shape {shape}"),
                    );
                }
            }
        }
    }
}

fn translated_loop(m: &mut Machine<Cpu>, entry: u32, args: &[u32], iterations: u32) -> f64 {
    let mut code = vec![0x55, 0xbd];
    code.extend_from_slice(&iterations.to_le_bytes());
    let start = code.len();
    for arg in args.iter().rev() {
        code.push(0x68);
        code.extend_from_slice(&arg.to_le_bytes());
    }
    code.push(0xe8);
    let next = CODE + code.len() as u32 + 4;
    code.extend_from_slice(&entry.wrapping_sub(next).to_le_bytes());
    code.extend_from_slice(&[0x83, 0xc4, (args.len() * 4) as u8, 0x4d, 0x75]);
    code.push((start as isize - code.len() as isize - 1) as u8);
    code.extend_from_slice(&[0x5d, 0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &code).unwrap();
    m.spawn("pixel-bench", CODE, &[], 0x10000).unwrap();
    let start = std::time::Instant::now();
    let result = m.run();
    let seconds = start.elapsed().as_secs_f64();
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    seconds * 1e9 / f64::from(iterations)
}

// spec: NAT-BODY-8, NAT-BODY-9
#[test]
#[ignore = "translated pixel micro-benchmark; nextest enforces a timeout"]
fn translated_pe_pixel_microbenchmark() {
    let Some(dist) = dist() else { return };
    for module in ["d3dx9_43.dll", "windowscodecs.dll"] {
        let (mut m, pe) = machine(&dist, module, true);
        let table = m.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
            .table()
            .unwrap();
        let (name, args, n) = if module.starts_with("d3dx") {
            let source = 0x20000000;
            let target = 0x21000000;
            for at in [source, target] {
                m.cpu.map(at, 0x40000, Prot::RW).unwrap();
                m.cpu.write(at, &vec![0x7f; 0x40000]).unwrap();
            }
            let mut data = vec![0; LEN as usize];
            for (at, buffer) in [(DATA, target), (DATA + 64, source)] {
                for (off, value) in [
                    (0, buffer),
                    (4, 1024),
                    (8, 0x40000),
                    (12, 0),
                    (16, 256),
                    (20, 256),
                    (24, 1),
                    (28, 0),
                    (32, 0),
                    (36, 256),
                    (40, 256),
                ] {
                    put(&mut data, at + off, 4, value);
                }
            }
            for (at, id, bits, shifts) in [
                (DATA + 128, 3, [8u8; 4], [24u8, 0, 8, 16]),
                (DATA + 192, 1, [8u8; 4], [24u8, 16, 8, 0]),
            ] {
                for (off, value) in [
                    (0, id),
                    (4, u32::from_le_bytes(bits)),
                    (8, u32::from_le_bytes(shifts)),
                    (12, 4),
                    (16, 1),
                    (20, 1),
                    (24, 4),
                    (28, 1),
                    (32, 1),
                    (36, 0),
                ] {
                    put(&mut data, at + off, 4, value);
                }
            }
            m.cpu.write(DATA, &data).unwrap();
            (
                "d3dx_load_pixels_from_pixels",
                vec![DATA, DATA + 128, DATA + 64, DATA + 192, 2, 0],
                1000,
            )
        } else {
            m.cpu.write(DATA, &vec![0x71; LEN as usize]).unwrap();
            m.cpu.write_u32(DATA + 4, 1024).unwrap();
            m.cpu.write(DATA + 11, &[32]).unwrap();
            (
                "png_read_filter_row_paeth_multibyte_pixel",
                vec![DATA, DATA + 0x1000, DATA + 0x2000],
                10000,
            )
        };
        let entry = pe.image_base + table.named(name).unwrap().rva;
        translated_loop(&mut m, entry, &args, 10);
        let mut samples = [0.0; 5];
        for value in &mut samples {
            *value = translated_loop(&mut m, entry, &args, n);
        }
        samples.sort_by(f64::total_cmp);
        eprintln!(
            "{name}: {:.3} ns/call (median of 5, {:.3}..{:.3})",
            samples[2], samples[0], samples[4]
        );
    }
}

// spec: NAT-BODY-8, NAT-BODY-9, NAT-BODY-16
#[test]
fn pixel_bodies_fault_at_entry() {
    let Some(dist) = dist() else { return };
    for (module, name, argc) in [
        (
            "windowscodecs.dll",
            "png_read_filter_row_paeth_multibyte_pixel",
            3,
        ),
        ("d3dx9_43.dll", "d3dx_load_pixels_from_pixels", 6),
    ] {
        for write in [false, true] {
            let (mut m, pe) = machine(&dist, module, true);
            let table = m.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
                .table()
                .unwrap();
            let entry = pe.image_base + table.named(name).unwrap().rva;
            prepare(&mut m, entry, argc, false);
            let mut data = vec![0x71; LEN as usize];
            let mut args = if argc == 3 {
                put(&mut data, DATA + 4, 4, 32);
                put(&mut data, DATA + 11, 1, 32);
                vec![DATA, DATA + 0x1000, DATA + 0x2000]
            } else {
                for (at, buffer) in [(DATA, DATA + 0x1000), (DATA + 64, DATA + 0x2000)] {
                    for (off, value) in [
                        (0, buffer),
                        (4, 16),
                        (8, 16),
                        (12, 0),
                        (16, 4),
                        (20, 1),
                        (24, 1),
                        (28, 0),
                        (32, 0),
                        (36, 4),
                        (40, 1),
                    ] {
                        put(&mut data, at + off, 4, value);
                    }
                }
                for at in [DATA + 128, DATA + 192] {
                    for (off, value) in [
                        (0, 1),
                        (4, 0x08080808),
                        (8, 0x00081018),
                        (12, 4),
                        (16, 1),
                        (20, 1),
                        (24, 4),
                        (28, 1),
                        (32, 1),
                        (36, 0),
                    ] {
                        put(&mut data, at + off, 4, value);
                    }
                }
                vec![DATA, DATA + 128, DATA + 64, DATA + 192, 2, 0]
            };
            for _ in 0..2 {
                assert!(call(&mut m, &data, &args).0.is_ok());
            }
            if write {
                if argc == 3 {
                    args[1] = 0;
                } else {
                    put(&mut data, DATA, 4, 0);
                }
            } else {
                args[0] = 0;
            }
            let (result, bytes) = call(&mut m, &data, &args);
            assert!(result.is_err());
            assert_eq!(bytes, data);
            let fault = &m.sys.latest_cpu_fault.as_ref().unwrap().fault;
            assert_eq!(fault.pc, entry);
            assert_eq!(
                fault.kind,
                if write {
                    adv_cpu::backend::FaultKind::Write(0)
                } else {
                    adv_cpu::backend::FaultKind::Read(if argc == 3 { 4 } else { 0 })
                }
            );
            assert_eq!(adv_nt::native::declines(&m.ext), 0);
        }
    }
}

// spec: NAT-BODY-9, NAT-BODY-16
#[test]
fn png_metadata_overlap_keeps_read_order() {
    let Some(dist) = dist() else { return };
    let (mut native, pe) = machine(&dist, "windowscodecs.dll", true);
    let (mut reference, _) = machine(&dist, "windowscodecs.dll", false);
    let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    for name in [
        "png_read_filter_row_sub",
        "png_read_filter_row_up",
        "png_read_filter_row_avg",
        "png_read_filter_row_paeth_1byte_pixel",
        "png_read_filter_row_paeth_multibyte_pixel",
    ] {
        let entry = pe.image_base + table.named(name).unwrap().rva;
        for m in [&mut native, &mut reference] {
            prepare(m, entry, 3, false);
        }
        let mut data = vec![0x71; LEN as usize];
        put(&mut data, DATA + 4, 4, 32);
        put(&mut data, DATA + 11, 1, 32);
        put(&mut data, DATA + 0x1000, 4, 1);
        let args = [DATA, DATA + 4, DATA + 0x1000];
        let mut actual = call(&mut native, &data, &args);
        let mut expected = call(&mut reference, &data, &args);
        actual.0.as_mut().unwrap()[0] = 0;
        expected.0.as_mut().unwrap()[0] = 0;
        assert_eq!(actual, expected, "{name}");
        assert_eq!(adv_nt::native::declines(&native.ext), 0);
    }
}

// spec: NAT-BODY-8, NAT-BODY-16
#[test]
fn d3dx_float_controls_match_wine() {
    let Some(dist) = dist() else { return };
    let (mut native, pe) = machine(&dist, "d3dx9_43.dll", true);
    let (mut reference, _) = machine(&dist, "d3dx9_43.dll", false);
    let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    let entry = pe.image_base + table.named("d3dx_load_pixels_from_pixels").unwrap().rva;
    for hilo in [true, false] {
        let mut data = vec![0x71; LEN as usize];
        for (at, buffer, width, height, bytes) in [
            (DATA, DATA + 0x1000, 2, 2, if hilo { 2 } else { 16 }),
            (
                DATA + 64,
                DATA + 0x4000,
                if hilo { 2 } else { 1 },
                if hilo { 2 } else { 1 },
                16,
            ),
        ] {
            for (off, val) in [
                (0, buffer),
                (4, width * bytes),
                (8, width * height * bytes),
                (12, 0),
                (16, width),
                (20, height),
                (24, 1),
                (28, 0),
                (32, 0),
                (36, width),
                (40, height),
            ] {
                put(&mut data, at + off, 4, val);
            }
        }
        for at in [DATA + 128, DATA + 192] {
            for (off, val) in [
                (0, 41),
                (4, 0x20202020),
                (8, 0x40200060),
                (12, 16),
                (16, 1),
                (20, 1),
                (24, 16),
                (28, 3),
                (32, 3),
                (36, 0),
            ] {
                put(&mut data, at + off, 4, val);
            }
        }
        if hilo {
            for (off, val) in [
                (0, 50),
                (4, 0x00080800),
                (8, 0x00080000),
                (12, 2),
                (24, 2),
                (28, 0),
                (32, 6),
            ] {
                put(&mut data, DATA + 128 + off, 4, val);
            }
            for (i, pixel) in [0x3cd8u32, 0x0a64, 0, 0x0101].into_iter().enumerate() {
                put(&mut data, DATA + 0x1000 + i as u32 * 2, 2, pixel);
            }
        } else {
            for (i, pixel) in [
                [0x00800000u32, 1, 0x3f800000, 0x3dcc_cccd],
                [0, 1, 0x3f800000, 0x3e4c_cccd],
                [0, 1, 0x3f000000, 0x3e99_999a],
                [0, 1, 0x3e800000, 0x3ecc_cccd],
            ]
            .into_iter()
            .enumerate()
            {
                for (c, value) in pixel.into_iter().enumerate() {
                    put(
                        &mut data,
                        DATA + 0x1000 + i as u32 * 16 + c as u32 * 4,
                        4,
                        value,
                    );
                }
            }
        }
        for mxcsr in [0x1f80u32, 0x1fc0, 0x9f80, 0x9fc0, 0x3f80, 0x5f80, 0x7f80] {
            for cw in [0x037fu16, 0x077f, 0x0b7f, 0x0f7f] {
                for m in [&mut native, &mut reference] {
                    prepare(m, entry, 6, false);
                    m.cpu.write_u32(SLOT + 0x300, mxcsr).unwrap();
                    m.cpu.write(SLOT + 0x304, &cw.to_le_bytes()).unwrap();
                    let mut code = [0; 256];
                    m.cpu.read(CODE, &mut code).unwrap();
                    let mut prefix = vec![0x0f, 0xae, 0x15];
                    prefix.extend_from_slice(&(SLOT + 0x300).to_le_bytes());
                    prefix.extend_from_slice(&[0xd9, 0x2d]);
                    prefix.extend_from_slice(&(SLOT + 0x304).to_le_bytes());
                    prefix.extend_from_slice(&code);
                    m.cpu.write(CODE, &prefix).unwrap();
                }
                same(
                    &mut native,
                    &mut reference,
                    &data,
                    &[
                        DATA + 64,
                        DATA + 192,
                        DATA,
                        DATA + 128,
                        if hilo { 2 } else { 5 },
                        0,
                    ],
                    &format!("D3DX float control {mxcsr:x}/{cw:x} hilo {hilo}"),
                );
            }
        }
        if !hilo {
            for m in [&mut native, &mut reference] {
                prepare(m, entry, 6, false);
                m.cpu.write_u32(SLOT + 0x300, 0x0f80).unwrap();
                let mut code = [0; 256];
                m.cpu.read(CODE, &mut code).unwrap();
                let mut prefix = vec![0x0f, 0xae, 0x15];
                prefix.extend_from_slice(&(SLOT + 0x300).to_le_bytes());
                prefix.extend_from_slice(&code);
                m.cpu.write(CODE, &prefix).unwrap();
            }
            let args = [DATA + 64, DATA + 192, DATA, DATA + 128, 5, 0];
            let actual = call(&mut native, &data, &args);
            let expected = call(&mut reference, &data, &args);
            assert!(actual.0.is_err() && expected.0.is_err());
            assert_eq!(&actual.1[0x4000..0x4010], &expected.1[0x4000..0x4010]);
            let fault = &native.sys.latest_cpu_fault.as_ref().unwrap().fault;
            assert_eq!(fault.pc, entry);
            assert_eq!(fault.kind, adv_cpu::backend::FaultKind::Trap(19));
            assert_eq!(adv_nt::native::declines(&native.ext), 0);
        }
    }
}

// spec: NAT-BODY-9, NAT-BODY-16
#[test]
fn png_short_rows_and_wrapped_average_complete() {
    let Some(dist) = dist() else { return };
    for name in [
        "png_read_filter_row_sub",
        "png_read_filter_row_up",
        "png_read_filter_row_avg",
        "png_read_filter_row_paeth_1byte_pixel",
        "png_read_filter_row_paeth_multibyte_pixel",
    ] {
        for len in [0, 1, 3, 7] {
            let (mut native, pe) = machine(&dist, "windowscodecs.dll", true);
            let (mut reference, _) = machine(&dist, "windowscodecs.dll", false);
            let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
                .table()
                .unwrap();
            let entry = pe.image_base + table.named(name).unwrap().rva;
            for m in [&mut native, &mut reference] {
                prepare(m, entry, 3, false);
            }
            let mut data = vec![0x71; LEN as usize];
            put(&mut data, DATA + 4, 4, len);
            put(&mut data, DATA + 11, 1, 64);
            let args = [DATA, DATA + 0x1000, DATA + 0x2000];
            let mut actual = call(&mut native, &data, &args);
            let mut expected = call(&mut reference, &data, &args);
            if name == "png_read_filter_row_avg" {
                assert!(actual.0.is_err() && expected.0.is_err());
                assert_eq!(actual.1, data);
                let fault = &native.sys.latest_cpu_fault.as_ref().unwrap().fault;
                assert_eq!(fault.pc, entry);
                assert_eq!(fault.kind, adv_cpu::backend::FaultKind::Read(DATA + LEN));
            } else {
                actual.0.as_mut().unwrap()[0] = 0;
                expected.0.as_mut().unwrap()[0] = 0;
                assert_eq!(actual, expected, "{name} len {len}");
            }
            assert_eq!(adv_nt::native::declines(&native.ext), 0);
        }
    }
}

// spec: NAT-BODY-8, NAT-BODY-16
#[test]
fn d3dx_block_rounding_wraps_and_zero_divisors_fault() {
    let Some(dist) = dist() else { return };
    let (mut native, pe) = machine(&dist, "d3dx9_43.dll", true);
    let (mut reference, _) = machine(&dist, "d3dx9_43.dll", false);
    let table = native.ext.get::<adv_nt::identity::Images>().0[&pe.image_base]
        .table()
        .unwrap();
    let entry = pe.image_base + table.named("d3dx_load_pixels_from_pixels").unwrap().rva;
    for m in [&mut native, &mut reference] {
        prepare(m, entry, 6, false);
    }
    let mut data = vec![0x71; LEN as usize];
    for (at, buffer) in [(DATA, DATA + 0x1000), (DATA + 64, DATA + 0x2000)] {
        for (off, value) in [
            (0, buffer),
            (4, 4),
            (8, 4),
            (12, 0),
            (16, 1),
            (20, 1),
            (24, 1),
            (28, 0),
            (32, 0),
            (36, 1),
            (40, 1),
        ] {
            put(&mut data, at + off, 4, value);
        }
    }
    for at in [DATA + 128, DATA + 192] {
        for (off, value) in [
            (0, 1),
            (4, 0x08080808),
            (8, 0x00081018),
            (12, 4),
            (16, 1),
            (20, 1),
            (24, 4),
            (28, 1),
            (32, 1),
            (36, 0),
        ] {
            put(&mut data, at + off, 4, value);
        }
    }
    let args = [DATA, DATA + 128, DATA + 64, DATA + 192, 2, 0];
    same(
        &mut native,
        &mut reference,
        &data,
        &args,
        "D3DX ordinary block copy",
    );
    // UINT addition in copy_pixels rounds this width down to zero blocks.
    for at in [DATA, DATA + 64] {
        put(&mut data, at + 16, 4, u32::MAX);
    }
    for at in [DATA + 128, DATA + 192] {
        put(&mut data, at + 16, 4, u32::MAX);
    }
    same(
        &mut native,
        &mut reference,
        &data,
        &args,
        "D3DX wrapped block count",
    );
    put(&mut data, DATA + 128 + 16, 4, 0);
    let actual = call(&mut native, &data, &args);
    let expected = call(&mut reference, &data, &args);
    assert!(actual.0.is_err() && expected.0.is_err());
    assert_eq!(actual.1, data);
    let fault = &native.sys.latest_cpu_fault.as_ref().unwrap().fault;
    assert_eq!(fault.pc, entry);
    assert_eq!(fault.kind, adv_cpu::backend::FaultKind::IntegerDivideByZero);
    assert_eq!(adv_nt::native::declines(&native.ext), 0);
}
