//! Actual stdcall/cdecl/vtable invocation, 64-bit registers and x87 results across guest waits.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TYPES: u32 = DATA + 0x1000;
const VALUES: u32 = TYPES + 32;
const LEFT: u32 = VALUES + 32;
const RIGHT: u32 = LEFT + 16;
const RESULT: u32 = DATA + 0x1200;
const OBJECT: u32 = DATA + 0x1400;
const TABLE: u32 = OBJECT + 16;
const THIS: u32 = TABLE + 16;
fn variant<B: Backend>(m: &mut Machine<B>, code: &mut Asm, address: u32, kind: u32, value: u64) {
    code.mov_mem_imm(address, kind)
        .mov_mem_imm(address + 8, value as u32)
        .mov_mem_imm(address + 12, (value >> 32) as u32);
    m.cpu.write_u32(address, kind).unwrap();
}
fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, args: &[u32], row: u32) {
    let api = m.resolve_import("oleaut32.dll", "DispCallFunc").unwrap();
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(api).mov_mem_eax(DATA + row * 4);
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
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-automation-call-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.write_u32(VALUES, LEFT).unwrap();
    m.cpu.write_u32(VALUES + 4, RIGHT).unwrap();
    m.cpu.write_u32(OBJECT, TABLE).unwrap();
    m.cpu.write_u32(OBJECT + 4, 100).unwrap();
    m.cpu.write_u32(TABLE + 4, CODE + 0x1400).unwrap();
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    for (address, convention) in [(CODE + 0x1000, 4), (CODE + 0x1100, 1)] {
        let mut callback = Asm::new(address);
        callback.push_imm(1).call(sleep);
        callback
            .bytes
            .extend([0x8b, 0x44, 0x24, 0x04, 0x03, 0x44, 0x24, 0x08]);
        if convention == 4 {
            callback.ret_n(8);
        } else {
            callback.ret();
        }
        m.cpu.write(address, &callback.bytes).unwrap();
    }
    // Two double stack arguments; result stays in the guest x87 stack.
    let mut callback = Asm::new(CODE + 0x1200);
    callback.push_imm(1).call(sleep);
    callback
        .bytes
        .extend([0xdd, 0x44, 0x24, 0x04, 0xdc, 0x44, 0x24, 0x0c]);
    callback.ret_n(16);
    m.cpu.write(CODE + 0x1200, &callback.bytes).unwrap();
    let mut callback = Asm::new(CODE + 0x1300);
    callback.push_imm(1).call(sleep);
    callback.bytes.extend([
        0x8b, 0x44, 0x24, 0x04, 0x03, 0x44, 0x24, 0x0c, 0x8b, 0x54, 0x24, 0x08, 0x13, 0x54, 0x24,
        0x10,
    ]);
    callback.ret_n(16);
    m.cpu.write(CODE + 0x1300, &callback.bytes).unwrap();
    let mut callback = Asm::new(CODE + 0x1400);
    callback.push_imm(1).call(sleep);
    callback.bytes.extend([0x8b, 0x44, 0x24, 0x04]);
    callback.mov_mem_eax(THIS).add_eax_imm(4).mov_eax_ptr_eax();
    callback
        .bytes
        .extend([0x03, 0x44, 0x24, 0x08, 0x03, 0x44, 0x24, 0x0c]);
    callback.ret_n(12);
    m.cpu.write(CODE + 0x1400, &callback.bytes).unwrap();
    let mut callback = Asm::new(CODE + 0x1500);
    callback.mov_mem_imm(THIS + 4, 42).ret();
    m.cpu.write(CODE + 0x1500, &callback.bytes).unwrap();
    let mut code = Asm::new(CODE);
    variant(&mut m, &mut code, LEFT, 3, (-7i64) as u64);
    variant(&mut m, &mut code, RIGHT, 3, 12);
    code.mov_mem_imm(TYPES, 3 | (3 << 16));
    for (row, instance, offset, convention) in [
        (0, 0, CODE + 0x1000, 4),
        (1, 0, CODE + 0x1100, 1),
        (2, OBJECT, 4, 4),
    ] {
        call(
            &mut m,
            &mut code,
            &[
                instance,
                offset,
                convention,
                3,
                2,
                TYPES,
                VALUES,
                RESULT + row * 16,
            ],
            row,
        );
    }
    variant(&mut m, &mut code, LEFT, 5, 1.5f64.to_bits());
    variant(&mut m, &mut code, RIGHT, 5, 2.25f64.to_bits());
    code.mov_mem_imm(TYPES, 5 | (5 << 16));
    call(
        &mut m,
        &mut code,
        &[0, CODE + 0x1200, 4, 5, 2, TYPES, VALUES, RESULT + 48],
        3,
    );
    variant(&mut m, &mut code, LEFT, 20, 0x100000002);
    variant(&mut m, &mut code, RIGHT, 20, 0x200000003);
    code.mov_mem_imm(TYPES, 20 | (20 << 16));
    call(
        &mut m,
        &mut code,
        &[0, CODE + 0x1300, 4, 20, 2, TYPES, VALUES, RESULT + 64],
        4,
    );
    call(
        &mut m,
        &mut code,
        &[0, CODE + 0x1500, 4, 0, 0, 0, 0, RESULT + 80],
        5,
    );
    code.mov_mem_imm(RESULT + 96, 0xcdcdcdcd);
    call(
        &mut m,
        &mut code,
        &[0, CODE + 0x1000, 4, 0xffff, 0, 0, 0, RESULT + 96],
        6,
    );
    code.mov_mem_imm(DATA + 0x800, 0)
        .mov_eax_mem(DATA + 0x800)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("Automation actual guest calls", CODE, &[], 65536)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for row in 0..7 {
        assert_eq!(
            m.cpu.read_u32(DATA + row * 4).unwrap(),
            if row == 6 { 0x80020008 } else { 0 },
            "call status {row}"
        );
    }
    for (row, kind, value) in [
        (0, 3, 5u64),
        (1, 3, 5),
        (2, 3, 105),
        (3, 5, 3.75f64.to_bits()),
        (4, 20, 0x300000005),
        (5, 0, 0),
    ] {
        assert_eq!(
            m.cpu.read_u16(RESULT + row * 16).unwrap(),
            kind,
            "result kind {row}"
        );
        if kind != 0 {
            assert_eq!(
                u64_at(&m, RESULT + row * 16 + 8),
                value,
                "result value {row}"
            );
        }
    }
    assert_eq!(m.cpu.read_u32(THIS + 4).unwrap(), 42);
    assert_eq!(m.cpu.read_u32(THIS).unwrap(), OBJECT);
    assert_eq!(m.cpu.read_u32(RESULT + 96).unwrap(), 0xcdcdcdcd);
}
#[test]
// spec: SYS-AUTOMATION-4
fn dispatch_calls_execute_guest_abi_and_resume_scalar_results_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
