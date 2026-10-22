use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const ZONE: u32 = DATA + 0x800;
const INPUT: u32 = DATA + 0xa00;
const OUTPUT: u32 = DATA + 0xc00;
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        crate::Win32Config::new(".", "fixture.exe", "target/closure-timezone-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    // Authored UTC-5 rules: second Sunday in March, first Sunday in November.
    m.cpu.write_u32(ZONE, 300).unwrap();
    m.cpu.write_u32(ZONE + 168, (-60i32) as u32).unwrap();
    for (at, st) in [
        (ZONE + 68, [0u16, 11, 0, 1, 2, 0, 0, 0]),
        (ZONE + 152, [0u16, 3, 0, 2, 2, 0, 0, 0]),
        (INPUT, [2024u16, 1, 0, 1, 20, 0, 0, 0]),
        (INPUT + 32, [2024u16, 7, 0, 1, 12, 0, 0, 0]),
        (INPUT + 64, [2024u16, 3, 0, 10, 6, 59, 59, 0]),
        (INPUT + 96, [2024u16, 3, 0, 10, 7, 0, 0, 0]),
    ] {
        m.cpu
            .write(
                at,
                &st.into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    let api = m
        .resolve_import("kernel32.dll", "SystemTimeToTzSpecificLocalTime")
        .unwrap();
    let mut code = Asm::new(CODE);
    for (i, (zone, input)) in [
        (0, INPUT),
        (ZONE, INPUT),
        (ZONE, INPUT + 32),
        (ZONE, INPUT + 64),
        (ZONE, INPUT + 96),
    ]
    .into_iter()
    .enumerate()
    {
        code.push_imm(OUTPUT + i as u32 * 32)
            .push_imm(input)
            .push_imm(zone)
            .call(api)
            .mov_mem_eax(DATA + i as u32 * 4);
    }
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("UTC conversions", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (year, month, day, hour, minute, second)) in [
        (2024, 1, 2, 5, 0, 0),
        (2024, 1, 1, 15, 0, 0),
        (2024, 7, 1, 8, 0, 0),
        (2024, 3, 10, 1, 59, 59),
        (2024, 3, 10, 3, 0, 0),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(m.cpu.read_u32(DATA + i as u32 * 4).unwrap(), 1);
        let at = OUTPUT + i as u32 * 32;
        let st = (0..8)
            .map(|n| m.cpu.read_u16(at + n * 2).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            (st[0], st[1], st[3], st[4], st[5], st[6]),
            (year, month, day, hour, minute, second)
        );
    }
}
#[test]
// spec: SYS-CORE-2
fn utc_conversion_uses_profile_bias_and_real_daylight_transition_boundaries_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
