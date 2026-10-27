mod support;
use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}
fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut r = Region::new(CODE, bytes.len());
        r.compile_strategy = strategy;
        m.add_region(r).unwrap();
    }
    m
}
fn reset(m: &mut Executor, low: u32, high: u32, divisor: u32, address: u32) {
    m.state = CpuState {
        regs: [
            low,
            divisor,
            high,
            0x1234_5678,
            STACK,
            0x8765_4321,
            7,
            address,
        ],
        eip: CODE,
        eflags: 0xed7,
        ..CpuState::default()
    };
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

#[test]
fn unsigned_divide_matches_unicorn_and_rust_for_narrow_wide_and_eax_alias_sources() {
    let divisors = [
        1,
        2,
        3,
        7,
        255,
        256,
        65535,
        65536,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
    ];
    let lows = [0, 1, 2, 254, 255, 65535, 0x7fff_ffff, 0x8000_0000, u32::MAX];
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        for form in 0..3 {
            let bytes = code(|a| {
                match form {
                    0 => a.div(ecx),
                    1 => a.div(dword_ptr(edi)),
                    _ => a.div(eax),
                }
                .unwrap();
                a.ret().unwrap();
            });
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            for divisor in divisors {
                for low in lows {
                    let actual_divisor = if form == 2 { low } else { divisor };
                    if actual_divisor == 0 {
                        continue;
                    }
                    for high in [0, 1, actual_divisor - 1] {
                        if high >= actual_divisor {
                            continue;
                        }
                        for m in [&mut jit, &mut reference] {
                            reset(m, low, high, divisor, DATA + 4094);
                            m.write(DATA + 4094, &divisor.to_le_bytes()).unwrap();
                        }
                        assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                        assert_eq!(
                            reference.run_unicorn(STOP, 100).unwrap(),
                            RunOutcome::ReachedStop
                        );
                        assert_eq!(jit.state.regs, reference.state.regs);
                        let dividend = (u64::from(high) << 32) | u64::from(low);
                        assert_eq!(
                            jit.state.regs[0],
                            (dividend / u64::from(actual_divisor)) as u32
                        );
                        assert_eq!(
                            jit.state.regs[2],
                            (dividend % u64::from(actual_divisor)) as u32
                        );
                        assert_eq!(
                            jit.state.eflags, 0xed7,
                            "native preserves undefined arithmetic flags"
                        );
                        assert_eq!(jit.state.eflags & !0x8d5, reference.state.eflags & !0x8d5);
                    }
                }
            }
        }
    }
}

#[test]
fn divide_source_fault_precedes_division_checks_and_preserves_all_registers() {
    let bytes = code(|a| {
        a.div(dword_ptr(edi)).unwrap();
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut reference = machine(&bytes, None);
        for m in [&mut jit, &mut reference] {
            reset(m, u32::MAX, u32::MAX, 0, DATA);
            m.write(DATA, &[0; 4]).unwrap();
            m.protect(DATA, 4096, 0).unwrap();
        }
        let before = jit.state.clone();
        let expected = RunOutcome::ReadFault {
            pc: CODE,
            address: DATA,
        };
        assert_eq!(jit.run(STOP, 100).unwrap(), expected);
        assert_eq!(reference.run_unicorn(STOP, 100).unwrap(), expected);
        assert_eq!(jit.state.regs, before.regs);
        assert_eq!(jit.state.eflags, before.eflags);
        assert_eq!(jit.state.regs, reference.state.regs);
    }
}

#[test]
fn short_divide_fallbacks_mix_with_native_signed_division() {
    let bytes = code(|a| {
        a.div(cl).unwrap();
        a.mov(dx, 0).unwrap();
        a.div(cx).unwrap();
        a.mov(edx, 0).unwrap();
        a.idiv(ecx).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, Some(CompileStrategy::Speed));
    let mut reference = machine(&bytes, None);
    for m in [&mut jit, &mut reference] {
        reset(m, 127, 0, 3, DATA);
    }
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        reference.run_unicorn(STOP, 100).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, reference.state.regs);
}

#[test]
fn signed_divide_handles_signs_aliases_and_exact_exception_boundaries() {
    let dividends: [i64; 12] = [
        i64::MIN,
        i64::MIN + 1,
        -0x1_0000_0001,
        -0x8000_0001,
        -0x8000_0000,
        -101,
        -1,
        0,
        101,
        0x7fff_ffff,
        0x1_0000_0001,
        i64::MAX,
    ];
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for form in 0..4 {
            let bytes = code(|a| {
                match form {
                    0 => a.idiv(ecx),
                    1 => a.idiv(dword_ptr(edi)),
                    2 => a.idiv(eax),
                    _ => a.idiv(edx),
                }
                .unwrap();
                a.ret().unwrap();
            });
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            for dividend in dividends {
                for divisor in [i32::MIN, -7, -1, 0, 1, 7, i32::MAX] {
                    let low = dividend as u32;
                    let high = (dividend >> 32) as u32;
                    let actual = match form {
                        2 => low as i32,
                        3 => high as i32,
                        _ => divisor,
                    };
                    for m in [&mut jit, &mut reference] {
                        reset(m, low, high, divisor as u32, DATA + 4094);
                        m.write(DATA + 4094, &divisor.to_le_bytes()).unwrap();
                    }
                    let before = jit.state.clone();
                    let quotient = (actual != 0).then(|| i128::from(dividend) / i128::from(actual));
                    if quotient.is_none_or(|q| q < i128::from(i32::MIN) || q > i128::from(i32::MAX))
                    {
                        assert_eq!(
                            jit.run(STOP, 100).unwrap(),
                            RunOutcome::Trap {
                                pc: CODE,
                                vector: 0
                            }
                        );
                        assert_eq!(jit.state.regs, before.regs);
                        assert_eq!(jit.state.eflags, before.eflags);
                        assert_eq!(jit.state.eip, CODE);
                    } else {
                        assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                        assert_eq!(
                            reference.run_unicorn(STOP, 100).unwrap(),
                            RunOutcome::ReachedStop
                        );
                        assert_eq!(
                            jit.state.regs, reference.state.regs,
                            "{dividend}/{actual}, {form}"
                        );
                        assert_eq!(jit.state.regs[0] as i32, quotient.unwrap() as i32);
                        assert_eq!(
                            jit.state.regs[2] as i32,
                            (i128::from(dividend) % i128::from(actual)) as i32
                        );
                        assert_eq!(jit.state.eflags, before.eflags);
                    }
                }
            }
            reset(&mut jit, 1, 0, 0, DATA);
            if form == 1 {
                jit.protect(DATA, 4096, 0).unwrap();
                let before = jit.state.clone();
                assert_eq!(
                    jit.run(STOP, 100).unwrap(),
                    RunOutcome::ReadFault {
                        pc: CODE,
                        address: DATA
                    }
                );
                assert_eq!(jit.state.regs, before.regs);
                assert_eq!(jit.state.eflags, before.eflags);
            }
        }
    }
}

#[test]
fn divide_zero_and_quotient_overflow_fall_back_at_original_pc_for_guest_exception_recovery() {
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Backend, Exit, Guest, GuestExt, Prot, Reg, TrapAction, TrapHandler};

    struct Recover {
        expected: [u32; 3],
        next: u32,
        calls: usize,
    }
    impl TrapHandler for Recover {
        fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
            assert_eq!(vector, 0);
            assert_eq!(cpu.reg(Reg::Eip), CODE);
            assert_eq!(
                [cpu.reg(Reg::Eax), cpu.reg(Reg::Edx), cpu.reg(Reg::Ecx)],
                self.expected
            );
            cpu.set_reg(Reg::Eip, self.next);
            cpu.set_reg(Reg::Eax, 91);
            cpu.set_reg(Reg::Edx, 73);
            self.calls += 1;
            TrapAction::Continue
        }
    }
    for form in 0..4 {
        let bytes = code(|a| {
            match form {
                0 => a.div(ecx),
                1 => a.div(dword_ptr(edi)),
                2 => a.div(eax),
                _ => a.div(edx),
            }
            .unwrap();
            a.ret().unwrap();
        });
        let next = CODE + bytes.len() as u32 - 1;
        for (low, high, divisor) in [(0, 0, 0), (u32::MAX, 1, 1), (9, 7, 7), (1, u32::MAX, 2)] {
            let actual_divisor = match form {
                2 => low,
                3 => high,
                _ => divisor,
            };
            if actual_divisor != 0 && high < actual_divisor {
                continue;
            }
            for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
                for jit in [false, true] {
                    let mut cpu = JitCpu::new(STOP, jit).unwrap();
                    cpu.map(CODE, 4096, Prot::RX).unwrap();
                    cpu.map(STOP, 4096, Prot::RX).unwrap();
                    cpu.map(DATA, 4096, Prot::RW).unwrap();
                    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
                    cpu.write(CODE, &bytes).unwrap();
                    cpu.write_u32(DATA, divisor).unwrap();
                    cpu.write_u32(STACK, STOP).unwrap();
                    cpu.set_reg(Reg::Eip, CODE);
                    cpu.set_reg(Reg::Esp, STACK);
                    cpu.set_reg(Reg::Eax, low);
                    cpu.set_reg(Reg::Edx, high);
                    cpu.set_reg(Reg::Ecx, divisor);
                    cpu.set_reg(Reg::Edi, DATA);
                    if jit {
                        let mut r = Region::new(CODE, bytes.len());
                        r.compile_strategy = strategy;
                        cpu.executor_mut().add_region(r).unwrap();
                    }
                    let mut recover = Recover {
                        expected: [low, high, divisor],
                        next,
                        calls: 0,
                    };
                    assert_eq!(cpu.run(&mut recover), Exit::Parked);
                    assert_eq!(recover.calls, 1);
                    assert_eq!(cpu.reg(Reg::Eax), 91);
                    assert_eq!(cpu.reg(Reg::Edx), 73);
                }
            }
        }
    }
}
