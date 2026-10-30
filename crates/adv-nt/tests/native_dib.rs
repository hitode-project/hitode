//! Calls the pinned ELF primitives in the interpreter and through native bodies.
use adv_cpu::{
    Cpu,
    backend::{Guest, GuestExt, Prot},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_nt::dispatch::Platform;
#[cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
use std::path::{Path, PathBuf};

#[path = "../../adv-unix/src/elf.rs"]
#[allow(dead_code)]
mod elf;
#[path = "../src/native/dib_layout.rs"]
mod layout;
use layout::{dib as d, *};

const BASE: u32 = 0x1000_0000;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x500000;
const LEN: u32 = 0x20000;
const DST: u32 = DATA;
const SRC: u32 = DATA + 0x100;
const BRUSH: u32 = DATA + 0x200;
const RECTS: u32 = DATA + 0x300;
const ORIGIN: u32 = DATA + 0x400;
const PARAMS: u32 = DATA + 0x500;
const EXTRA: u32 = DATA + 0x800;
const PALETTE: u32 = DATA + 0x1000;
const DEST_BITS: u32 = DATA + 0x4000;
const SOURCE_BITS: u32 = DATA + 0x8000;
const MASK_BITS: u32 = DATA + 0xc000;
const RESULT: u32 = DATA + 0x18000;

fn dist() -> Option<PathBuf> {
    std::env::var_os("ADV_WINE_DIST").map(PathBuf::from)
}
fn set(data: &mut [u8], base: u32, offset: usize, value: u32) {
    let at = (base - DATA) as usize + offset;
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn at(table: &str, name: &str) -> u32 {
    BASE + u32::from_str_radix(
        table
            .lines()
            .skip(1)
            .find_map(|row| {
                let fields: Vec<_> = row.split('\t').collect();
                (fields[3] == name).then_some(fields[0])
            })
            .unwrap_or_else(|| panic!("missing {name}")),
        16,
    )
    .unwrap()
}
fn machine(dist: &Path, native: bool, patch: Option<u32>) -> Machine<Cpu> {
    let mut m = Machine::new(
        Cpu::new(PARK_ADDR, native).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    let pe =
        adv_pe::Pe::parse(&std::fs::read(dist.join("lib/wine/i386-windows/ntdll.dll")).unwrap())
            .unwrap();
    adv_nt::image::map(
        &mut m.cpu,
        &mut m.sys.vm,
        &mut adv_nt::memory::Memory::default(),
        &pe,
        pe.image_base,
    )
    .unwrap();
    let exports = pe.exports().unwrap();
    let path = dist.join("lib/wine/i386-unix/win32u.so");
    let object = elf::parse(&std::fs::read(&path).unwrap()).unwrap();
    assert!(m.sys.vm.reserve_at(BASE, object.image.len() as u32));
    m.cpu
        .map(BASE, object.image.len() as u32, Prot::RWX)
        .unwrap();
    m.cpu.write(BASE, &object.image).unwrap();
    m.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    m.cpu.map(DATA, LEN, Prot::RW).unwrap();
    m.cpu.write(CODE + 0xf00, &[0x0f, 0x0b]).unwrap();
    let mut relocations = Vec::new();
    for r in &object.relocations {
        let address = BASE + r.offset;
        let word = m.cpu.read_u32(address).unwrap();
        let symbol = &object.symbols[r.symbol as usize];
        let value = if symbol.defined {
            BASE + symbol.value
        } else {
            exports
                .iter()
                .find_map(|e| match (&e.name, &e.target) {
                    (Some(name), adv_pe::ExportTarget::Rva(rva)) if name == &symbol.name => {
                        Some(pe.image_base + rva)
                    }
                    _ => None,
                })
                .unwrap_or(CODE + 0xf00)
        };
        let value = match r.kind {
            elf::R_386_RELATIVE => word.wrapping_add(BASE),
            elf::R_386_32 => word.wrapping_add(value),
            elf::R_386_PC32 => word.wrapping_add(value).wrapping_sub(address),
            elf::R_386_GLOB_DAT | elf::R_386_JMP_SLOT => value,
            _ => continue,
        };
        m.cpu.write_u32(address, value).unwrap();
        relocations.push(r.offset);
    }
    relocations.sort_unstable();
    if let Some(patch) = patch {
        m.cpu.write(patch, &[0xcc]).unwrap();
    }
    let mut env = adv_machine::Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        context: 0,
        thunk: 0,
    };
    adv_nt::identity::note_unix(&mut env, &path, BASE, &object.image, &relocations);
    m
}
fn execute(
    m: &mut Machine<Cpu>,
    entry: u32,
    args: &[u32],
    data: &[u8],
    returns: bool,
    mxcsr: Option<u32>,
) -> (RunResult, Vec<u8>) {
    m.cpu.write(DATA, data).unwrap();
    let mut code = Vec::new();
    if let Some(control) = mxcsr {
        m.cpu.write_u32(RESULT + 0x100, control).unwrap();
        code.extend_from_slice(&[0x0f, 0xae, 0x15]);
        code.extend_from_slice(&(RESULT + 0x100).to_le_bytes());
    }
    // Preserve the ABI's nonvolatile registers and record their post-call values.
    code.extend_from_slice(&[0x53, 0x55, 0x56, 0x57]);
    for (op, value) in [
        (0xbb, 0x12345678u32),
        (0xbd, 0x23456789),
        (0xbe, 0x3456789a),
        (0xbf, 0x456789ab),
    ] {
        code.push(op);
        code.extend_from_slice(&value.to_le_bytes());
    }
    for arg in args.iter().rev() {
        code.push(0x68);
        code.extend_from_slice(&arg.to_le_bytes());
    }
    code.push(0xe8);
    let next = CODE + code.len() as u32 + 4;
    code.extend_from_slice(&entry.wrapping_sub(next).to_le_bytes());
    code.extend_from_slice(&[0x83, 0xc4, args.len() as u8 * 4]);
    if returns {
        code.push(0xa3);
        code.extend_from_slice(&RESULT.to_le_bytes());
    }
    for (modrm, offset) in [(0x1d, 4), (0x2d, 8), (0x35, 12), (0x3d, 16), (0x25, 20)] {
        code.extend_from_slice(&[0x89, modrm]);
        code.extend_from_slice(&(RESULT + offset).to_le_bytes());
    }
    code.extend_from_slice(&[0x5f, 0x5e, 0x5d, 0x5b, 0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &code).unwrap();
    m.spawn("dib", CODE, &[], 0x10000).unwrap();
    let result = m.run();
    let mut bytes = vec![0; LEN as usize];
    m.cpu.read(DATA, &mut bytes).unwrap();
    (result, bytes)
}
fn call(m: &mut Machine<Cpu>, entry: u32, args: &[u32], data: &[u8], returns: bool) -> Vec<u8> {
    let (result, bytes) = execute(m, entry, args, data, returns, None);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    bytes
}
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}
const FORMATS: [(&str, u32, u32); 8] = [
    ("8888", 32, FUNCS_8888),
    ("32", 32, FUNCS_32),
    ("24", 24, FUNCS_24),
    ("555", 16, FUNCS_555),
    ("16", 16, FUNCS_16),
    ("8", 8, FUNCS_8),
    ("4", 4, FUNCS_4),
    ("1", 1, FUNCS_1),
];
fn dib(data: &mut [u8], address: u32, format: usize, bits: u32, seed: u32) {
    let (_, bpp, funcs) = FORMATS[format];
    let stride = (32 * bpp).div_ceil(32) * 4;
    set(data, address, d::bit_count, bpp);
    set(data, address, d::width, 32);
    set(data, address, d::height, 16);
    set(
        data,
        address,
        d::compression,
        if seed & 4 != 0 { 3 } else { 0 },
    );
    for (off, value) in [
        (rect::left, 0),
        (rect::top, 0),
        (rect::right, 32),
        (rect::bottom, 16),
    ] {
        set(data, address, d::rect + off, value);
    }
    set(
        data,
        address,
        d::stride,
        if seed & 1 != 0 {
            (-(stride as i32)) as u32
        } else {
            stride
        },
    );
    set(
        data,
        address,
        d::bits + bits::ptr,
        bits + if seed & 1 != 0 { stride * 15 } else { 0 },
    );
    let (shifts, lens) = match format {
        0 | 2 => ([16, 8, 0], [8, 8, 8]),
        1 => match seed / 2 % 4 {
            0 => ([0, 8, 16], [8, 8, 8]),
            1 => ([20, 10, 0], [10, 10, 10]),
            2 => ([16, 8, 0], [5, 6, 5]),
            _ => ([0, 12, 24], [4, 4, 4]),
        },
        3 => ([10, 5, 0], [5, 5, 5]),
        _ => match seed / 2 % 3 {
            0 => ([11, 5, 0], [5, 6, 5]),
            1 => ([0, 5, 10], [5, 5, 5]),
            _ => ([8, 4, 0], [4, 4, 4]),
        },
    };
    for i in 0..3 {
        set(
            data,
            address,
            [d::red_shift, d::green_shift, d::blue_shift][i],
            shifts[i],
        );
        set(
            data,
            address,
            [d::red_len, d::green_len, d::blue_len][i],
            lens[i],
        );
        set(
            data,
            address,
            [d::red_mask, d::green_mask, d::blue_mask][i],
            ((1 << lens[i]) - 1) << shifts[i],
        );
    }
    set(data, address, d::funcs, BASE + funcs);
    if bpp <= 8 {
        set(data, address, d::color_table, PALETTE);
        set(data, address, d::color_table_size, 1 << bpp);
    }
}
fn fixture(format: usize, seed: u32) -> Vec<u8> {
    let mut data = vec![0; LEN as usize];
    let mut rng = Rng(seed.wrapping_add(0x57ec1d));
    for byte in &mut data[0x1000..0x10000] {
        *byte = rng.next() as u8;
    }
    dib(&mut data, DST, format, DEST_BITS, seed);
    dib(&mut data, SRC, format, SOURCE_BITS, seed + 1);
    dib(&mut data, BRUSH, format, MASK_BITS, 0);
    for i in 0..3 {
        let l = 1 + rng.next() % 5;
        let t = 1 + rng.next() % 3;
        let r = l + 1 + rng.next() % (29 - l);
        let b = t + 1 + rng.next() % (13 - t);
        for (j, v) in [l, t, r, b].into_iter().enumerate() {
            set(&mut data, RECTS, i * 16 + j * 4, v);
        }
    }
    set(&mut data, ORIGIN, 0, 3);
    set(&mut data, ORIGIN, 4, 1);
    data
}
fn compare(
    native: &mut Machine<Cpu>,
    reference: &mut Machine<Cpu>,
    table: &str,
    function: &str,
    args: &[u32],
    data: &[u8],
    case: &str,
) {
    let entry = at(table, function);
    assert!(
        native.sys.redirects.contains_key(&entry),
        "unbound {function}"
    );
    let expected = call(
        reference,
        entry,
        args,
        data,
        function.starts_with("gradient"),
    );

    // The translated caller warms before it can redirect. Compare every run,
    // and require at least one accepted body for each individual input.
    for _ in 0..3 {
        let actual = call(native, entry, args, data, function.starts_with("gradient"));
        if let Some(offset) = actual.iter().zip(&expected).position(|(a, b)| a != b) {
            panic!(
                "{function} {case}, byte {offset:#x}: native {}, Wine {}",
                actual[offset], expected[offset]
            );
        }
    }
    assert!(
        native.sys.direct_bodies.contains_key(&entry),
        "{function} {case} did not execute its body"
    );
}
fn family(family: &str) {
    let Some(dist) = dist() else {
        return;
    };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut native = machine(&dist, true, None);
    let mut reference = machine(&dist, false, None);
    reference.sys.redirects.clear();
    for (format, format_info) in FORMATS.iter().enumerate().take(5) {
        let suffix = if matches!(
            family,
            "solid_rects"
                | "solid_line"
                | "pattern_rects"
                | "copy_rect"
                | "mask_rect"
                | "stretch_row"
                | "shrink_row"
        ) {
            match format_info.1 {
                32 => "32",
                24 => "24",
                _ => "16",
            }
        } else if family == "halftone" && format == 0 {
            "888"
        } else {
            format_info.0
        };
        let name = format!("{family}_{suffix}");

        for seed in 0..64 {
            let mut data = fixture(format, seed);
            let args = match family {
                "solid_rects" => vec![
                    DST,
                    3,
                    RECTS,
                    if seed % 3 == 0 { 0 } else { 0x91827364 },
                    seed.wrapping_mul(0x1928374),
                ],
                "solid_line" => {
                    let negative = seed & 2 != 0;
                    set(&mut data, ORIGIN, 0, if negative { 25 } else { 3 });
                    set(&mut data, ORIGIN, 4, if negative { 13 } else { 1 });
                    for (i, v) in [
                        if seed % 3 == 0 { 0 } else { (-7i32) as u32 },
                        (-13i32) as u32,
                        7,
                        seed % 2,
                        11,
                        if negative { u32::MAX } else { 1 },
                        if negative { u32::MAX } else { 1 },
                        seed % 2,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        set(&mut data, PARAMS, i * 4, v);
                    }
                    vec![
                        DST,
                        ORIGIN,
                        PARAMS,
                        if seed % 3 == 0 { 0 } else { 0xf0e1d2c3 },
                        seed * 0x1928374,
                    ]
                }
                "pattern_rects" => {
                    set(&mut data, BRUSH, d::width, 7);
                    set(&mut data, BRUSH, d::height, 5);
                    set(
                        &mut data,
                        EXTRA,
                        masks::and_,
                        if seed % 3 == 0 { 0 } else { MASK_BITS + 0x1000 },
                    );
                    set(&mut data, EXTRA, masks::xor_, MASK_BITS);
                    set(&mut data, ORIGIN, 0, (-11i32) as u32);
                    set(&mut data, ORIGIN, 4, 23);
                    vec![DST, 3, RECTS, ORIGIN, BRUSH, EXTRA]
                }
                "copy_rect" => {
                    if seed & 16 != 0 {
                        let header = data[..d::SIZE].to_vec();
                        data[0x100..0x100 + d::SIZE].copy_from_slice(&header);
                    }
                    set(&mut data, ORIGIN, 0, if seed & 1 != 0 { 1 } else { 3 });
                    set(&mut data, ORIGIN, 4, if seed & 2 != 0 { 1 } else { 3 });
                    vec![DST, RECTS, SRC, ORIGIN, 1 + seed % 16, seed % 16]
                }
                "blend_rects" => {
                    dib(&mut data, SRC, 0, SOURCE_BITS, seed + 1);
                    set(&mut data, ORIGIN, 0, 1);
                    set(&mut data, ORIGIN, 4, 1);
                    vec![
                        DST,
                        3,
                        RECTS,
                        SRC,
                        ORIGIN,
                        ([0, 1, 127, 128, 254, 255][seed as usize % 6] << 16)
                            | ((seed / 6 % 2) << 24),
                    ]
                }
                "gradient_rect" => {
                    let mode = seed % 4;
                    for (i, (x, y)) in [
                        (0, 0),
                        (
                            if mode == 2 { 30 } else { 32 },
                            if mode == 2 { 7 } else { 16 },
                        ),
                        if seed % 16 == 2 { (0, 0) } else { (3, 16) },
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        set(&mut data, EXTRA, i * vertex::SIZE + vertex::x, x);
                        set(&mut data, EXTRA, i * vertex::SIZE + vertex::y, y);
                        set(
                            &mut data,
                            EXTRA,
                            i * vertex::SIZE + vertex::Red,
                            seed * 913 + 0x1050_0000,
                        );
                        set(
                            &mut data,
                            EXTRA,
                            i * vertex::SIZE + vertex::Blue,
                            seed * 753 + 0xb0ff_0000,
                        );
                    }
                    vec![DST, RECTS, EXTRA, mode]
                }
                "mask_rect" => {
                    dib(&mut data, SRC, 7, SOURCE_BITS, seed + 1);
                    set(&mut data, SRC, d::rect + rect::left, seed % 3);
                    set(&mut data, SRC, d::rect + rect::top, seed % 2);
                    vec![DST, RECTS, SRC, ORIGIN, 1 + seed % 16]
                }
                "draw_glyph" | "draw_subpixel_glyph" => {
                    let sub = family == "draw_subpixel_glyph";
                    dib(
                        &mut data,
                        SRC,
                        if sub { 0 } else { 5 },
                        SOURCE_BITS,
                        seed + 1,
                    );
                    for v in &mut data[0x8000..0xc000] {
                        if !sub {
                            *v %= 18;
                        }
                    }
                    let text = seed.wrapping_mul(0x713b983);
                    for mask in 0..17 {
                        for c in 0..3 {
                            let t = ((text >> (16 - c * 8)) & 255) as u8;
                            let p = (EXTRA - DATA) as usize + mask * 6 + c * 2;
                            data[p] = (u32::from(t) * mask as u32 / 16) as u8;
                            data[p + 1] = (255 - ((255 - u32::from(t)) * mask as u32 / 16)) as u8;
                        }
                    }
                    if sub {
                        set(
                            &mut data,
                            EXTRA,
                            gamma::gamma,
                            if seed % 3 == 0 { 1000 } else { 1400 },
                        );
                        for i in 0..256 {
                            data[(EXTRA - DATA) as usize + gamma::encode + i] = (255 - i) as u8;
                            data[(EXTRA - DATA) as usize + gamma::decode + i] = (i / 2) as u8;
                        }
                    }
                    vec![
                        DST,
                        RECTS,
                        SRC,
                        ORIGIN,
                        text,
                        if sub && seed % 4 == 0 { 0 } else { EXTRA },
                    ]
                }
                "stretch_row" | "shrink_row" => {
                    let negative = seed & 8 != 0;
                    set(&mut data, ORIGIN, 0, if negative { 25 } else { 3 });
                    for (i, v) in [
                        if seed % 3 == 0 { 0 } else { (-7i32) as u32 },
                        (-13i32) as u32,
                        7,
                        19,
                        if negative { u32::MAX } else { 1 },
                        if negative { u32::MAX } else { 1 },
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        set(&mut data, PARAMS, i * 4, v);
                    }
                    vec![DST, ORIGIN, SRC, ORIGIN, PARAMS, 1 + seed % 4, seed / 4 % 2]
                }
                "halftone" => {
                    for (address, rect) in [(EXTRA, [0, 0, 19, 11]), (PARAMS, [1, 1, 27, 14])] {
                        let [x, y, w, h] = rect;
                        let mirror = if address == EXTRA {
                            seed & 2 != 0
                        } else {
                            seed & 4 != 0
                        };
                        let mirrored_y = if address == EXTRA {
                            seed & 8 != 0
                        } else {
                            seed & 16 != 0
                        };
                        set(
                            &mut data,
                            address,
                            coords::x,
                            if mirror { x + w - 1 } else { x },
                        );
                        set(
                            &mut data,
                            address,
                            coords::y,
                            if mirrored_y { y + h - 1 } else { y },
                        );
                        set(
                            &mut data,
                            address,
                            coords::width,
                            if mirror { (-(w as i32)) as u32 } else { w },
                        );
                        set(
                            &mut data,
                            address,
                            coords::height,
                            if mirrored_y { (-(h as i32)) as u32 } else { h },
                        );
                        for (i, v) in [x, y, x + w, y + h].into_iter().enumerate() {
                            set(&mut data, address, coords::visrect + i * 4, v);
                        }
                    }
                    vec![DST, EXTRA, SRC, PARAMS]
                }
                _ => unreachable!(),
            };
            compare(
                &mut native,
                &mut reference,
                &table,
                &name,
                &args,
                &data,
                &format!("format {format} seed {seed}"),
            );
        }
        assert!(
            native.sys.direct_bodies.contains_key(&at(&table, &name)),
            "{name} never reached a host body"
        );
    }
    assert_eq!(
        adv_nt::native::declines(&native.ext),
        0,
        "body declined covered inputs"
    );
}

// spec: NAT-BODY-4
#[test]
fn solid_rects_match_wine() {
    family("solid_rects");
}
// spec: NAT-BODY-4
#[test]
fn solid_lines_match_wine() {
    family("solid_line");
}
// spec: NAT-BODY-4
#[test]
fn patterns_match_wine() {
    family("pattern_rects");
}
// spec: NAT-BODY-4
#[test]
fn copies_match_wine() {
    family("copy_rect");
}
// spec: NAT-BODY-4
#[test]
fn blends_match_wine() {
    family("blend_rects");
}
// spec: NAT-BODY-4
#[test]
fn gradients_match_wine() {
    family("gradient_rect");
}
// spec: NAT-BODY-4
#[test]
fn masks_match_wine() {
    family("mask_rect");
}
// spec: NAT-BODY-4
#[test]
fn glyphs_match_wine() {
    family("draw_glyph");
}
// spec: NAT-BODY-4
#[test]
fn subpixel_glyphs_match_wine() {
    family("draw_subpixel_glyph");
}
// spec: NAT-BODY-4
#[test]
fn stretched_rows_match_wine() {
    family("stretch_row");
}
// spec: NAT-BODY-4
#[test]
fn shrunk_rows_match_wine() {
    family("shrink_row");
}
// spec: NAT-BODY-4
#[test]
fn halftones_match_wine() {
    family("halftone");
}

// spec: NAT-BODY-4
#[test]
fn conversions_match_wine() {
    let Some(dist) = dist() else {
        return;
    };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut native = machine(&dist, true, None);
    let mut reference = machine(&dist, false, None);
    reference.sys.redirects.clear();
    for src in 0..5 {
        for (dst, format) in FORMATS.iter().enumerate() {
            for seed in 0..32 {
                let mut data = fixture(dst, seed);
                dib(&mut data, SRC, src, SOURCE_BITS, seed + 1);
                // Include partial last bytes and padding in every destination format.
                set(&mut data, RECTS, rect::left, 2);
                set(&mut data, RECTS, rect::right, 3 + seed % 25);
                if seed % 8 == 6 {
                    set(&mut data, RECTS, rect::left, 0);
                    set(&mut data, RECTS, rect::right, 32);
                    dib(&mut data, SRC, src, SOURCE_BITS, seed);
                }
                if dst < 5 && seed % 8 == 7 {
                    let bits = u32::from_le_bytes(
                        data[0x100 + d::bits..0x104 + d::bits].try_into().unwrap(),
                    );
                    set(&mut data, SRC, d::bits, bits - SOURCE_BITS + DEST_BITS + 4);
                }
                if dst >= 5 && seed % 3 == 0 {
                    set(&mut data, DST, d::color_table, 0);
                }
                if dst == 7 && seed % 4 == 0 {
                    set(&mut data, DST, d::color_table_size, 1);
                    set(&mut data, DST, d::color_table, PALETTE);
                }
                if dst == 7 && seed % 4 == 0 {
                    let pixel = if seed % 8 == 4 { 0x1234u32 } else { 0 };
                    let bg = if seed % 8 == 4 { 0x10ff0000 | pixel } else { 0 };
                    set(&mut data, PALETTE, 0, bg);
                    let sa = u32::from_le_bytes(
                        data[0x100 + d::bits..0x100 + d::bits + 4]
                            .try_into()
                            .unwrap(),
                    );
                    let stride = i32::from_le_bytes(
                        data[0x100 + d::stride..0x100 + d::stride + 4]
                            .try_into()
                            .unwrap(),
                    );
                    let l = u32::from_le_bytes(
                        data[(RECTS - DATA) as usize..(RECTS - DATA) as usize + 4]
                            .try_into()
                            .unwrap(),
                    );
                    let t = u32::from_le_bytes(
                        data[(RECTS - DATA) as usize + 4..(RECTS - DATA) as usize + 8]
                            .try_into()
                            .unwrap(),
                    );
                    let at = sa.wrapping_add((t as i32 * stride) as u32) + l * FORMATS[src].1 / 8;
                    let value = if FORMATS[src].1 == 24 {
                        ((pixel & 255) << 16) | (pixel & 0xff00) | (pixel >> 16)
                    } else {
                        pixel
                    };
                    let offset = (at - DATA) as usize;
                    data[offset..offset + FORMATS[src].1 as usize / 8]
                        .copy_from_slice(&value.to_le_bytes()[..FORMATS[src].1 as usize / 8]);
                }
                let args = [DST, SRC, RECTS, seed % 2];
                compare(
                    &mut native,
                    &mut reference,
                    &table,
                    &format!("convert_to_{}", format.0),
                    &args,
                    &data,
                    &format!("src {src} seed {seed}"),
                );
            }
        }
    }
    assert_eq!(adv_nt::native::declines(&native.ext), 0);
}

// spec: NAT-ID-2, NAT-BODY-4
#[test]
fn changed_elf_function_is_not_bound() {
    let Some(dist) = dist() else {
        return;
    };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let address = at(&table, "solid_rects_32");
    let intact = machine(&dist, true, None);
    assert!(intact.sys.redirects.contains_key(&address));
    let patched = machine(&dist, true, Some(address));
    assert!(!patched.sys.redirects.contains_key(&address));
    assert!(
        patched
            .sys
            .redirects
            .contains_key(&at(&table, "solid_rects_16"))
    );
    let images = patched.ext.get::<adv_nt::identity::Images>();
    assert_eq!(images.containing(address).unwrap().name, "win32u.so");
    assert!(
        adv_nt::identity::place(&patched.ext, address)
            .unwrap()
            .starts_with("win32u.so!solid_rects_32+")
    );
}

// spec: NAT-BODY-4
#[test]
fn dib_layout_matches_dwarf() {
    let Some(dist) = dist() else {
        return;
    };
    let path = dist
        .with_file_name("wine11-sse-dist")
        .join("usr/local/lib/wine/i386-unix/win32u.so");
    let dump = std::process::Command::new("dwarfdump")
        .arg("--debug-info")
        .arg(path)
        .output()
        .unwrap();
    assert!(dump.status.success());
    let text = String::from_utf8(dump.stdout).unwrap();
    let entries: Vec<_> = text.split("\n0x").collect();
    let source = include_str!("../src/native/dib_layout.rs");
    for (module, name) in [
        ("dib", "dib_info"),
        ("bits", "gdi_image_bits"),
        ("rect", "tagRECT"),
        ("point", "tagPOINT"),
        ("line", "line_params"),
        ("stretch", "stretch_params"),
        ("intensity", "intensity_range"),
        ("gamma", "font_gamma_ramp"),
        ("coords", "bitblt_coords"),
        ("blend", "_BLENDFUNCTION"),
        ("vertex", "_TRIVERTEX"),
        ("rgb", "tagRGBQUAD"),
        ("masks", "rop_mask_bits"),
    ] {
        let mut start = entries
            .iter()
            .position(|e| {
                (e.contains("DW_TAG_structure_type") || e.contains("DW_TAG_typedef"))
                    && e.contains(&format!("DW_AT_name\t(\"{name}\")"))
            })
            .unwrap();
        if entries[start].contains("DW_TAG_typedef") {
            let id = entries[start]
                .split("DW_AT_type\t(0x")
                .nth(1)
                .unwrap()
                .split(' ')
                .next()
                .unwrap();
            start = entries
                .iter()
                .position(|e| e.starts_with(&format!("{id}:")))
                .unwrap();
        }
        let members: Vec<_> = entries[start + 1..]
            .iter()
            .take_while(|e| !e.contains(":     NULL"))
            .copied()
            .collect();
        let constants = source
            .split(&format!("pub mod {module} {{"))
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
            let expected = line.split("= ").nth(1).unwrap().trim_end_matches(';');
            let expected = usize::from_str_radix(expected.trim_start_matches("0x"), 16).unwrap();
            let (entry, attribute) = if field == "SIZE" {
                (entries[start], "DW_AT_byte_size\t(0x")
            } else {
                (
                    members
                        .iter()
                        .find(|e| {
                            e.contains(&format!(
                                "DW_AT_name\t(\"{}\")",
                                field.trim_end_matches('_')
                            ))
                        })
                        .unwrap()
                        .to_owned(),
                    "DW_AT_data_member_location\t(0x",
                )
            };
            let actual = entry
                .split(attribute)
                .nth(1)
                .unwrap()
                .split(')')
                .next()
                .unwrap();
            assert_eq!(
                expected,
                usize::from_str_radix(actual, 16).unwrap(),
                "{name}.{field}"
            );
        }
    }
    for line in source.lines().filter(|l| l.starts_with("pub const")) {
        let name = line
            .split("pub const ")
            .nth(1)
            .unwrap()
            .split(':')
            .next()
            .unwrap();
        let expected =
            u32::from_str_radix(line.split("= 0x").nth(1).unwrap().trim_end_matches(';'), 16)
                .unwrap();
        let entry = entries
            .iter()
            .find(|e| {
                e.contains(&format!("\"{}\"", name.to_ascii_lowercase()))
                    && e.contains("DW_OP_addr")
            })
            .unwrap();
        let actual = entry
            .split("DW_OP_addr 0x")
            .nth(1)
            .unwrap()
            .split(')')
            .next()
            .unwrap();
        assert_eq!(expected, u32::from_str_radix(actual, 16).unwrap(), "{name}");
    }
}

// spec: NAT-ID-2
#[test]
fn elf_tables_include_static_symbols_and_mask_relocations() {
    // An authored ELF32 function: push a relocated immediate; ret. SHT_SYMTAB
    // contains a local STT_FUNC, which the dynamic symbol table does not expose.
    let mut image = vec![0; 0x400];
    image[..6].copy_from_slice(b"\x7fELF\x01\x01");
    let put = |image: &mut [u8], at: usize, value: u32| {
        image[at..at + 4].copy_from_slice(&value.to_le_bytes())
    };
    put(&mut image, 16, 3 | (3 << 16));
    put(&mut image, 20, 1);
    put(&mut image, 28, 52);
    put(&mut image, 32, 0x200);
    put(&mut image, 40, 52 | (32 << 16));
    put(&mut image, 44, 1 | (40 << 16));
    put(&mut image, 48, 5);
    for (i, v) in [1, 0, 0, 0, 0x400, 0x400, 5, 0x1000]
        .into_iter()
        .enumerate()
    {
        put(&mut image, 52 + i * 4, v);
    }
    image[0x100..0x106].copy_from_slice(&[0x68, 0x10, 0, 0, 0, 0xc3]);
    image[0x181..0x188].copy_from_slice(b"static\0");
    for (i, v) in [1, 0x100, 6, 2 | (1 << 16)].into_iter().enumerate() {
        put(&mut image, 0x160 + i * 4, v);
    }
    for (index, fields) in [
        (1, [0, 1, 6, 0x100, 0x100, 6, 0, 0, 1, 0]),
        (2, [0, 2, 0, 0, 0x150, 32, 3, 0, 4, 16]),
        (3, [0, 3, 0, 0, 0x180, 8, 0, 0, 1, 0]),
        (4, [0, 9, 2, 0x190, 0x190, 8, 2, 1, 4, 8]),
    ] {
        for (i, v) in fields.into_iter().enumerate() {
            put(&mut image, 0x200 + index * 40 + i * 4, v);
        }
    }
    put(&mut image, 0x190, 0x101);
    put(&mut image, 0x194, 8);
    let root = std::env::temp_dir().join(format!("dib-elf-identity-{}", std::process::id()));
    let debug = root.join("debug");
    let stripped = root.join("stripped");
    std::fs::create_dir_all(debug.join("lib/wine/i386-windows")).unwrap();
    std::fs::create_dir_all(stripped.join("lib/wine/i386-windows")).unwrap();
    for dist in [&debug, &stripped] {
        std::fs::create_dir_all(dist.join("lib/wine/i386-unix")).unwrap();
        std::fs::write(dist.join("lib/wine/i386-unix/authored.so"), &image).unwrap();
    }
    let output = std::process::Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/wine/identity.py"))
        .arg(&debug)
        .arg(&stripped)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut m = Machine::new(
        Cpu::new(PARK_ADDR, false).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(BASE, 0x1000, Prot::RX).unwrap();
    m.cpu.write(BASE, &image).unwrap();
    m.cpu.write_u32(BASE + 0x101, 0xdeadbeef).unwrap();
    let mut env = adv_machine::Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        context: 0,
        thunk: 0,
    };
    adv_nt::identity::note_unix(
        &mut env,
        &stripped.join("lib/wine/i386-unix/authored.so"),
        BASE,
        &image,
        &[0x101],
    );
    let module = m
        .ext
        .get::<adv_nt::identity::Images>()
        .0
        .get(&BASE)
        .unwrap();
    let table = module.table().unwrap();
    let f = table.named("static").unwrap();
    assert_eq!((f.rva, f.size), (0x100, 6));
    assert!(adv_nt::identity::intact(&m.cpu, BASE + 0x100, f, |a| a == BASE + 0x101));
    m.cpu.write(BASE + 0x105, &[0xcc]).unwrap();
    assert!(!adv_nt::identity::intact(&m.cpu, BASE + 0x100, f, |a| a == BASE + 0x101));
    image[52 + 24] ^= 2;
    m.cpu.write(BASE, &image[..84]).unwrap();
    let mut env = adv_machine::Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        context: 0,
        thunk: 0,
    };
    adv_nt::identity::note_unix(
        &mut env,
        &stripped.join("lib/wine/i386-unix/authored.so"),
        BASE,
        &image,
        &[0x101],
    );
    assert!(
        m.ext
            .get::<adv_nt::identity::Images>()
            .0
            .get(&BASE)
            .unwrap()
            .table()
            .is_none()
    );
    std::fs::remove_dir_all(root).unwrap();
}

// spec: NAT-ID-2, NAT-BODY-4
#[test]
fn dib_headers_and_faults_complete_at_entry() {
    let Some(dist) = dist() else {
        return;
    };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    for case in 0..3 {
        let mut native = machine(&dist, true, None);
        let mut reference = machine(&dist, false, None);
        reference.sys.redirects.clear();
        let entry = at(&table, "solid_rects_32");
        let args = [DST, 3, RECTS, 0, 0x12345678];
        let warm = fixture(0, 19);
        for _ in 0..3 {
            assert_eq!(
                call(&mut native, entry, &args, &warm, false),
                call(&mut reference, entry, &args, &warm, false)
            );
        }
        let mut data = warm;
        match case {
            0 => set(&mut data, DST, d::funcs, BASE + FUNCS_8),
            1 => set(&mut data, DST, d::width, 100000),
            2 => {
                set(&mut data, RECTS, rect::left, 0);
                set(&mut data, RECTS, rect::right, 2);
                set(&mut data, RECTS, rect::top, 0);
                set(&mut data, RECTS, rect::bottom, 1);
                set(&mut data, RECTS + 16, rect::left, 0);
                set(&mut data, RECTS + 16, rect::right, 2);
                set(&mut data, RECTS + 16, rect::top, 10000);
                set(&mut data, RECTS + 16, rect::bottom, 10001);
            }
            _ => unreachable!(),
        }
        let before = adv_nt::native::declines(&native.ext);
        let (actual, bytes) = execute(&mut native, entry, &args, &data, false, None);
        let (expected, reference_bytes) = execute(&mut reference, entry, &args, &data, false, None);
        if case == 2 {
            assert!(matches!(actual, RunResult::Failed(_)), "{actual:?}");
            assert!(matches!(expected, RunResult::Failed(_)), "{expected:?}");
        } else {
            assert!(matches!(actual, RunResult::Exited(0)), "{actual:?}");
            assert!(matches!(expected, RunResult::Exited(0)), "{expected:?}");
        }
        if case == 2 {
            assert_eq!(bytes, data, "fault wrote pixels");
        } else {
            assert_eq!(bytes, reference_bytes);
        }
        assert_eq!(
            adv_nt::native::declines(&native.ext) - before,
            0,
            "case {case}"
        );
    }
}

// spec: NAT-BODY-4
#[test]
fn copies_snapshot_only_touched_rows() {
    let Some(dist) = dist() else {
        return;
    };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut native = machine(&dist, true, None);
    let mut reference = machine(&dist, false, None);
    reference.sys.redirects.clear();
    for seed in [0, 2] {
        // Rows past the fixture are unmapped; a whole-surface snapshot declines.
        let mut data = fixture(0, seed);
        set(&mut data, DST, d::height, 100000);
        set(&mut data, SRC, d::height, 100000);
        set(&mut data, ORIGIN, 0, 1);
        set(&mut data, ORIGIN, 4, 1);
        let before = adv_nt::native::declines(&native.ext);
        compare(
            &mut native,
            &mut reference,
            &table,
            "copy_rect_32",
            &[DST, RECTS, SRC, ORIGIN, 13, 0],
            &data,
            &format!("seed {seed}"),
        );
        assert_eq!(adv_nt::native::declines(&native.ext), before);
    }
}

// spec: NAT-ID-2, NAT-BODY-4
#[test]
fn halftone_covers_nondefault_rounding() {
    let Some(dist) = dist() else {
        return;
    };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut native = machine(&dist, true, None);
    let mut reference = machine(&dist, false, None);
    reference.sys.redirects.clear();
    let mut data = fixture(0, 29);
    for (address, w, h) in [(EXTRA, 19, 11), (PARAMS, 27, 14)] {
        for (field, value) in [
            (coords::width, w),
            (coords::height, h),
            (coords::visrect + rect::right, w),
            (coords::visrect + rect::bottom, h),
        ] {
            set(&mut data, address, field, value);
        }
    }
    let entry = at(&table, "halftone_888");
    let args = [DST, EXTRA, SRC, PARAMS];
    for _ in 0..3 {
        assert_eq!(
            call(&mut native, entry, &args, &data, false),
            call(&mut reference, entry, &args, &data, false)
        );
    }
    let before = adv_nt::native::declines(&native.ext);
    for control in [0x1f80, 0x3f80, 0x5f80, 0x7f80, 0x9fc0] {
        let (actual, bytes) = execute(&mut native, entry, &args, &data, false, Some(control));
        let (expected, reference_bytes) =
            execute(&mut reference, entry, &args, &data, false, Some(control));
        assert!(matches!(actual, RunResult::Exited(0)), "{actual:?}");
        assert!(matches!(expected, RunResult::Exited(0)), "{expected:?}");
        assert_eq!(bytes, reference_bytes);
    }
    assert_eq!(adv_nt::native::declines(&native.ext), before);
}

fn prepare_body(m: &mut Machine<Cpu>, name: &str, args: &[u32]) -> u32 {
    if m.sys.contexts.is_empty() {
        m.spawn("body", CODE, &[], 0x10000).unwrap();
    }
    let thunk = m
        .sys
        .thunks
        .published_index(adv_nt::native::MODULE, &format!("win32u.so!{name}"))
        .unwrap();
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, RESULT + 0x800);
    for (i, arg) in args.iter().enumerate() {
        m.cpu
            .write_u32(RESULT + 0x804 + i as u32 * 4, *arg)
            .unwrap();
    }
    thunk
}
fn body(m: &mut Machine<Cpu>, thunk: u32) -> adv_machine::Outcome {
    let f = m.sys.thunks.get(thunk).f.handler;
    f(&mut adv_machine::Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        context: 0,
        thunk,
    })
}

// Captured from c2e87f32: changed destination bytes from the original bodies.
// Unchanged bytes come from the deterministic input, including padding/gaps.
fn recorded_bytes(golden: &mut &[u8], data: &[u8], actual: &[u8]) {
    let count = u16::from_le_bytes(golden[..2].try_into().unwrap()) as usize;
    let mut expected = [0; 256];
    expected.copy_from_slice(&data[0x4000..0x4100]);
    for pair in golden[2..2 + count * 2].as_chunks::<2>().0 {
        expected[pair[0] as usize] = pair[1];
    }
    *golden = &golden[2 + count * 2..];
    assert_eq!(actual, expected);
}

// Small surfaces with sentinel bytes in both DIB padding and stride gaps.
fn row_fixture(dst: usize, src: usize, seed: u32, top: u32, alias: bool) -> Vec<u8> {
    let mut data = fixture(dst, seed);
    dib(&mut data, SRC, src, SOURCE_BITS, seed / 2);
    for (address, format, bits, negative) in [
        (DST, dst, DEST_BITS, seed & 1 != 0),
        (
            SRC,
            src,
            if alias { DEST_BITS + 4 } else { SOURCE_BITS },
            seed & 2 != 0,
        ),
    ] {
        let stride = (9 * FORMATS[format].1).div_ceil(32) * 4 + 4;
        set(&mut data, address, d::width, 9);
        set(&mut data, address, d::height, 6);
        set(
            &mut data,
            address,
            d::stride,
            if negative {
                (-(stride as i32)) as u32
            } else {
                stride
            },
        );
        set(
            &mut data,
            address,
            d::bits,
            bits + if negative { stride * 5 } else { 0 },
        );
    }
    set(&mut data, DST, d::rect + rect::top, top);
    for (field, value) in [
        (rect::left, 1),
        (rect::top, top),
        (rect::right, 8),
        (rect::bottom, top + 2),
    ] {
        set(&mut data, RECTS, field, value);
    }
    data
}

// spec: NAT-BODY-1, NAT-BODY-4
#[test]
fn touched_bodies_preserve_recorded_bytes() {
    let Some(dist) = dist() else {
        return;
    };
    let mut m = machine(&dist, true, None);
    let mut window = machine(&dist, true, None);
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut golden = include_bytes!("fixtures/native_dib.bin").as_slice();
    for family in ["convert_to", "halftone", "solid_line"] {
        for (dst, format) in
            FORMATS
                .iter()
                .enumerate()
                .take(if family == "convert_to" { 8 } else { 5 })
        {
            for src in 0..if family == "solid_line" { 1 } else { 5 } {
                for seed in 0..8 {
                    for top in [0, 2, 4] {
                        let mut data = row_fixture(dst, src, seed, top, seed & 4 != 0);
                        let suffix = if family == "solid_line" {
                            match format.1 {
                                32 => "32",
                                24 => "24",
                                _ => "16",
                            }
                        } else if family == "halftone" && dst == 0 {
                            "888"
                        } else {
                            format.0
                        };
                        let name = format!("{family}_{suffix}");
                        let args = match family {
                            "convert_to" => vec![DST, SRC, RECTS, seed % 2],
                            "halftone" => {
                                for (address, y, w, h) in [(EXTRA, 0, 7, 2), (PARAMS, top, 8, 2)] {
                                    for (field, value) in [
                                        (coords::x, 0),
                                        (coords::y, y),
                                        (coords::width, w),
                                        (coords::height, h),
                                        (coords::visrect + rect::left, 0),
                                        (coords::visrect + rect::top, y),
                                        (coords::visrect + rect::right, w),
                                        (coords::visrect + rect::bottom, y + h),
                                    ] {
                                        set(&mut data, address, field, value);
                                    }
                                }
                                vec![DST, EXTRA, SRC, PARAMS, 0x1f80]
                            }
                            _ => {
                                set(&mut data, DST, d::rect + rect::top, 0);
                                set(&mut data, ORIGIN, 0, if seed & 4 != 0 { 7 } else { 1 });
                                set(
                                    &mut data,
                                    ORIGIN,
                                    4,
                                    if seed & 4 != 0 { top + 1 } else { top },
                                );
                                for (field, value) in [
                                    (line::err_start, u32::MAX),
                                    (line::err_add_1, (-2i32) as u32),
                                    (line::err_add_2, 1),
                                    (line::bias, 0),
                                    (line::length, 3),
                                    (line::x_inc, if seed & 4 != 0 { u32::MAX } else { 1 }),
                                    (line::y_inc, if seed & 4 != 0 { u32::MAX } else { 1 }),
                                    (line::x_major, 1),
                                ] {
                                    set(&mut data, PARAMS, field, value);
                                }
                                vec![
                                    DST,
                                    ORIGIN,
                                    PARAMS,
                                    if seed & 2 != 0 { 0 } else { 0x7f3f1fff },
                                    0x432176ab,
                                ]
                            }
                        };
                        m.cpu.write(DATA, &data).unwrap();
                        let thunk = prepare_body(&mut m, &name, &args);
                        assert!(
                            matches!(body(&mut m, thunk), adv_machine::Outcome::Return64(_)),
                            "{name} src {src} seed {seed} top {top}"
                        );
                        let mut bytes = [0; 256];
                        m.cpu.read(DEST_BITS, &mut bytes).unwrap();
                        recorded_bytes(&mut golden, &data, &bytes);
                        for _ in 0..2 {
                            call(&mut window, at(&table, &name), &args, &data, false);
                            let mut actual = [0; 256];
                            window.cpu.read(DEST_BITS, &mut actual).unwrap();
                            assert_eq!(actual, bytes, "{name} window");
                        }
                        m.cpu.write(DATA, &data).unwrap();
                        prepare_body(&mut m, &name, &args);
                        ALLOCATIONS.with(|n| n.set(Some(0)));
                        let result = body(&mut m, thunk);
                        let allocations = ALLOCATIONS.with(|n| n.replace(None)).unwrap();
                        assert!(matches!(result, adv_machine::Outcome::Return64(_)));
                        assert_eq!(allocations, 0, "{name} src {src} seed {seed} top {top}");
                    }
                }
            }
        }
    }
    assert!(golden.is_empty());
}

fn thread_seconds() -> f64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time is writable for the complete timespec.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) },
        0
    );
    time.tv_sec as f64 + time.tv_nsec as f64 * 1e-9
}

// spec: NAT-BODY-4
#[test]
#[ignore = "manual micro-benchmark; nextest enforces a timeout"]
fn convert_microbenchmark() {
    let Some(dist) = dist() else {
        return;
    };
    let mut m = machine(&dist, true, None);
    for (src, dst, label) in [
        (0, 2, "8888->24"),
        (0, 1, "8888->8888 bitfields"),
        (0, 3, "8888->555"),
        (0, 4, "8888->565"),
        (2, 0, "24->8888"),
    ] {
        let mut data = fixture(dst, 0);
        dib(&mut data, SRC, src, SOURCE_BITS, 0);
        if dst == 1 {
            for (field, value) in [
                (d::red_shift, 16),
                (d::green_shift, 8),
                (d::blue_shift, 0),
                (d::red_mask, 0xff0000),
                (d::green_mask, 0xff00),
                (d::blue_mask, 0xff),
            ] {
                set(&mut data, DST, field, value);
            }
        }
        let source = 0x20000000;
        let target = 0x21000000;
        if src == 0 && dst == 2 {
            m.cpu
                .map(source, (960 * 540 * 4 + 4095) & !4095, Prot::RW)
                .unwrap();
            m.cpu
                .map(target, (960 * 540 * 4 + 4095) & !4095, Prot::RW)
                .unwrap();
        }
        let pixels: Vec<_> = (0..960 * 540 * 4).map(|i| (i * 71) as u8).collect();
        m.cpu.write(source, &pixels).unwrap();
        for (address, bits, bpp) in [(DST, target, FORMATS[dst].1), (SRC, source, FORMATS[src].1)] {
            for (field, value) in [
                (d::bits, bits),
                (d::width, 960),
                (d::height, 540),
                (d::stride, 960 * bpp / 8),
            ] {
                set(&mut data, address, field, value);
            }
        }
        for rows in [540, 16] {
            for (field, value) in [
                (rect::left, 0),
                (rect::top, 0),
                (rect::right, 960),
                (rect::bottom, rows),
            ] {
                set(&mut data, RECTS, field, value);
            }
            m.cpu.write(DATA, &data).unwrap();
            let thunk = prepare_body(
                &mut m,
                &format!("convert_to_{}", FORMATS[dst].0),
                &[DST, SRC, RECTS, 0],
            );
            for _ in 0..10 {
                assert!(matches!(
                    body(&mut m, thunk),
                    adv_machine::Outcome::Return64(_)
                ));
            }
            let mut samples = [0.0f64; 5];
            for sample in &mut samples {
                let start = thread_seconds();
                for _ in 0..200 {
                    assert!(matches!(
                        body(&mut m, thunk),
                        adv_machine::Outcome::Return64(_)
                    ));
                }
                *sample = (thread_seconds() - start) * 1e6 / 200.0;
            }
            samples.sort_by(f64::total_cmp);
            eprintln!(
                "convert 960x{rows} {label}: {:.3} CPU us/call, {:.3} ns/pixel (median of 5 batches of 200 warm calls; range {:.3}..{:.3})",
                samples[2],
                samples[2] * 1000.0 / (960 * rows) as f64,
                samples[0],
                samples[4]
            );
        }
    }
}

thread_local! { static ALLOCATIONS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }
struct Allocator;
fn allocation() {
    ALLOCATIONS.with(|n| {
        if let Some(count) = n.get() {
            n.set(Some(count + 1));
        }
    });
}
// SAFETY: all storage operations delegate to System with the original arguments.
unsafe impl std::alloc::GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        allocation();
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        allocation();
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        allocation();
        unsafe { std::alloc::System.realloc(p, layout, size) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

struct AccessGuest<'a> {
    cpu: &'a mut dyn Guest,
    reads: std::cell::RefCell<Vec<(u32, usize)>>,
    writes: Vec<(u32, usize)>,
    denied: Option<(u32, usize)>,
}
impl AccessGuest<'_> {
    fn allowed(&self, at: u32, len: usize) -> bool {
        !self.denied.is_some_and(|(row, size)| {
            u64::from(at) < u64::from(row) + size as u64
                && u64::from(row) < u64::from(at) + len as u64
        })
    }
}
impl Guest for AccessGuest<'_> {
    fn fpu_environment(
        &mut self,
    ) -> Result<adv_cpu::backend::FpuEnvironment, adv_cpu::backend::FpuEnvironmentError> {
        self.cpu.fpu_environment()
    }

    fn reg(&self, r: adv_cpu::backend::Reg) -> u32 {
        self.cpu.reg(r)
    }
    fn set_reg(&mut self, r: adv_cpu::backend::Reg, value: u32) {
        self.cpu.set_reg(r, value);
    }
    fn read(&self, at: u32, data: &mut [u8]) -> Result<(), adv_cpu::backend::MemFault> {
        self.reads.borrow_mut().push((at, data.len()));
        if !self.allowed(at, data.len()) {
            return Err(adv_cpu::backend::MemFault {
                addr: at,
                len: data.len() as u32,
            });
        }
        self.cpu.read(at, data)
    }
    fn write(&mut self, at: u32, data: &[u8]) -> Result<(), adv_cpu::backend::MemFault> {
        self.writes.push((at, data.len()));
        if !self.allowed(at, data.len()) {
            return Err(adv_cpu::backend::MemFault {
                addr: at,
                len: data.len() as u32,
            });
        }
        self.cpu.write(at, data)
    }
    fn accessible(&self, at: u32, len: u32, prot: Prot) -> bool {
        self.allowed(at, len as usize) && self.cpu.accessible(at, len, prot)
    }
    fn map(&mut self, at: u32, len: u32, prot: Prot) -> Result<(), adv_cpu::backend::MemFault> {
        self.cpu.map(at, len, prot)
    }
    fn unmap(&mut self, at: u32, len: u32) -> Result<(), adv_cpu::backend::MemFault> {
        self.cpu.unmap(at, len)
    }
    fn protect(&mut self, at: u32, len: u32, prot: Prot) -> Result<(), adv_cpu::backend::MemFault> {
        self.cpu.protect(at, len, prot)
    }
}

// spec: NAT-BODY-1, NAT-BODY-4
#[test]
fn touched_bodies_limit_access_and_fault_without_writes() {
    let Some(dist) = dist() else {
        return;
    };
    let mut m = machine(&dist, true, None);
    for family in ["convert_to_8888", "halftone_888", "solid_line_32"] {
        for negative in [false, true] {
            for denied in [
                None,
                Some(DEST_BITS),
                Some(DEST_BITS + 256),
                Some(SOURCE_BITS + 384),
            ] {
                let mut data = fixture(0, 0);
                for (address, bits) in [(DST, DEST_BITS), (SRC, SOURCE_BITS)] {
                    set(&mut data, address, d::height, 100000);
                    // Declared rows outside this band are inaccessible.
                    set(
                        &mut data,
                        address,
                        d::stride,
                        if negative { (-128i32) as u32 } else { 128 },
                    );
                    set(
                        &mut data,
                        address,
                        d::bits,
                        bits + if negative { 128 * 99999 } else { 0 },
                    );
                    set(
                        &mut data,
                        address,
                        d::rect + rect::top,
                        if negative { 99994 } else { 0 },
                    );
                }
                // Physical destination rows 1..3; source rows 2..4.
                set(
                    &mut data,
                    DST,
                    d::rect + rect::top,
                    if negative { 99997 } else { 1 },
                );
                for (field, value) in [
                    (rect::left, 0),
                    (rect::top, 2),
                    (rect::right, 32),
                    (rect::bottom, 4),
                ] {
                    set(&mut data, RECTS, field, value);
                }
                set(&mut data, ORIGIN, 0, 1);
                set(&mut data, ORIGIN, 4, 0);
                for (field, value) in [
                    (line::err_start, 0),
                    (line::err_add_1, 0),
                    (line::err_add_2, 0),
                    (line::bias, 0),
                    (line::length, 2),
                    (line::x_inc, 1),
                    (line::y_inc, 1),
                    (line::x_major, 0),
                ] {
                    set(&mut data, PARAMS, field, value);
                }
                for (address, y) in [(EXTRA, 0), (PARAMS + 0x100, 2)] {
                    for (field, value) in [
                        (coords::x, 0),
                        (coords::y, y),
                        (coords::width, 32),
                        (coords::height, 2),
                        (coords::visrect + rect::left, 0),
                        (coords::visrect + rect::top, y),
                        (coords::visrect + rect::right, 32),
                        (coords::visrect + rect::bottom, y + 2),
                    ] {
                        set(&mut data, address, field, value);
                    }
                }
                let args = match family {
                    "convert_to_8888" => vec![DST, SRC, RECTS, 0],
                    "halftone_888" => vec![DST, EXTRA, SRC, PARAMS + 0x100, 0x1f80],
                    _ => vec![DST, ORIGIN, PARAMS, 0x7fffffff, 0x87654321],
                };
                m.cpu.write(DATA, &data).unwrap();
                let thunk = prepare_body(&mut m, family, &args);
                let f = m.sys.thunks.get(thunk).f.handler;
                let mut cpu = AccessGuest {
                    cpu: &mut m.cpu,
                    reads: Default::default(),
                    writes: Vec::new(),
                    denied: denied.map(|row| (row, 128)),
                };
                let before = adv_nt::native::declines(&m.ext);
                let result = f(&mut adv_machine::Env {
                    cpu: &mut cpu,
                    sys: &mut m.sys,
                    ext: &mut m.ext,
                    context: 0,
                    thunk,
                });
                if denied == Some(DEST_BITS + 256)
                    || (family != "solid_line_32" && denied == Some(SOURCE_BITS + 384))
                {
                    assert!(
                        !matches!(result, adv_machine::Outcome::Return64(_)),
                        "{family} accepted inaccessible row"
                    );
                    assert_eq!(adv_nt::native::declines(&m.ext), before);
                    assert!(cpu.writes.is_empty());
                    let mut actual = [0; 512];
                    cpu.cpu.read(DEST_BITS, &mut actual).unwrap();
                    assert_eq!(actual, data[0x4000..0x4200]);
                } else {
                    assert!(
                        matches!(result, adv_machine::Outcome::Return64(_)),
                        "{family} declined"
                    );
                    for &(at, len) in cpu.reads.borrow().iter() {
                        if (SOURCE_BITS..SOURCE_BITS + 0x4000).contains(&at) {
                            assert_eq!((at, len), (SOURCE_BITS + 256, 256));
                        }
                    }
                    assert!(!cpu.writes.is_empty());
                    for (at, len) in cpu.writes {
                        assert!(
                            at >= DEST_BITS + 128 && at + len as u32 <= DEST_BITS + 384,
                            "{family} wrote outside touched rows"
                        );
                    }
                }
            }
        }
    }
}

// spec: NAT-BODY-4
#[test]
fn raw_conversions_preserve_recorded_bytes() {
    let Some(dist) = dist() else {
        return;
    };
    let mut m = machine(&dist, true, None);
    let mut window = machine(&dist, true, None);
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut golden = include_bytes!("fixtures/native_dib_raw.bin").as_slice();
    for (format, info) in FORMATS.iter().enumerate().take(5) {
        for signs in 0..4 {
            for alias in 0..3 {
                for top in [0, 2] {
                    for rows in [0, 2] {
                        let mut data = row_fixture(format, format, signs, 0, false);
                        for (address, bits, negative) in [
                            (
                                DST,
                                DEST_BITS + if alias == 2 { 4 } else { 0 },
                                signs & 1 != 0,
                            ),
                            (
                                SRC,
                                if alias == 0 {
                                    SOURCE_BITS
                                } else {
                                    DEST_BITS + if alias == 1 { 4 } else { 0 }
                                },
                                signs & 2 != 0,
                            ),
                        ] {
                            let stride = info.1 + 4;
                            set(&mut data, address, d::width, 8);
                            set(
                                &mut data,
                                address,
                                d::stride,
                                if negative {
                                    (-(stride as i32)) as u32
                                } else {
                                    stride
                                },
                            );
                            set(
                                &mut data,
                                address,
                                d::bits,
                                bits + if negative { stride * 5 } else { 0 },
                            );
                        }
                        for (field, value) in [
                            (rect::left, 0),
                            (rect::top, top),
                            (rect::right, 8),
                            (rect::bottom, top + rows),
                        ] {
                            set(&mut data, RECTS, field, value);
                        }
                        m.cpu.write(DATA, &data).unwrap();
                        let name = format!("convert_to_{}", info.0);
                        let thunk = prepare_body(&mut m, &name, &[DST, SRC, RECTS, 0]);
                        assert!(
                            matches!(body(&mut m, thunk), adv_machine::Outcome::Return64(_)),
                            "{name} signs {signs} alias {alias} top {top} rows {rows}"
                        );
                        let mut bytes = [0; 256];
                        m.cpu.read(DEST_BITS, &mut bytes).unwrap();
                        recorded_bytes(&mut golden, &data, &bytes);
                        for _ in 0..2 {
                            call(
                                &mut window,
                                at(&table, &name),
                                &[DST, SRC, RECTS, 0],
                                &data,
                                false,
                            );
                            let mut actual = [0; 256];
                            window.cpu.read(DEST_BITS, &mut actual).unwrap();
                            assert_eq!(actual, bytes, "{name} window");
                        }
                        m.cpu.write(DATA, &data).unwrap();
                        prepare_body(&mut m, &name, &[DST, SRC, RECTS, 0]);
                        ALLOCATIONS.with(|n| n.set(Some(0)));
                        let result = body(&mut m, thunk);
                        let allocations = ALLOCATIONS.with(|n| n.replace(None)).unwrap();
                        assert!(matches!(result, adv_machine::Outcome::Return64(_)));
                        assert_eq!(
                            allocations, 0,
                            "{name} signs {signs} alias {alias} top {top} rows {rows}"
                        );
                    }
                }
            }
        }
    }
    assert!(golden.is_empty());
}

// spec: NAT-BODY-1, NAT-BODY-4
#[test]
fn halftone_keeps_touched_row_access_requirements() {
    let Some(dist) = dist() else {
        return;
    };
    let mut m = machine(&dist, true, None);
    let mut data = fixture(0, 0);
    dib(&mut data, SRC, 0, SOURCE_BITS, 0);
    for address in [EXTRA, PARAMS] {
        for (field, value) in [
            (coords::x, 0),
            (coords::y, 0),
            (coords::width, 7),
            (coords::height, 2),
            (coords::visrect + rect::left, 0),
            (coords::visrect + rect::top, 0),
            (coords::visrect + rect::right, 7),
            (coords::visrect + rect::bottom, 2),
        ] {
            set(&mut data, address, field, value);
        }
    }
    for at in [DEST_BITS + 28, SOURCE_BITS + 28] {
        m.cpu.write(DATA, &data).unwrap();
        let thunk = prepare_body(&mut m, "halftone_888", &[DST, EXTRA, SRC, PARAMS, 0x1f80]);
        let f = m.sys.thunks.get(thunk).f.handler;
        // This byte lies outside the converted pixels, within a touched row.
        let mut cpu = AccessGuest {
            cpu: &mut m.cpu,
            reads: Default::default(),
            writes: Vec::new(),
            denied: Some((at, 1)),
        };
        let result = f(&mut adv_machine::Env {
            cpu: &mut cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            context: 0,
            thunk,
        });
        assert!(!matches!(result, adv_machine::Outcome::Return64(_)));
        assert!(cpu.writes.is_empty());
    }
}

// Wine's per-pixel conversion: unpack each source channel to eight bits, then
// pack it into the destination field. Destination padding is cleared.
fn reference_pixel(data: &[u8], (src, dst): (usize, usize), pixel: u32) -> u32 {
    let field = |address: u32, offset: usize| {
        let at = (address - DATA) as usize + offset;
        u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
    };
    let channels = |address: u32| {
        (
            [d::red_shift, d::green_shift, d::blue_shift].map(|f| field(address, f) as i32),
            [d::red_len, d::green_len, d::blue_len].map(|f| field(address, f)),
        )
    };
    let mask = |len: u32| (255u32 << (8 - len.min(8))) & 255;
    let rgb = match src {
        0 | 2 => [(pixel >> 16) & 255, (pixel >> 8) & 255, pixel & 255],
        3 => [
            ((pixel >> 7) & 248) | ((pixel >> 12) & 7),
            ((pixel >> 2) & 248) | ((pixel >> 7) & 7),
            ((pixel << 3) & 248) | ((pixel >> 2) & 7),
        ],
        _ => {
            let (shifts, lens) = channels(SRC);
            std::array::from_fn(|i| {
                let shift = shifts[i] - (8 - lens[i] as i32);
                let value = if shift < 0 {
                    pixel.wrapping_shl(-shift as u32)
                } else {
                    pixel.wrapping_shr(shift as u32)
                } & mask(lens[i]);
                value | value.wrapping_shr(lens[i])
            })
        }
    };
    match dst {
        0 | 2 => (rgb[0] & 255) << 16 | (rgb[1] & 255) << 8 | (rgb[2] & 255),
        3 => ((rgb[0] << 7) & 0x7c00) | ((rgb[1] << 2) & 0x3e0) | ((rgb[2] >> 3) & 31),
        _ => {
            let (shifts, lens) = channels(DST);
            (0..3).fold(0, |p, i| {
                let shift = shifts[i] - (8 - lens[i] as i32);
                let value = rgb[i] & mask(lens[i]);
                p | if shift < 0 {
                    value.wrapping_shr(-shift as u32)
                } else {
                    value.wrapping_shl(shift as u32)
                }
            })
        }
    }
}

// spec: NAT-BODY-4
#[test]
fn true_color_rows_match_wine_pixel_path() {
    let Some(dist) = dist() else {
        return;
    };
    let mut m = machine(&dist, true, None);
    for (src, source) in FORMATS.iter().enumerate().take(5) {
        for (dst, target) in FORMATS.iter().enumerate().take(5) {
            for width in [1, 2, 3, 7, 8, 9, 15, 16, 17, 960] {
                for seed in [0, 2, 4, 6] {
                    for signs in 0..4 {
                        let mut data = fixture(dst, seed);
                        dib(&mut data, SRC, src, SOURCE_BITS, seed + 2);
                        for (address, bits, bpp, extra, negative) in [
                            (SRC, SOURCE_BITS, source.1, 2, signs & 1 != 0),
                            (DST, DEST_BITS, target.1, 1, signs & 2 != 0),
                        ] {
                            let row_len = ((width + extra) * bpp).div_ceil(32) * 4;
                            let stride = row_len + 4;
                            set(&mut data, address, d::width, width + extra);
                            set(&mut data, address, d::height, 3);
                            set(
                                &mut data,
                                address,
                                d::stride,
                                if negative {
                                    (-(stride as i32)) as u32
                                } else {
                                    stride
                                },
                            );
                            set(
                                &mut data,
                                address,
                                d::bits,
                                bits + if negative { 2 * stride } else { 0 },
                            );
                        }
                        for (field, value) in [
                            (rect::left, 1),
                            (rect::top, 1),
                            (rect::right, width + 1),
                            (rect::bottom, 3),
                        ] {
                            set(&mut data, RECTS, field, value);
                        }
                        m.cpu.write(DATA, &data).unwrap();
                        let name = format!("convert_to_{}", target.0);
                        let thunk = prepare_body(&mut m, &name, &[DST, SRC, RECTS, 0]);
                        assert!(
                            matches!(body(&mut m, thunk), adv_machine::Outcome::Return64(_)),
                            "{src}->{dst} width {width} seed {seed} signs {signs}"
                        );
                        let row_len = ((width + 1) * target.1).div_ceil(32) * 4;
                        let stride = row_len + 4;
                        let mut actual = vec![0; (3 * stride) as usize];
                        m.cpu.read(DEST_BITS, &mut actual).unwrap();
                        let mut expected = data[0x4000..0x4000 + actual.len()].to_vec();
                        let (ssize, dsize) = (source.1.div_ceil(8), target.1.div_ceil(8));
                        // Wine copies matching formats verbatim, including unused bits.
                        let masks = |address: u32| {
                            [d::red_mask, d::green_mask, d::blue_mask].map(|f| {
                                let at = (address - DATA) as usize + f;
                                u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
                            })
                        };
                        let raw = (src == dst && matches!(src, 0 | 2 | 3))
                            || (matches!(dst, 1 | 4)
                                && !matches!(src, 0 | 3)
                                && source.1 == target.1
                                && masks(SRC) == masks(DST));
                        let source_stride = ((width + 2) * source.1).div_ceil(32) * 4 + 4;
                        for y in 0..2 {
                            let at = (if signs & 2 != 0 { 2 - y } else { y } * stride) as usize;
                            let from = (0x8000
                                + if signs & 1 != 0 { 1 - y } else { y + 1 } * source_stride
                                + ssize) as usize;
                            let row = &mut expected[at..at + row_len as usize];
                            row.fill(0);
                            for x in 0..width as usize {
                                let mut pixel = [0; 4];
                                let input = from + x * ssize as usize;
                                pixel[..ssize as usize]
                                    .copy_from_slice(&data[input..input + ssize as usize]);
                                let pixel = u32::from_le_bytes(pixel);
                                let value = if raw {
                                    pixel
                                } else {
                                    reference_pixel(&data, (src, dst), pixel)
                                };
                                let output = x * dsize as usize;
                                row[output..output + dsize as usize]
                                    .copy_from_slice(&value.to_le_bytes()[..dsize as usize]);
                            }
                        }
                        assert_eq!(
                            actual, expected,
                            "{src}->{dst} width {width} seed {seed} signs {signs}"
                        );
                        m.cpu.write(DATA, &data).unwrap();
                        prepare_body(&mut m, &name, &[DST, SRC, RECTS, 0]);
                        ALLOCATIONS.with(|n| n.set(Some(0)));
                        let result = body(&mut m, thunk);
                        let allocations = ALLOCATIONS.with(|n| n.replace(None)).unwrap();
                        assert!(matches!(result, adv_machine::Outcome::Return64(_)));
                        assert_eq!(
                            allocations, 0,
                            "{src}->{dst} width {width} seed {seed} signs {signs}"
                        );
                    }
                }
            }
        }
    }
}

// spec: NAT-BODY-1, NAT-BODY-4
#[test]
fn conversion_keeps_declared_source_pixel_width() {
    let Some(dist) = dist() else {
        return;
    };
    let mut m = machine(&dist, true, None);
    let mut data = fixture(0, 0);
    dib(&mut data, SRC, 2, SOURCE_BITS, 0);
    // The existing body accepts headers whose declared pixel width differs
    // from the format table's usual width. Preserve that per-pixel behavior.
    set(&mut data, SRC, d::bit_count, 32);
    set(&mut data, SRC, d::stride, 128);
    for (field, value) in [
        (rect::left, 0),
        (rect::top, 0),
        (rect::right, 9),
        (rect::bottom, 2),
    ] {
        set(&mut data, RECTS, field, value);
    }
    let mut expected = data[0x4000..0x4100].to_vec();
    for y in 0..2 {
        for x in 0..9 {
            let at = y * 128 + x * 4;
            // The old rgb/pack path reads a DWORD and copies its BGR channels.
            expected[at..at + 3].copy_from_slice(&data[0x8000 + at..0x8000 + at + 3]);
            expected[at + 3] = 0;
        }
        expected[y * 128 + 36..y * 128 + 128].fill(0);
    }
    for _ in 0..2 {
        m.cpu.write(DATA, &data).unwrap();
        let thunk = prepare_body(&mut m, "convert_to_8888", &[DST, SRC, RECTS, 0]);
        assert!(matches!(
            body(&mut m, thunk),
            adv_machine::Outcome::Return64(_)
        ));
        let mut actual = [0; 256];
        m.cpu.read(DEST_BITS, &mut actual).unwrap();
        assert_eq!(actual.as_slice(), expected);
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

// spec: NAT-BODY-4
#[test]
#[ignore = "translated pixel micro-benchmark; nextest enforces a timeout"]
fn translated_dib_microbenchmark() {
    let Some(dist) = dist() else { return };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut m = machine(&dist, true, None);
    let source = 0x20000000;
    let target = 0x21000000;
    for at in [source, target] {
        m.cpu.map(at, 0x200000, Prot::RW).unwrap();
        m.cpu.write(at, &vec![0x7f; 0x200000]).unwrap();
    }
    for (name, w, h, n) in [
        ("copy_rect_32", 16, 1, 100000),
        ("blend_rects_8888", 64, 64, 10000),
        ("convert_to_24", 960, 540, 200),
    ] {
        let mut data = fixture(if name == "convert_to_24" { 2 } else { 0 }, 0);
        for (address, bits, bpp) in [
            (DST, target, if name == "convert_to_24" { 24 } else { 32 }),
            (SRC, source, 32),
        ] {
            for (field, value) in [
                (d::bits, bits),
                (d::width, w),
                (d::height, h),
                (d::stride, w * bpp / 8),
                (d::rect + rect::left, 0),
                (d::rect + rect::top, 0),
            ] {
                set(&mut data, address, field, value);
            }
        }
        for (field, value) in [
            (rect::left, 0),
            (rect::top, 0),
            (rect::right, w),
            (rect::bottom, h),
        ] {
            set(&mut data, RECTS, field, value);
        }
        set(&mut data, ORIGIN, 0, 0);
        set(&mut data, ORIGIN, 4, 0);
        m.cpu.write(DATA, &data).unwrap();
        let args = if name == "copy_rect_32" {
            vec![DST, RECTS, SRC, ORIGIN, 13, 0]
        } else if name.starts_with("blend") {
            vec![DST, 1, RECTS, SRC, ORIGIN, 0x017f0000]
        } else {
            vec![DST, SRC, RECTS, 0]
        };
        let entry = at(&table, name);
        translated_loop(&mut m, entry, &args, 10);
        let mut samples = [0.0; 5];
        for value in &mut samples {
            *value = translated_loop(&mut m, entry, &args, n);
        }
        samples.sort_by(f64::total_cmp);
        eprintln!(
            "{name} {w}x{h}: {:.3} ns/call (median of 5, {:.3}..{:.3})",
            samples[2], samples[0], samples[4]
        );
    }
}

// spec: NAT-BODY-4, NAT-BODY-16
#[test]
fn indexed_sources_and_overlapping_conversions_match_wine() {
    let Some(dist) = dist() else { return };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut native = machine(&dist, true, None);
    let mut reference = machine(&dist, false, None);
    reference.sys.redirects.clear();
    for (dst, format) in FORMATS.iter().enumerate() {
        for src in 5..8 {
            for offset in [None, Some(4u32), Some(u32::MAX - 3)] {
                let mut data = fixture(dst, 0);
                dib(&mut data, SRC, src, SOURCE_BITS, 0);
                if let Some(offset) = offset {
                    set(&mut data, SRC, d::bits, DEST_BITS.wrapping_add(offset));
                }
                for (field, value) in [
                    (rect::left, 1),
                    (rect::top, 0),
                    (rect::right, 17),
                    (rect::bottom, 2),
                ] {
                    set(&mut data, RECTS, field, value);
                }
                let name = format!("convert_to_{}", format.0);
                compare(
                    &mut native,
                    &mut reference,
                    &table,
                    &name,
                    &[DST, SRC, RECTS, 0],
                    &data,
                    &format!("indexed {src}->{dst} offset {offset:?}"),
                );
            }
        }
    }
}

// spec: NAT-BODY-4, NAT-BODY-16
#[test]
fn halftone_unmasked_precision_faults_at_entry() {
    let Some(dist) = dist() else { return };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut native = machine(&dist, true, None);
    let mut reference = machine(&dist, false, None);
    reference.sys.redirects.clear();
    let mut data = fixture(0, 0);
    for address in [EXTRA, PARAMS] {
        for (field, value) in [
            (coords::x, 0),
            (coords::y, 0),
            (coords::width, 7),
            (coords::height, 2),
            (coords::visrect, 0),
            (coords::visrect + 4, 0),
            (coords::visrect + 8, 7),
            (coords::visrect + 12, 2),
        ] {
            set(&mut data, address, field, value);
        }
    }
    let entry = at(&table, "halftone_888");
    let args = [DST, EXTRA, SRC, PARAMS];
    for m in [&mut native, &mut reference] {
        let (result, bytes) = execute(m, entry, &args, &data, false, Some(0x0f80));
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        assert!(bytes[0x4000..0x8000] == data[0x4000..0x8000]);
    }
    assert_eq!(
        native.sys.latest_cpu_fault.as_ref().unwrap().fault.kind,
        adv_cpu::backend::FaultKind::Trap(19)
    );
    assert_eq!(
        native.sys.latest_cpu_fault.as_ref().unwrap().fault.pc,
        entry
    );
    assert_eq!(adv_nt::native::declines(&native.ext), 0);
}

// spec: NAT-BODY-4, NAT-BODY-16
#[test]
fn indexed_markers_and_mono_background_match_wine() {
    let Some(dist) = dist() else { return };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let mut native = machine(&dist, true, None);
    let mut reference = machine(&dist, false, None);
    reference.sys.redirects.clear();
    for (dst, format) in FORMATS.iter().enumerate().skip(5) {
        for src in 5..8 {
            for single in [false, true] {
                for left in [0, 1] {
                    let mut data = fixture(dst, 0);
                    dib(&mut data, SRC, src, SOURCE_BITS, 0);
                    set(&mut data, SRC, d::color_table, PALETTE + 0x800);
                    set(&mut data, PALETTE + 0x800, 0, 0x10ff0080);
                    set(&mut data, PALETTE + 0x800, 4, 0x10ff00ff);
                    if single && dst == 7 {
                        set(&mut data, DST, d::color_table_size, 1);
                    }
                    for (field, value) in [
                        (rect::left, left),
                        (rect::top, 0),
                        (rect::right, left + 17),
                        (rect::bottom, 2),
                    ] {
                        set(&mut data, RECTS, field, value);
                    }
                    compare(
                        &mut native,
                        &mut reference,
                        &table,
                        &format!("convert_to_{}", format.0),
                        &[DST, SRC, RECTS, 0],
                        &data,
                        &format!("palette {src}->{dst} single {single} left {left}"),
                    );
                }
            }
        }
    }
    for depth in 5..8 {
        let mut data = fixture(0, 0);
        dib(&mut data, SRC, depth, SOURCE_BITS, 0);
        compare(
            &mut native,
            &mut reference,
            &table,
            "mask_rect_32",
            &[DST, RECTS, SRC, ORIGIN, 7],
            &data,
            &format!("mask source header {depth}"),
        );
    }
}

// spec: NAT-BODY-4, NAT-BODY-16
#[test]
fn zero_brush_dimension_faults_at_entry() {
    let Some(dist) = dist() else { return };
    let table = std::fs::read_to_string(dist.join("share/hitode/identity/win32u.so.tsv")).unwrap();
    let entry = at(&table, "pattern_rects_32");
    for field in [d::width, d::height] {
        let mut data = fixture(0, 0);
        set(&mut data, BRUSH, field, 0);
        let args = [DST, 1, RECTS, ORIGIN, BRUSH, EXTRA];
        let mut native = machine(&dist, true, None);
        let mut reference = machine(&dist, false, None);
        let actual = execute(&mut native, entry, &args, &data, false, None);
        let expected = execute(&mut reference, entry, &args, &data, false, None);
        assert!(matches!(actual.0, RunResult::Failed(_)));
        assert!(matches!(expected.0, RunResult::Failed(_)));
        assert_eq!(&actual.1[0x4000..0x8000], &expected.1[0x4000..0x8000]);
        let fault = &native.sys.latest_cpu_fault.as_ref().unwrap().fault;
        assert_eq!(fault.pc, entry);
        assert_eq!(fault.kind, adv_cpu::backend::FaultKind::IntegerDivideByZero);
        assert_eq!(adv_nt::native::declines(&native.ext), 0);
    }
}
