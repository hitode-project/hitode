//! Authored guest VARIANTs and guest IUnknown callbacks; no host COM objects.
//! Native oracle: variant-native/probe.c, SHA256
//! 95C683A4232E85413D59EE409CB47D38AEE5231EBD31B97EF53274AFCD5EA782.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{
    Heaps,
    com::{alloc_bstr, copy_bstr, owns_bstr},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const RO: u32 = 0x430000;
const SOURCE: u32 = DATA + 0x100;
const DEST: u32 = SOURCE + 16;
const BSRC: u32 = SOURCE + 32;
const BDST: u32 = SOURCE + 48;
const CSRC: u32 = SOURCE + 64;
const CDST: u32 = SOURCE + 80;
const MSRC: u32 = SOURCE + 96;
const MDST: u32 = SOURCE + 112;
const BYSRC: u32 = SOURCE + 128;
const BYDST: u32 = SOURCE + 144;
const INVALID_SRC: u32 = SOURCE + 160;
const ARRAY_SRC: u32 = SOURCE + 176;
const SNAP: u32 = DATA + 0x300;
const RESULTS: u32 = DATA + 0x800;
const OBJECT: u32 = DATA + 0x1000;
const TABLE: u32 = OBJECT + 16;
const REFS: u32 = OBJECT + 32;
const ADDS: u32 = REFS + 4;
const RELEASES: u32 = REFS + 8;
const ORDER: u32 = REFS + 12;
const ADD_SEEN: u32 = REFS + 16;
const SELF_RESULT: u32 = REFS + 20;
const BEFORE: u32 = REFS + 24;
const AFTER: u32 = REFS + 28;
const ADD_CODE: u32 = CODE + 0x1000;
const RELEASE_CODE: u32 = CODE + 0x1400;
const MUTATE_CODE: u32 = CODE + 0x1800;

fn env<B: Backend>(m: &mut Machine<B>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn invoke(code: &mut Asm, target: u32, args: &[u32], result: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(target).mov_mem_eax(result);
}
fn raw<B: Backend>(m: &mut Machine<B>, address: u32, kind: u16, value: u32) {
    let mut b = [0; 16];
    b[..2].copy_from_slice(&kind.to_le_bytes());
    b[8..12].copy_from_slice(&value.to_le_bytes());
    m.cpu.write(address, &b).unwrap();
}
fn snap(code: &mut Asm, from: u32, to: u32) {
    for i in 0..4 {
        code.mov_eax_mem(from + i * 4).mov_mem_eax(to + i * 4);
    }
}

fn contract<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for (name, argc) in [
        ("VariantCopy", 2),
        ("#10", 2),
        ("VariantClear", 1),
        ("#9", 1),
        ("VariantInit", 1),
        ("#8", 1),
    ] {
        assert_eq!(
            registry.get("oleaut32.dll", name).unwrap().conv,
            adv_machine::Conv::Stdcall { argc }
        );
    }
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/variant",
            "fixture.exe",
            "/nonexistent/variant-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.map(RO, 4096, Prot::READ).unwrap();
    m.cpu.write(RO, &[0xcc; 16]).unwrap();
    let original: [u8; 16] = [
        3, 0, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 0xa5, 42, 0, 0, 0, 0xa5, 0xa5, 0xa5, 0xa5,
    ];
    m.cpu.write(SOURCE, &original).unwrap();
    m.cpu.write(DEST, &[0xcc; 16]).unwrap();
    let odd = [b'a', 0, 0, 0, b'b'];
    m.cpu.write(DATA + 0x1800, &odd).unwrap();
    let source_bstr = copy_bstr(&mut env(&mut m), DATA + 0x1800, odd.len() as u32);
    let old_bstr = alloc_bstr(&mut env(&mut m), &[b'o' as u16, b'l' as u16, b'd' as u16]);
    raw(&mut m, BSRC, 8, source_bstr);
    raw(&mut m, BDST, 8, old_bstr);
    raw(&mut m, CSRC, 13, OBJECT);
    raw(&mut m, CDST, 13, OBJECT);
    raw(&mut m, MSRC, 3, 10);
    raw(&mut m, MDST, 9, OBJECT);
    raw(&mut m, BYSRC, 0x400d, 0x600000);
    raw(&mut m, BYDST, 0, 0);
    raw(&mut m, INVALID_SRC, 15, 0);
    raw(&mut m, ARRAY_SRC, 0x2003, 0);
    m.cpu.write_u32(OBJECT, TABLE).unwrap();
    m.cpu.write_u32(TABLE + 4, ADD_CODE).unwrap();
    m.cpu.write_u32(TABLE + 8, RELEASE_CODE).unwrap();
    m.cpu.write_u32(REFS, 2).unwrap();
    let copy = m.resolve_import("oleaut32.dll", "VariantCopy").unwrap();
    let clear = m.resolve_import("oleaut32.dll", "#9").unwrap();
    let init = m.resolve_import("oleaut32.dll", "#8").unwrap();
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    let protect = m.resolve_import("kernel32.dll", "VirtualProtect").unwrap();
    let mut add = Asm::new(ADD_CODE);
    add.mov_eax_mem(CDST).mov_mem_eax(ADD_SEEN);
    invoke(&mut add, copy, &[CDST, CDST], SELF_RESULT);
    invoke(&mut add, sleep, &[1], RESULTS + 0xf0);
    add.mov_eax_mem(ADDS).add_eax_imm(1).mov_mem_eax(ADDS);
    add.mov_eax_mem(ORDER).add_eax_imm(1).mov_mem_eax(ORDER);
    add.mov_eax_mem(REFS)
        .add_eax_imm(1)
        .mov_mem_eax(REFS)
        .ret_n(4);
    m.cpu.write(ADD_CODE, &add.bytes).unwrap();
    let mut release = Asm::new(RELEASE_CODE);
    release
        .mov_eax_mem(RELEASES)
        .add_eax_imm(1)
        .mov_mem_eax(RELEASES);
    release
        .mov_eax_mem(ORDER)
        .add_eax_imm(10)
        .mov_mem_eax(ORDER);
    release
        .mov_eax_mem(REFS)
        .add_eax_imm(u32::MAX)
        .mov_mem_eax(REFS)
        .ret_n(4);
    m.cpu.write(RELEASE_CODE, &release.bytes).unwrap();
    let mut mutate = Asm::new(MUTATE_CODE);
    mutate.mov_mem_imm(MSRC + 8, 99);
    mutate
        .mov_eax_mem(REFS)
        .add_eax_imm(u32::MAX)
        .mov_mem_eax(REFS)
        .ret_n(4);
    m.cpu.write(MUTATE_CODE, &mutate.bytes).unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(BEFORE);
    invoke(&mut code, init, &[DEST], RESULTS);
    snap(&mut code, DEST, SNAP);
    invoke(&mut code, copy, &[DEST, SOURCE], RESULTS + 4);
    snap(&mut code, DEST, SNAP + 16);
    invoke(&mut code, clear, &[DEST], RESULTS + 8);
    snap(&mut code, DEST, SNAP + 32);
    invoke(
        &mut code,
        protect,
        &[source_bstr - 4, 4, 1, DATA + 0x1f00],
        RESULTS + 96,
    );
    invoke(&mut code, copy, &[DEST, BSRC], RESULTS + 100);
    invoke(
        &mut code,
        protect,
        &[source_bstr - 4, 4, 4, DATA + 0x1f00],
        RESULTS + 104,
    );
    invoke(&mut code, copy, &[BDST, INVALID_SRC], RESULTS + 12);
    code.mov_eax_mem(BDST + 8).mov_mem_eax(RESULTS + 16);
    invoke(&mut code, copy, &[RO, BSRC], RESULTS + 20);
    invoke(&mut code, copy, &[BDST, BSRC], RESULTS + 24);
    code.mov_eax_mem(BDST + 8).mov_mem_eax(RESULTS + 28);
    invoke(&mut code, copy, &[BDST, BDST], RESULTS + 32);
    code.mov_eax_mem(BDST + 8).mov_mem_eax(RESULTS + 36);
    // Read counted bytes through the public BSTR front, including the odd byte.
    code.mov_eax_mem(BDST + 8)
        .push_eax()
        .call(
            m.resolve_import("oleaut32.dll", "SysStringByteLen")
                .unwrap(),
        )
        .mov_mem_eax(RESULTS + 40);
    code.mov_eax_mem(BDST + 8)
        .mov_eax_ptr_eax()
        .mov_mem_eax(RESULTS + 44);
    code.mov_eax_mem(BDST + 8).add_eax_imm(4);
    code.bytes.extend_from_slice(&[0x0f, 0xb6, 0x00]);
    code.mov_mem_eax(RESULTS + 108);
    invoke(&mut code, clear, &[BDST], RESULTS + 48);
    invoke(&mut code, clear, &[BSRC], RESULTS + 52);
    invoke(&mut code, copy, &[CDST, CSRC], RESULTS + 56);
    code.mov_eax_mem(REFS).mov_mem_eax(RESULTS + 60);
    invoke(&mut code, clear, &[CDST], RESULTS + 64);
    invoke(&mut code, clear, &[CSRC], RESULTS + 68);
    code.mov_mem_imm(REFS, 1)
        .mov_mem_imm(TABLE + 8, MUTATE_CODE);
    invoke(&mut code, copy, &[MDST, MSRC], RESULTS + 72);
    code.mov_eax_mem(MDST + 8).mov_mem_eax(RESULTS + 76);
    invoke(&mut code, copy, &[BYDST, BYSRC], RESULTS + 80);
    snap(&mut code, BYDST, SNAP + 48);
    invoke(&mut code, clear, &[BYDST], RESULTS + 84);
    invoke(&mut code, copy, &[DEST, ARRAY_SRC], RESULTS + 88);
    invoke(&mut code, clear, &[INVALID_SRC], RESULTS + 92);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(AFTER);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("authored VariantCopy", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (offset, value) in [
        (0, 0),
        (4, 0),
        (8, 0),
        (12, 0x80020008),
        (16, old_bstr),
        (20, 0x80070057),
        (24, 0),
        (32, 0),
        (40, 5),
        (44, 0x61),
        (48, 0),
        (52, 0),
        (56, 0),
        (60, 2),
        (64, 0),
        (68, 0),
        (72, 0),
        (76, 99),
        (80, 0),
        (84, 0),
        (88, 0x80004001),
        (92, 0x80020008),
        (96, 1),
        (100, 0x80070057),
        (104, 1),
        (108, u32::from(b'b')),
    ] {
        assert_eq!(
            m.cpu.read_u32(RESULTS + offset).unwrap(),
            value,
            "result +{offset}"
        );
    }
    assert_eq!(
        m.cpu.read_u32(BEFORE).unwrap(),
        m.cpu.read_u32(AFTER).unwrap()
    );
    let mut bytes = [0; 16];
    m.cpu.read(SNAP, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 16]);
    m.cpu.read(SNAP + 16, &mut bytes).unwrap();
    assert_eq!(bytes, original);
    m.cpu.read(SNAP + 32, &mut bytes).unwrap();
    let mut cleared = original;
    cleared[..2].fill(0);
    assert_eq!(bytes, cleared);
    m.cpu.read(SNAP + 48, &mut bytes).unwrap();
    assert_eq!(u16::from_le_bytes(bytes[..2].try_into().unwrap()), 0x400d);
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
        0x600000
    );
    let copied = m.cpu.read_u32(RESULTS + 28).unwrap();
    assert_ne!(copied, source_bstr);
    // The retired destination allocation may be reused for the deep copy.
    assert_eq!(copied, m.cpu.read_u32(RESULTS + 36).unwrap());
    for pointer in [source_bstr, old_bstr, copied] {
        assert!(!owns_bstr(&env(&mut m), pointer));
        assert_eq!(m.ext.get_mut::<Heaps>().size(0, pointer - 4), None);
    }
    assert_eq!(m.cpu.read_u32(ADDS).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(RELEASES).unwrap(), 3);
    assert_eq!(m.cpu.read_u32(REFS).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(ADD_SEEN).unwrap(), 13);
    assert_eq!(m.cpu.read_u32(SELF_RESULT).unwrap(), 0);
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, reason)| api == "oleaut32.dll!VariantCopy" && reason.contains("SAFEARRAY"))
    );
    assert_eq!(m.cpu.read_u32(RO).unwrap(), 0xcccccccc);
}

#[test]
fn guest_variant_copy_preserves_native_bytes_bstr_ownership_and_callback_references() {
    contract(JitCpu::new(PARK_ADDR, false).unwrap());
    contract(JitCpu::new(PARK_ADDR, true).unwrap());
    contract(UnicornCpu::new(PARK_ADDR).unwrap());
}
