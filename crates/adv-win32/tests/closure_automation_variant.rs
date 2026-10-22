//! Indirect copies and extended coercion preserve shared ownership across guest yields.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, RunResult, layout::PARK_ADDR};
use adv_os::com::{alloc_bstr, free_bstr, owns_bstr};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const SOURCE: u32 = DATA + 0x1000;
const DEST: u32 = DATA + 0x2000;
const OBJECT: u32 = DATA + 0x3000;
const TABLE: u32 = OBJECT + 16;
const REFS: u32 = TABLE + 16;
const TEXT: u32 = REFS + 16;
const MARK: u32 = 0x12345678;
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
fn text<B: Backend>(m: &Machine<B>, pointer: u32) -> String {
    let n = m.cpu.read_u32(pointer - 4).unwrap();
    let mut raw = vec![0; n as usize];
    m.cpu.read(pointer, &mut raw).unwrap();
    String::from_utf16(
        &raw.as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes(*b))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}
fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[u32], row: u32) {
    let api = m.resolve_import("oleaut32.dll", name).unwrap();
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    code.push_imm(MARK).call(set);
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(api)
        .mov_mem_eax(DATA + row * 8)
        .call(get)
        .mov_mem_eax(DATA + row * 8 + 4);
}
fn u64_at<B: Backend>(m: &Machine<B>, address: u32) -> u64 {
    let mut bytes = [0; 8];
    m.cpu.read(address, &mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-variant-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    let shared = alloc_bstr(
        &mut env(&mut m),
        &"shared".encode_utf16().collect::<Vec<_>>(),
    );
    let amount = alloc_bstr(&mut env(&mut m), &"2.5".encode_utf16().collect::<Vec<_>>());
    let date = alloc_bstr(
        &mut env(&mut m),
        &"2/29/2024".encode_utf16().collect::<Vec<_>>(),
    );
    let overflow = alloc_bstr(
        &mut env(&mut m),
        &"2147483648".encode_utf16().collect::<Vec<_>>(),
    );
    let preserved = alloc_bstr(&mut env(&mut m), &"old".encode_utf16().collect::<Vec<_>>());
    m.cpu.write_u32(TEXT, 42).unwrap();
    m.cpu.write_u32(TEXT + 4, shared).unwrap();
    raw(&mut m, SOURCE, 0x4003, u64::from(TEXT));
    raw(&mut m, SOURCE + 16, 0x400c, u64::from(SOURCE));
    raw(&mut m, SOURCE + 32, 0x4008, u64::from(TEXT + 4));
    raw(&mut m, DEST + 32, 8, u64::from(shared));
    raw(&mut m, SOURCE + 48, 0x400c, u64::from(SOURCE + 48));
    raw(&mut m, SOURCE + 64, 8, u64::from(amount));
    raw(&mut m, SOURCE + 80, 3, 42);
    raw(&mut m, SOURCE + 96, 8, u64::from(date));
    raw(&mut m, SOURCE + 112, 11, u64::from(u16::MAX));
    raw(&mut m, SOURCE + 128, 8, u64::from(overflow));
    raw(&mut m, DEST + 128, 8, u64::from(preserved));
    m.cpu.write_u32(OBJECT, TABLE).unwrap();
    m.cpu.write_u32(TABLE + 4, CODE + 0x2000).unwrap();
    m.cpu.write_u32(TABLE + 8, CODE + 0x2100).unwrap();
    m.cpu.write_u32(REFS, 2).unwrap();
    m.cpu.write_u32(TEXT + 8, OBJECT).unwrap();
    raw(&mut m, SOURCE + 144, 0x400d, u64::from(TEXT + 8));
    raw(&mut m, DEST + 144, 13, u64::from(OBJECT));
    raw(&mut m, DEST + 160, 13, u64::from(OBJECT));
    let mut code = Asm::new(CODE);
    for (row, destination, source) in [
        (0, DEST, SOURCE),
        (1, DEST + 16, SOURCE + 16),
        (2, DEST + 32, SOURCE + 32),
        (3, SOURCE, SOURCE),
        (4, DEST + 48, SOURCE + 48),
        (5, DEST + 64, 0),
    ] {
        call(
            &mut m,
            &mut code,
            "VariantCopyInd",
            &[destination, source],
            row,
        );
    }
    for (row, destination, source, target, flags) in [
        (6, SOURCE + 64, SOURCE + 64, 6, 0),
        (7, DEST + 80, SOURCE + 80, 5, 0),
        (8, DEST + 96, SOURCE + 96, 7, 0),
        (9, DEST + 112, SOURCE + 112, 8, 2),
        (10, DEST + 128, SOURCE + 128, 3, 0),
        (11, DEST + 144, SOURCE + 144, 13, 0),
        (12, DEST + 160, SOURCE + 80, 8, 0),
    ] {
        call(
            &mut m,
            &mut code,
            "VariantChangeTypeEx",
            &[destination, source, 0x409, flags, target],
            row,
        );
    }
    call(
        &mut m,
        &mut code,
        "VariantChangeTypeEx",
        &[DEST + 176, SOURCE + 176, 0x409, 0, 1],
        13,
    );
    code.mov_mem_imm(DATA + 0x800, 0)
        .mov_eax_mem(DATA + 0x800)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    for (address, delta) in [(CODE + 0x2000, 1u32), (CODE + 0x2100, u32::MAX)] {
        let mut callback = Asm::new(address);
        callback
            .mov_eax_mem(REFS)
            .add_eax_imm(delta)
            .mov_mem_eax(REFS)
            .push_imm(1)
            .call(sleep)
            .mov_eax_mem(REFS)
            .ret_n(4);
        m.cpu.write(address, &callback.bytes).unwrap();
    }
    m.spawn("Automation indirect ownership", CODE, &[], 65536)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for row in 0..14 {
        let expected = match row {
            4 | 5 => 0x80070057,
            10 => 0x8002000a,
            _ => 0,
        };
        assert_eq!(
            m.cpu.read_u32(DATA + row * 8).unwrap(),
            expected,
            "status row {row}"
        );
        assert_eq!(
            m.cpu.read_u32(DATA + row * 8 + 4).unwrap(),
            MARK,
            "LastError row {row}"
        );
    }
    for address in [DEST, DEST + 16, SOURCE] {
        assert_eq!(m.cpu.read_u16(address).unwrap(), 3);
        assert_eq!(m.cpu.read_u32(address + 8).unwrap(), 42);
    }
    assert_eq!(m.cpu.read_u16(DEST + 176).unwrap(), 1);
    let copied = m.cpu.read_u32(DEST + 40).unwrap();
    assert_ne!(copied, shared);
    assert_eq!(text(&m, copied), "shared");
    assert_eq!(m.cpu.read_u16(SOURCE + 64).unwrap(), 6);
    assert_eq!(u64_at(&m, SOURCE + 72), 25000);
    assert_eq!(m.cpu.read_u16(DEST + 80).unwrap(), 5);
    assert_eq!(f64::from_bits(u64_at(&m, DEST + 88)), 42.0);
    assert_eq!(f64::from_bits(u64_at(&m, DEST + 104)), 45351.0);
    let boolean = m.cpu.read_u32(DEST + 120).unwrap();
    assert_eq!(text(&m, boolean), "True");
    assert_eq!(m.cpu.read_u16(DEST + 128).unwrap(), 8);
    assert_eq!(m.cpu.read_u32(DEST + 136).unwrap(), preserved);
    assert!(owns_bstr(&env(&mut m), preserved));
    assert_eq!(m.cpu.read_u16(DEST + 144).unwrap(), 13);
    assert_eq!(m.cpu.read_u32(DEST + 152).unwrap(), OBJECT);
    let numeric = m.cpu.read_u32(DEST + 168).unwrap();
    assert_eq!(text(&m, numeric), "42");
    // The last replacement retired its prior COM reference; the indirect copy kept one.
    assert_eq!(m.cpu.read_u32(REFS).unwrap(), 1);
    for pointer in [copied, boolean, numeric, date, overflow, preserved] {
        free_bstr(&mut env(&mut m), pointer);
    }
}
#[test]
// spec: SYS-AUTOMATION-3
fn indirect_and_extended_variants_stage_ownership_and_preserve_failures_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
