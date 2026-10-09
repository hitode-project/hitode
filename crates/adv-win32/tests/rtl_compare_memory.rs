//! RTL prefix counts use guest reads and stop before bytes beyond a mismatch.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

fn cases<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    const CODE: u32 = 0x400000;
    const DATA: u32 = 0x410000;
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write(DATA, b"ab\0de!").unwrap();
    m.cpu.write(DATA + 64, b"ab\0df!").unwrap();
    m.cpu.write(DATA + 4095, b"z").unwrap();
    let mut a = Asm::new(CODE);
    for (module_index, module) in ["ntdll.dll", "kernel32.dll"].into_iter().enumerate() {
        let f = m.resolve_import(module, "RtlCompareMemory").unwrap();
        for (index, (left, right, length)) in [
            (0xffff_ffff, 0, 0),
            (DATA, DATA + 64, 6),
            (DATA, DATA + 64, 3),
            (DATA, DATA, 6),
            (DATA, DATA + 4095, 20),
        ]
        .into_iter()
        .enumerate()
        {
            a.push_imm(length).push_imm(right).push_imm(left).call(f);
            a.mov_mem_eax(DATA + 128 + (module_index * 5 + index) as u32 * 4);
        }
    }
    a.bytes.extend([0x31, 0xc0]);
    a.ret();
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("RTL comparison", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for index in 0..10 {
        assert_eq!(
            m.cpu.read_u32(DATA + 128 + index * 4).unwrap(),
            [0, 4, 3, 6, 0][index as usize % 5]
        );
    }
}

#[test]
fn matching_prefix_counts_preserve_nuls_and_stop_at_first_difference() {
    cases(UnicornCpu::new(PARK_ADDR).unwrap());
    cases(JitCpu::new(PARK_ADDR, false).unwrap());
    cases(JitCpu::new(PARK_ADDR, true).unwrap());
}
