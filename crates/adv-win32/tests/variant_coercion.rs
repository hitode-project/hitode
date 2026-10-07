//! Guest Automation coercion, old ownership retirement and checked publication.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, RunResult, layout::PARK_ADDR};
use adv_os::com::{alloc_bstr, owns_bstr};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const RO: u32 = 0x430000;
const SOURCE: u32 = DATA + 0x100;
const DEST: u32 = DATA + 0x300;
const RESULTS: u32 = DATA + 0x600;
const OBJECT: u32 = DATA + 0x800;
const TABLE: u32 = OBJECT + 16;
const RELEASES: u32 = TABLE + 16;
const SEEN: u32 = RELEASES + 4;
const CALLBACK: u32 = CODE + 0x1000;

fn env<B: Backend>(m: &mut Machine<B>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn raw<B: Backend>(m: &mut Machine<B>, address: u32, kind: u16, value: u64) {
    let mut bytes = [0; 16];
    bytes[..2].copy_from_slice(&kind.to_le_bytes());
    bytes[8..].copy_from_slice(&value.to_le_bytes());
    m.cpu.write(address, &bytes).unwrap();
}
fn call(code: &mut Asm, target: u32, args: &[u32], result: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(target).mov_mem_eax(result);
}
fn contract<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for name in ["VariantChangeType", "#12"] {
        assert_eq!(
            registry.get("oleaut32.dll", name).unwrap().conv,
            adv_machine::Conv::Stdcall { argc: 4 }
        );
    }
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/coercion",
            "fixture.exe",
            "/nonexistent/coercion-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(RO, 4096, Prot::READ).unwrap();
    m.cpu.write(RO, &[0xa5; 16]).unwrap();
    let change = m
        .resolve_import("oleaut32.dll", "VariantChangeType")
        .unwrap();
    let ordinal = m.resolve_import("oleaut32.dll", "#12").unwrap();
    let clear = m.resolve_import("oleaut32.dll", "VariantClear").unwrap();
    let length = m.resolve_import("oleaut32.dll", "SysStringLen").unwrap();
    let protect = m.resolve_import("kernel32.dll", "VirtualProtect").unwrap();
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    let good = alloc_bstr(&mut env(&mut m), &"TrUe".encode_utf16().collect::<Vec<_>>());
    let bad = alloc_bstr(&mut env(&mut m), &"bad".encode_utf16().collect::<Vec<_>>());
    let inplace = alloc_bstr(&mut env(&mut m), &[b'0' as u16]);
    let old = alloc_bstr(&mut env(&mut m), &"old".encode_utf16().collect::<Vec<_>>());
    let underflow = alloc_bstr(
        &mut env(&mut m),
        &"1e-999".encode_utf16().collect::<Vec<_>>(),
    );
    for (i, (kind, value)) in [
        (3, (-7i32 as u32) as u64),
        (0, 0),
        (11, 2),
        (5, 0.25f64.to_bits()),
        (8, good as u64),
        (8, bad as u64),
        (1, 0),
        (0x4003, SOURCE as u64),
        (8, inplace as u64),
        (8, underflow as u64),
    ]
    .into_iter()
    .enumerate()
    {
        raw(&mut m, SOURCE + i as u32 * 16, kind, value);
    }
    let mut original = [0xa5; 16];
    original[..2].copy_from_slice(&3u16.to_le_bytes());
    original[8..12].copy_from_slice(&77u32.to_le_bytes());
    for i in 0..14 {
        m.cpu.write(DEST + i * 16, &original).unwrap();
    }
    raw(&mut m, DEST, 8, old as u64);
    raw(&mut m, DEST + 10 * 16, 13, OBJECT as u64);
    m.cpu.write_u32(OBJECT, TABLE).unwrap();
    m.cpu.write_u32(TABLE + 8, CALLBACK).unwrap();
    let mut release = Asm::new(CALLBACK);
    release
        .mov_eax_mem(RELEASES)
        .add_eax_imm(1)
        .mov_mem_eax(RELEASES);
    release.mov_eax_mem(DEST + 10 * 16).mov_mem_eax(SEEN);
    call(&mut release, sleep, &[0], RESULTS + 0x100);
    release.bytes.extend_from_slice(&[0x31, 0xc0]);
    release.ret_n(4);
    m.cpu.write(CALLBACK, &release.bytes).unwrap();
    let mut code = Asm::new(CODE);
    for i in 0..8 {
        call(
            &mut code,
            if i == 0 { ordinal } else { change },
            &[DEST + i * 16, SOURCE + i * 16, 0, 11],
            RESULTS + i * 4,
        );
    }
    call(
        &mut code,
        change,
        &[DEST + 8 * 16, SOURCE, 0, 5],
        RESULTS + 32,
    );
    call(
        &mut code,
        change,
        &[DEST + 9 * 16, SOURCE, 2, 11],
        RESULTS + 36,
    );
    call(
        &mut code,
        change,
        &[SOURCE + 8 * 16, SOURCE + 8 * 16, 0, 11],
        RESULTS + 40,
    );
    call(
        &mut code,
        change,
        &[SOURCE + 5 * 16, SOURCE + 5 * 16, 0, 11],
        RESULTS + 44,
    );
    call(&mut code, length, &[bad], RESULTS + 48);
    call(
        &mut code,
        change,
        &[DEST + 10 * 16, SOURCE, 0, 11],
        RESULTS + 52,
    );
    call(&mut code, change, &[RO, SOURCE, 0, 11], RESULTS + 56);
    // The source BSTR stays owned but its length prefix is genuinely inaccessible.
    let prefix_page = (good - 4) & !4095;
    call(
        &mut code,
        protect,
        &[prefix_page, 4096, 1, RESULTS + 0x104],
        RESULTS + 60,
    );
    call(
        &mut code,
        change,
        &[DEST + 11 * 16, SOURCE + 4 * 16, 0, 11],
        RESULTS + 64,
    );
    call(
        &mut code,
        protect,
        &[prefix_page, 4096, 4, RESULTS + 0x104],
        RESULTS + 68,
    );
    call(
        &mut code,
        change,
        &[DEST + 12 * 16, SOURCE + 9 * 16, 0, 11],
        RESULTS + 76,
    );
    call(
        &mut code,
        change,
        &[DEST + 13 * 16, SOURCE, 0, 0x123],
        RESULTS + 80,
    );
    for source in [SOURCE + 4 * 16, SOURCE + 5 * 16, SOURCE + 9 * 16] {
        call(&mut code, clear, &[source], RESULTS + 72);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("coercion", CODE, &[], 0x10000).unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    for i in 0..5 {
        assert_eq!(m.cpu.read_u32(RESULTS + i * 4).unwrap(), 0);
    }
    for i in [5, 6] {
        assert_eq!(m.cpu.read_u32(RESULTS + i * 4).unwrap(), 0x80020005);
    }
    for i in [7, 8, 9] {
        assert_eq!(m.cpu.read_u32(RESULTS + i * 4).unwrap(), 0x80004001);
    }
    for (i, value) in [(0, -1i16), (1, 0), (2, 2), (3, -1), (4, -1), (10, -1)] {
        assert_eq!(m.cpu.read_u16(DEST + i * 16).unwrap(), 11);
        assert_eq!(m.cpu.read_u16(DEST + i * 16 + 8).unwrap() as i16, value);
    }
    for i in [5, 6, 7, 8, 9, 11, 12, 13] {
        let mut bytes = [0; 16];
        m.cpu.read(DEST + i * 16, &mut bytes).unwrap();
        assert_eq!(bytes, original);
    }
    assert_eq!(m.cpu.read_u32(RESULTS + 40).unwrap(), 0);
    assert_eq!(m.cpu.read_u16(SOURCE + 8 * 16).unwrap(), 11);
    assert_eq!(m.cpu.read_u16(SOURCE + 8 * 16 + 8).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(RESULTS + 44).unwrap(), 0x80020005);
    assert_eq!(m.cpu.read_u32(RESULTS + 48).unwrap(), 3);
    assert_eq!(m.cpu.read_u32(RESULTS + 52).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(RELEASES).unwrap(), 1);
    assert_eq!(m.cpu.read_u16(SEEN).unwrap(), 13);
    assert_eq!(m.cpu.read_u32(RESULTS + 56).unwrap(), 0x80070057);
    assert_eq!(m.cpu.read_u32(RESULTS + 60).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(RESULTS + 64).unwrap(), 0x80070057);
    assert_eq!(m.cpu.read_u32(RESULTS + 68).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(RO).unwrap(), 0xa5a5a5a5);
    assert_eq!(m.cpu.read_u32(RESULTS + 76).unwrap(), 0x80004001);
    assert_eq!(m.cpu.read_u32(RESULTS + 80).unwrap(), 0x80020008);
    for pointer in [good, bad, inplace, old, underflow] {
        assert!(!owns_bstr(&env(&mut m), pointer));
    }
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, reason)| api == "oleaut32.dll!VariantChangeType"
                && reason.contains("sourceVT=0x4003")
                && reason.contains("targetVT=0x000b"))
    );
    assert_eq!(m.ext.get::<ApiDiagnostics>().stub_calls, 0);
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, reason)| api == "oleaut32.dll!VariantChangeType"
                && reason.contains("underflow")
                && reason.contains("sourceVT=0x0008"))
    );
}

#[test]
fn guest_variant_bool_coercion_preserves_failure_values_and_shared_ownership() {
    contract(JitCpu::new(PARK_ADDR, false).unwrap());
    contract(JitCpu::new(PARK_ADDR, true).unwrap());
    contract(UnicornCpu::new(PARK_ADDR).unwrap());
}
