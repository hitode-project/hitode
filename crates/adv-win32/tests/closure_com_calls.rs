//! OLE adapters execute actual guest QueryInterface/method/Release chains across waits.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OBJECT: u32 = DATA + 0x1000;
const TABLE: u32 = OBJECT + 16;
const REFS: u32 = TABLE + 64;
const BOUNDS: u32 = REFS + 16;
const NONE: u32 = BOUNDS + 16;
const CAPTURE: u32 = DATA + 0x2000;
fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[u32], row: u32) {
    let api = m.resolve_import("ole32.dll", name).unwrap();
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(api).mov_mem_eax(DATA + row * 4);
}
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::ole32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-com-call-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 16384, Prot::RW).unwrap();
    m.cpu.write_u32(OBJECT, TABLE).unwrap();
    m.cpu.write_u32(REFS, 1).unwrap();
    m.cpu.write_u32(NONE, TABLE + 32).unwrap();
    for (slot, offset) in [
        (0, 0x1000),
        (1, 0x1100),
        (2, 0x1200),
        (3, 0x1300),
        (4, 0x1400),
        (6, 0x1500),
        (8, 0x1600),
    ] {
        m.cpu.write_u32(TABLE + slot * 4, CODE + offset).unwrap();
    }
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    // The QI callback returns a real retained interface and then lets another thread run.
    let mut query = Asm::new(CODE + 0x1000);
    query.mov_eax_mem(REFS).add_eax_imm(1).mov_mem_eax(REFS);
    query.bytes.extend([0x8b, 0x44, 0x24, 0x08, 0x8b, 0x00]);
    query.mov_mem_eax(CAPTURE + 0x100);
    query.bytes.extend([0x8b, 0x44, 0x24, 0x0c, 0xc7, 0x00]);
    query.bytes.extend(OBJECT.to_le_bytes());
    query
        .push_imm(1)
        .call(sleep)
        .mov_mem_imm(DATA + 0x800, 0)
        .mov_eax_mem(DATA + 0x800)
        .ret_n(12);
    m.cpu.write(CODE + 0x1000, &query.bytes).unwrap();
    for (address, delta) in [(CODE + 0x1100, 1u32), (CODE + 0x1200, u32::MAX)] {
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
    for (address, count, output, status) in [
        (CODE + 0x1300, 11u32, CAPTURE, 0x123u32),
        (CODE + 0x1400, 9, CAPTURE + 0x40, 0x80070005),
        (CODE + 0x1500, 3, CAPTURE + 0x80, 0),
    ] {
        let mut callback = Asm::new(address);
        for i in 0..count {
            callback.bytes.extend([0x8b, 0x44, 0x24, (4 + i * 4) as u8]);
            callback.mov_mem_eax(output + i * 4);
        }
        callback
            .push_imm(1)
            .call(sleep)
            .mov_mem_imm(DATA + 0x804, status)
            .mov_eax_mem(DATA + 0x804)
            .ret_n((count * 4) as u16);
        m.cpu.write(address, &callback.bytes).unwrap();
    }
    let mut query = Asm::new(CODE + 0x1600);
    query.bytes.extend([0x8b, 0x44, 0x24, 0x0c, 0xc7, 0x00]);
    query.bytes.extend(0u32.to_le_bytes());
    query
        .mov_mem_imm(DATA + 0x808, 0x80004002)
        .mov_eax_mem(DATA + 0x808)
        .ret_n(12);
    m.cpu.write(CODE + 0x1600, &query.bytes).unwrap();
    let mut code = Asm::new(CODE);
    call(
        &mut m,
        &mut code,
        "OleDraw",
        &[OBJECT, 1, 0x7654, BOUNDS],
        0,
    );
    call(
        &mut m,
        &mut code,
        "CoSetProxyBlanket",
        &[OBJECT, 10, 0, 0, 2, 3, 0, 0],
        1,
    );
    call(&mut m, &mut code, "OleLockRunning", &[OBJECT, 1, 0], 2);
    call(&mut m, &mut code, "OleLockRunning", &[NONE, 1, 0], 3);
    call(
        &mut m,
        &mut code,
        "CoSetProxyBlanket",
        &[NONE, 10, 0, 0, 2, 3, 0, 0],
        4,
    );
    call(&mut m, &mut code, "OleDraw", &[NONE, 1, 0, BOUNDS], 5);
    call(
        &mut m,
        &mut code,
        "IsEqualGUID",
        &[CAPTURE + 0x200, CAPTURE + 0x210],
        6,
    );
    m.cpu.write_u32(CAPTURE + 0x220, 42).unwrap();
    call(
        &mut m,
        &mut code,
        "IsEqualGUID",
        &[CAPTURE + 0x200, CAPTURE + 0x220],
        7,
    );
    code.mov_mem_imm(DATA + 0x80c, 0)
        .mov_eax_mem(DATA + 0x80c)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("OLE real interface forwarding", CODE, &[], 65536)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (row, value) in [0x123, 0x80070005, 0, 0, 0x80004002, 0x80004002, 1, 0]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            m.cpu.read_u32(DATA + row as u32 * 4).unwrap(),
            value,
            "method result {row}"
        );
    }
    for (offset, expected) in [
        (
            0,
            vec![OBJECT, 1, u32::MAX, 0, 0, 0, 0x7654, BOUNDS, 0, 0, 0],
        ),
        (0x40, vec![OBJECT, OBJECT, 10, 0, 0, 2, 3, 0, 0]),
        (0x80, vec![OBJECT, 1, 0]),
    ] {
        for (i, value) in expected.into_iter().enumerate() {
            assert_eq!(
                m.cpu.read_u32(CAPTURE + offset + i as u32 * 4).unwrap(),
                value,
                "forwarded argument {offset:x}/{i}"
            );
        }
    }
    assert_eq!(m.cpu.read_u32(CAPTURE + 0x100).unwrap(), 0x126);
    assert_eq!(m.cpu.read_u32(REFS).unwrap(), 1);
}
#[test]
// spec: SYS-COM-2
fn ole_draw_proxy_and_running_lock_forward_and_balance_guest_interfaces_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
