mod support;
use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const SRC: u32 = 0x20000;
const DST: u32 = 0x30000;
const STACK: u32 = 0x5f800;

fn instruction(operation: u32) -> Vec<u8> {
    match operation {
        0 => vec![0xf3, 0xa4],
        1 => vec![0xf3, 0x66, 0xa5],
        2 => vec![0xf3, 0xa5],
        3 => vec![0xf3, 0xaa],
        4 => vec![0xf3, 0x66, 0xab],
        _ => vec![0xf3, 0xab],
    }
}
fn machine(operation: u32, jit: bool, len: usize) -> Executor {
    let mut m = Executor::new(CODE, len).unwrap();
    m.protect(CODE, len, READ | WRITE).unwrap();
    let mut bytes = instruction(operation);
    bytes.push(0xc3);
    m.write(CODE, &bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if jit {
        m.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    m
}
fn start(m: &mut Executor, count: u32, src: u32, dst: u32, flags: u32) {
    start_with_stack(m, count, src, dst, flags, STACK);
}
fn start_with_stack(m: &mut Executor, count: u32, src: u32, dst: u32, flags: u32, stack: u32) {
    m.state.eip = CODE;
    m.state.regs = [0xa1b2_c3d4, count, 0x1234, 0x2345, stack, 0x3456, src, dst];
    m.state.eflags = flags;
    m.write(stack, &STOP.to_le_bytes()).unwrap();
}
fn init(m: &mut Executor) {
    let bytes: Vec<u8> = (0..0x10000).map(|i| (i * 37 + i / 13) as u8).collect();
    m.write(SRC, &bytes).unwrap();
    m.write(DST, &[0x6d; 0x10000]).unwrap();
}
fn width(operation: u32) -> u32 {
    match operation % 3 {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

#[test]
fn forward_copies_and_fills_match_unicorn_for_aligned_unaligned_and_cross_page_ranges() {
    for operation in 0..6 {
        for (src_offset, dst_offset) in [(0, 0), (3, 5)] {
            for count in [0, 1, 2, 17, 1023, 1024, 1025, 8193] {
                let mut jit = machine(operation, true, 0x50000);
                let mut uc = machine(operation, false, 0x50000);
                for m in [&mut jit, &mut uc] {
                    init(m);
                    start(m, count, SRC + src_offset, DST + dst_offset, 0x8d7);
                }
                assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(
                    uc.run_unicorn(STOP, 100000).unwrap(),
                    RunOutcome::ReachedStop
                );
                assert_eq!(
                    jit.state.regs, uc.state.regs,
                    "operation {operation}, count {count}"
                );
                assert_eq!(jit.state.eflags, uc.state.eflags);
                assert_eq!(
                    jit.memory().read(DST, 0x10000).unwrap(),
                    uc.memory().read(DST, 0x10000).unwrap()
                );
            }
        }
    }
}

#[test]
fn zero_count_ignores_invalid_addresses_and_direction_flag() {
    for operation in 0..6 {
        for flags in [0x8d7, 0xcd7] {
            let mut jit = machine(operation, true, 0x50000);
            let mut uc = machine(operation, false, 0x50000);
            for m in [&mut jit, &mut uc] {
                start(m, 0, u32::MAX, 0xffff_fffe, flags);
            }
            assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(uc.run_unicorn(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(jit.state.regs, uc.state.regs);
            assert_eq!(jit.state.eflags, uc.state.eflags);
        }
    }
}

#[test]
fn backwards_ranges_retain_rep_semantics() {
    for operation in 0..6 {
        let mut jit = machine(operation, true, 0x50000);
        let mut uc = machine(operation, false, 0x50000);
        for m in [&mut jit, &mut uc] {
            init(m);
            start(m, 27, SRC + 0x700, DST + 0x800, 0xcd7);
        }
        assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(
            jit.memory().read(DST, 4096).unwrap(),
            uc.memory().read(DST, 4096).unwrap()
        );
        assert_eq!(jit.state.eflags, uc.state.eflags);
    }
}

#[test]
fn overlapping_copies_retain_element_order_and_flags() {
    for operation in 0..3 {
        for flags in [0x8d7, 0xcd7] {
            for delta in [0i32, 1, -1, 4, -4] {
                let mut jit = machine(operation, true, 0x50000);
                let mut uc = machine(operation, false, 0x50000);
                for m in [&mut jit, &mut uc] {
                    init(m);
                    start(
                        m,
                        27,
                        SRC + 0x100,
                        (SRC as i32 + 0x100 + delta) as u32,
                        flags,
                    );
                }
                assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(jit.state.regs, uc.state.regs);
                assert_eq!(
                    jit.memory().read(SRC, 4096).unwrap(),
                    uc.memory().read(SRC, 4096).unwrap()
                );
                assert_eq!(jit.state.eflags, uc.state.eflags);
            }
        }
    }
}

#[test]
fn protected_later_pages_preserve_unicorn_partial_fault_progress() {
    for operation in [0, 2, 3, 5] {
        for backwards in [false, true] {
            for read_fault in [false, true] {
                if read_fault && operation >= 3 {
                    continue;
                }
                let mut jit = machine(operation, true, 0x50000);
                let mut uc = machine(operation, false, 0x50000);
                for m in [&mut jit, &mut uc] {
                    init(m);
                    let offset = if backwards {
                        4096 + 16 - width(operation)
                    } else {
                        4096 - 16
                    };
                    start(
                        m,
                        32,
                        SRC + offset,
                        DST + offset,
                        if backwards { 0xcd7 } else { 0x8d7 },
                    );
                    let protected = if backwards { 0 } else { 4096 };
                    if read_fault {
                        m.protect(SRC + protected, 4096, 0).unwrap();
                    } else {
                        m.protect(DST + protected, 4096, READ).unwrap();
                    }
                }
                let actual = jit.run(STOP, 1000).unwrap();
                let expected = uc.run_unicorn(STOP, 1000).unwrap();
                assert_eq!(
                    actual, expected,
                    "operation {operation}, read_fault {read_fault}"
                );
                assert!(matches!(
                    actual,
                    RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                ));
                assert_eq!(jit.state.regs, uc.state.regs);
                assert_eq!(jit.state.eflags, uc.state.eflags);
                assert_eq!(jit.state.eip, CODE);
                assert_eq!(
                    jit.memory().read(DST, 8192).unwrap(),
                    uc.memory().read(DST, 8192).unwrap()
                );

                assert_eq!(jit.state.regs[1], 32 - 16 / width(operation));
            }
        }
    }
}

#[test]
fn overflowing_bulk_ranges_use_checked_native_elements() {
    for operation in [2, 5] {
        let mut jit = machine(operation, true, 0x50000);
        let mut uc = machine(operation, false, 0x50000);
        for m in [&mut jit, &mut uc] {
            init(m);
            start(m, 0x4000_0001, SRC, DST, 0x8d7);
        }
        assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::Budget);
        assert_eq!(uc.run_unicorn(STOP, 1).unwrap(), RunOutcome::Budget);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(
            jit.memory().read(DST, 16).unwrap(),
            uc.memory().read(DST, 16).unwrap()
        );

        assert_eq!(jit.state.regs[1], 0x4000_0000);
    }
    for operation in [0, 2, 3, 5] {
        for bad_source in [false, true] {
            if bad_source && operation >= 3 {
                continue;
            }
            let mut jit = machine(operation, true, 0x50000);
            let mut uc = machine(operation, false, 0x50000);
            for m in [&mut jit, &mut uc] {
                start(
                    m,
                    8,
                    if bad_source { 0xffff_fffc } else { SRC },
                    if bad_source { DST } else { 0xffff_fffc },
                    0x8d7,
                );
            }
            assert_eq!(
                jit.run(STOP, 100).unwrap(),
                uc.run_unicorn(STOP, 100).unwrap()
            );
            assert_eq!(jit.state.regs, uc.state.regs);
        }
    }
}

#[test]
fn writes_to_executable_pages_fall_back_and_invalidate_warm_code() {
    const TARGET: u32 = 0x12000;
    let mut m = machine(5, true, 0x50000);
    m.protect(TARGET, 4096, READ | WRITE | EXEC).unwrap();
    m.write(TARGET, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
    m.add_region(Region::new(TARGET, 6)).unwrap();
    start(&mut m, 1, SRC, DST, 2);
    m.state.eip = TARGET;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 1);
    start(&mut m, 1, SRC, TARGET + 1, 2);
    m.state.regs[0] = 73;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);

    start(&mut m, 1, SRC, DST, 2);
    m.state.eip = TARGET;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 73);
}

#[test]
fn bulk_deadline_stops_at_a_restartable_rep_then_finishes_after_rearming() {
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Backend, Exit, Guest, GuestExt, Prot, Reg, TrapAction, TrapHandler};
    use std::time::{Duration, Instant};
    struct NoTrap;
    impl TrapHandler for NoTrap {
        fn on_trap(&mut self, _: &mut dyn Guest, _: u8) -> TrapAction {
            panic!("unexpected trap")
        }
    }
    const BUFFER: u32 = 0x6100_0000;
    const SIZE: u32 = 64 << 20;
    let mut cpu = JitCpu::new(STOP, true).unwrap();
    cpu.map(CODE, 4096, Prot::RX).unwrap();
    cpu.map(STOP, 4096, Prot::RX).unwrap();
    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
    cpu.map(BUFFER, SIZE, Prot::RW).unwrap();
    cpu.write(CODE, &[0xf3, 0xaa, 0xc3]).unwrap();
    cpu.executor_mut().add_region(Region::new(CODE, 3)).unwrap();
    cpu.write_u32(STACK, STOP).unwrap();
    cpu.set_reg(Reg::Esp, STACK);
    cpu.set_reg(Reg::Eip, CODE);
    cpu.set_reg(Reg::Eax, 0x44);
    cpu.set_reg(Reg::Ecx, SIZE);
    cpu.set_reg(Reg::Edi, BUFFER);
    cpu.set_reg(Reg::Eflags, 0x8d7);
    let budget = Duration::from_millis(1);
    cpu.set_execution_budget(Some(budget));
    let start = Instant::now();
    assert_eq!(
        cpu.run(&mut NoTrap),
        Exit::BudgetExceeded { pc: CODE, budget }
    );
    assert!(start.elapsed() < Duration::from_millis(250));
    assert!(cpu.reg(Reg::Ecx) > 0);
    assert_eq!(cpu.reg(Reg::Ecx) + cpu.reg(Reg::Edi) - BUFFER, SIZE);
    assert_eq!(cpu.reg(Reg::Eflags), 0x8d7);
    cpu.set_execution_budget(None);
    assert_eq!(cpu.run(&mut NoTrap), Exit::Parked);
    assert_eq!(cpu.reg(Reg::Ecx), 0);
    assert_eq!(cpu.reg(Reg::Edi), BUFFER + SIZE);
    assert_eq!(cpu.reg(Reg::Eflags), 0x8d7);
    for at in [BUFFER, BUFFER + SIZE - 16] {
        let mut bytes = [0; 16];
        cpu.read(at, &mut bytes).unwrap();
        assert_eq!(bytes, [0x44; 16]);
    }
}

#[test]
fn a_host_call_that_expires_the_budget_does_not_start_following_bulk_writes() {
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Backend, Exit, Guest, GuestExt, Prot, Reg, TrapAction, TrapHandler};
    use std::time::Duration;
    struct Slow;
    impl TrapHandler for Slow {
        fn on_trap(&mut self, _: &mut dyn Guest, vector: u8) -> TrapAction {
            assert_eq!(vector, 0x2e);
            std::thread::sleep(Duration::from_millis(400));
            TrapAction::Continue
        }
    }
    let mut cpu = JitCpu::new(STOP, true).unwrap();
    cpu.map(CODE, 4096, Prot::RX).unwrap();
    cpu.map(STOP, 4096, Prot::RX).unwrap();
    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
    cpu.map(DST, 4096, Prot::RW).unwrap();
    cpu.write(CODE, &[0xcd, 0x2e, 0xf3, 0xaa, 0xc3]).unwrap();
    cpu.executor_mut().add_region(Region::new(CODE, 5)).unwrap();
    cpu.write_u32(STACK, STOP).unwrap();
    cpu.set_reg(Reg::Esp, STACK);
    cpu.set_reg(Reg::Eip, CODE);
    cpu.set_reg(Reg::Eax, 0x44);
    cpu.set_reg(Reg::Ecx, 4096);
    cpu.set_reg(Reg::Edi, DST);
    // Wide enough that translating the first block never spends it.
    let budget = Duration::from_millis(200);
    cpu.set_execution_budget(Some(budget));
    assert_eq!(
        cpu.run(&mut Slow),
        Exit::BudgetExceeded {
            pc: CODE + 2,
            budget
        }
    );
    assert_eq!(cpu.reg(Reg::Ecx), 4096);
    assert_eq!(cpu.reg(Reg::Edi), DST);
    let mut bytes = [0; 16];
    cpu.read(DST, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 16]);
}

#[test]
fn native_address_size_and_fs_prefix_keep_zero_count_in_native_code() {
    // Address-size 16 needs SI/DI/CX, and FS changes the MOVS source base.
    for bytes in [vec![0x67, 0xf3, 0xa4, 0xc3], vec![0x64, 0xf3, 0xa4, 0xc3]] {
        let mut jit = machine(0, true, 0x50000);
        let mut uc = machine(0, false, 0x50000);
        for m in [&mut jit, &mut uc] {
            m.write(CODE, &bytes).unwrap();
            start(m, 0, SRC, DST, 0x8d7);
        }
        jit.set_region(Region::new(CODE, bytes.len())).unwrap();
        assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(uc.run_unicorn(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(jit.state.regs, uc.state.regs);
    }
}
