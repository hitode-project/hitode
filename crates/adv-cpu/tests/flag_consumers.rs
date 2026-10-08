mod support;
use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;
const VALUES: [u32; 11] = [
    0,
    1,
    2,
    15,
    16,
    0x7fff_fffe,
    0x7fff_ffff,
    0x8000_0000,
    0x8000_0001,
    0xffff_fffe,
    0xffff_ffff,
];

fn assemble(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
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
fn reset(m: &mut Executor, left: u32, right: u32, flags: u32, address: u32) {
    m.state = CpuState {
        regs: [
            left,
            right,
            left,
            right,
            STACK,
            0x5678_1234,
            0xaabb_ccdd,
            address,
        ],
        eflags: flags,
        eip: CODE,
        ..CpuState::default()
    };
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}
fn compare(jit: &mut Executor, reference: &mut Executor) {
    assert_eq!(jit.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        reference.run_unicorn(STOP, 10_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, reference.state.regs);
    assert_eq!(jit.state.eflags, reference.state.eflags);
}

macro_rules! setcc {
    ($asm:expr, $condition:expr, $destination:expr) => {
        match $condition {
            0 => $asm.seto($destination),
            1 => $asm.setno($destination),
            2 => $asm.setb($destination),
            3 => $asm.setae($destination),
            4 => $asm.sete($destination),
            5 => $asm.setne($destination),
            6 => $asm.setbe($destination),
            7 => $asm.seta($destination),
            8 => $asm.sets($destination),
            9 => $asm.setns($destination),
            10 => $asm.setp($destination),
            11 => $asm.setnp($destination),
            12 => $asm.setl($destination),
            13 => $asm.setge($destination),
            14 => $asm.setle($destination),
            15 => $asm.setg($destination),
            _ => unreachable!(),
        }
        .unwrap()
    };
}

fn carry_control(a: &mut CodeAssembler, form: usize) {
    match form {
        0 => a.clc(),
        1 => a.stc(),
        _ => a.cmc(),
    }
    .unwrap();
}

#[test]
fn carry_controls_preserve_other_flags_and_feed_branches_and_setcc() {
    for producer in 0..7 {
        for carry in 0..3 {
            let bytes = assemble(|a| {
                match producer {
                    0 => Ok(()),
                    1 => a.add(eax, ecx),
                    2 => a.cmp(eax, ecx),
                    3 => a.add(al, cl),
                    4 => a.cmp(ax, cx),
                    5 => a.inc(eax),
                    _ => a.xor(eax, ecx),
                }
                .unwrap();
                carry_control(a, carry);
                let mut carried = a.create_label();
                let mut joined = a.create_label();
                a.jb(carried).unwrap();
                a.mov(esi, 1).unwrap();
                a.jmp(joined).unwrap();
                a.set_label(&mut carried).unwrap();
                a.mov(esi, 2).unwrap();
                a.set_label(&mut joined).unwrap();
                for condition in 0..16 {
                    setcc!(a, condition, byte_ptr(DATA as i32 + condition));
                }
                a.ret().unwrap();
            });
            let cases: Vec<_> = if producer == 0 {
                (0..64)
                    .map(|bits| {
                        let mut flags = 0x602;
                        for (index, bit) in [0, 2, 4, 6, 7, 11].into_iter().enumerate() {
                            flags |= ((bits >> index) & 1) << bit;
                        }
                        (0xa5a5_a5a5, 0x5a5a_5a5a, flags)
                    })
                    .collect()
            } else {
                [0, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX]
                    .into_iter()
                    .flat_map(|left| VALUES.map(|right| (left, right, 0xed7)))
                    .collect()
            };
            for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
                let mut jit = machine(&bytes, Some(strategy));
                let mut reference = machine(&bytes, None);
                for &(left, right, flags) in &cases {
                    for m in [&mut jit, &mut reference] {
                        reset(m, left, right, flags, DATA + 64);
                    }
                    compare(&mut jit, &mut reference);
                    assert_eq!(
                        jit.memory().read(DATA, 16).unwrap(),
                        reference.memory().read(DATA, 16).unwrap(),
                        "{strategy:?}, producer={producer}, carry={carry}, {left:#x}, {right:#x}"
                    );
                }
                assert_eq!(jit.stats.interpreter_entries, 0);
            }
        }
    }
}

#[test]
fn carry_controls_chain_through_inc_dec_adc_sbb_and_a_loop_merge() {
    let bytes = assemble(|a| {
        a.cmp(edx, ebx).unwrap();
        a.cmc().unwrap();
        a.inc(esi).unwrap();
        a.dec(ebp).unwrap();
        a.adc(eax, ecx).unwrap();
        a.stc().unwrap();
        a.sbb(esi, ebx).unwrap();
        a.clc().unwrap();
        a.mov(edi, 5).unwrap();
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.adc(eax, ecx).unwrap();
        a.cmc().unwrap();
        a.inc(esi).unwrap();
        a.dec(edi).unwrap();
        a.jnz(top).unwrap();
        a.cmc().unwrap();
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut reference = machine(&bytes, None);
        for left in VALUES {
            for right in VALUES {
                for flags in [0x202, 0xed7] {
                    for m in [&mut jit, &mut reference] {
                        reset(m, left, right, flags, DATA + 64);
                    }
                    compare(&mut jit, &mut reference);
                }
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0);
    }
}

#[test]
fn sbb_forms_match_results_all_flags_and_live_conditions_at_borrow_boundaries() {
    carry_forms_match(true);
}

#[test]
fn adc_forms_match_results_all_flags_and_live_conditions_at_carry_boundaries() {
    carry_forms_match(false);
}

fn carry_forms_match(subtract: bool) {
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        for form in 0..8 {
            let bytes = assemble(|a| {
                if subtract {
                    match form {
                        0 => a.sbb(eax, ecx),
                        1 => a.sbb(eax, eax),
                        2 => a.sbb(eax, -1),
                        3 => a.sbb(eax, i32::MIN),
                        4 => a.sbb(eax, dword_ptr(edi)),
                        5 => a.sbb(dword_ptr(edi), ecx),
                        6 => a.sbb(dword_ptr(edi), -1),
                        _ => a.sbb(dword_ptr(edi), i32::MIN),
                    }
                } else {
                    match form {
                        0 => a.adc(eax, ecx),
                        1 => a.adc(eax, eax),
                        2 => a.adc(eax, -1),
                        3 => a.adc(eax, i32::MIN),
                        4 => a.adc(eax, dword_ptr(edi)),
                        5 => a.adc(dword_ptr(edi), ecx),
                        6 => a.adc(dword_ptr(edi), -1),
                        _ => a.adc(dword_ptr(edi), i32::MIN),
                    }
                }
                .unwrap();
                for condition in 0..16 {
                    setcc!(a, condition, byte_ptr(DATA as i32 + condition));
                }
                a.ret().unwrap();
            });
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            for left in VALUES {
                for right in VALUES {
                    for carry in [0, 1] {
                        for m in [&mut jit, &mut reference] {
                            reset(m, left, right, 0xed6 | carry, DATA + 4094);
                            let memory = if form == 4 { right } else { left };
                            m.write(DATA + 4094, &memory.to_le_bytes()).unwrap();
                        }
                        compare(&mut jit, &mut reference);
                        assert_eq!(
                            jit.memory().read(DATA + 4094, 4).unwrap(),
                            reference.memory().read(DATA + 4094, 4).unwrap(),
                            "{strategy:?}, form {form}, {left:#x}-{right:#x}-{carry}"
                        );
                        assert_eq!(
                            jit.memory().read(DATA, 16).unwrap(),
                            reference.memory().read(DATA, 16).unwrap()
                        );
                    }
                }
            }
            assert_eq!(
                jit.stats.interpreter_entries, 0,
                "carry form {form}, subtract={subtract}"
            );
        }
    }
}

#[test]
fn sbb_chain_uses_lazy_carry_preserved_by_inc_dec_then_feeds_setcc() {
    let bytes = assemble(|a| {
        a.cmp(edx, ebx).unwrap();
        a.inc(esi).unwrap();
        a.dec(ebp).unwrap();
        a.sbb(eax, eax).unwrap();
        a.sbb(eax, -1).unwrap();
        for condition in 0..16 {
            setcc!(a, condition, byte_ptr(DATA as i32 + condition));
        }
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut reference = machine(&bytes, None);
        for left in VALUES {
            for right in VALUES {
                for m in [&mut jit, &mut reference] {
                    reset(m, left, right, 0xed7, DATA + 64);
                }
                compare(&mut jit, &mut reference);
                assert_eq!(jit.state.regs[0], if left < right { u32::MAX } else { 1 });
                assert_eq!(
                    jit.memory().read(DATA, 16).unwrap(),
                    reference.memory().read(DATA, 16).unwrap()
                );
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0);
    }
}

#[test]
fn adc_chain_preserves_lazy_carry_across_inc_dec_and_faulting_stores() {
    let bytes = assemble(|a| {
        a.add(edx, ebx).unwrap();
        a.inc(esi).unwrap();
        a.dec(ebp).unwrap();
        a.adc(eax, ecx).unwrap();
        a.adc(eax, -1).unwrap();
        a.adc(dword_ptr(edi), ecx).unwrap();
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut reference = machine(&bytes, None);
        for left in VALUES {
            for right in VALUES {
                for m in [&mut jit, &mut reference] {
                    reset(m, left, right, 0xed7, DATA);
                    m.write(DATA, &left.to_le_bytes()).unwrap();
                }
                compare(&mut jit, &mut reference);
                assert_eq!(
                    jit.memory().read(DATA, 4).unwrap(),
                    reference.memory().read(DATA, 4).unwrap()
                );
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0);
    }
    let bytes = assemble(|a| {
        a.adc(dword_ptr(edi), ecx).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, Some(CompileStrategy::Fast));
    reset(&mut jit, 1, u32::MAX, 0xed7, DATA);
    jit.protect(DATA, 4096, READ).unwrap();
    let before = jit.state.clone();
    assert_eq!(
        jit.run(STOP, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: CODE,
            address: DATA
        }
    );
    assert_eq!(jit.state.regs, before.regs);
    assert_eq!(jit.state.eflags, before.eflags);
}

#[test]
fn all_setcc_conditions_preserve_flags_and_high_register_bits_for_raw_and_lazy_flags() {
    for producer in 0..5 {
        let bytes = assemble(|a| {
            match producer {
                0 => {}
                1 => a.cmp(eax, ecx).unwrap(),
                2 => a.add(eax, ecx).unwrap(),
                3 => a.test(al, cl).unwrap(),
                _ => a.cmp(ax, cx).unwrap(),
            }
            for condition in 0..16 {
                for (index, register) in [al, cl, dl, bl, ah, ch, dh, bh].into_iter().enumerate() {
                    setcc!(a, condition, register);
                    a.mov(
                        byte_ptr(DATA as i32 + condition * 8 + index as i32),
                        register,
                    )
                    .unwrap();
                }
                setcc!(a, condition, byte_ptr(DATA as i32 + 128 + condition));
            }
            a.ret().unwrap();
        });
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            let cases: Vec<_> = if producer == 0 {
                (0..64)
                    .map(|bits| {
                        let mut flags = 0x602;
                        for (index, bit) in [0, 2, 4, 6, 7, 11].into_iter().enumerate() {
                            flags |= ((bits >> index) & 1) << bit;
                        }
                        (0xa5a5_a5a5, 0x5a5a_5a5a, flags)
                    })
                    .collect()
            } else {
                VALUES
                    .into_iter()
                    .flat_map(|left| VALUES.into_iter().map(move |right| (left, right, 0xed7)))
                    .collect()
            };
            for (left, right, flags) in cases {
                for m in [&mut jit, &mut reference] {
                    reset(m, left, right, flags, DATA + 64);
                    m.write(DATA, &[0xa5; 160]).unwrap();
                }
                compare(&mut jit, &mut reference);
                assert_eq!(
                    jit.memory().read(DATA, 160).unwrap(),
                    reference.memory().read(DATA, 160).unwrap(),
                    "{strategy:?}, producer {producer}, {left:#x}, {right:#x}, {flags:#x}"
                );
            }
            assert_eq!(jit.stats.interpreter_entries, 0);
        }
    }
}

#[test]
fn sbb_and_setcc_memory_faults_leave_registers_flags_and_bytes_before_the_instruction() {
    for form in 0..3 {
        let bytes = assemble(|a| {
            match form {
                0 => a.sbb(eax, dword_ptr(edi)),
                1 => a.sbb(dword_ptr(edi), ecx),
                _ => a.setl(byte_ptr(edi)),
            }
            .unwrap();
            a.ret().unwrap();
        });
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            for m in [&mut jit, &mut reference] {
                reset(m, 0x7fff_ffff, 0xffff_ffff, 0xed7, DATA);
                m.write(DATA, &0x8000_0000u32.to_le_bytes()).unwrap();
                m.protect(DATA, 4096, if form == 0 { 0 } else { READ })
                    .unwrap();
            }
            let before = jit.state.clone();
            let expected = if form == 0 {
                RunOutcome::ReadFault {
                    pc: CODE,
                    address: DATA,
                }
            } else {
                RunOutcome::WriteFault {
                    pc: CODE,
                    address: DATA,
                }
            };
            assert_eq!(jit.run(STOP, 100).unwrap(), expected);
            assert_eq!(reference.run_unicorn(STOP, 100).unwrap(), expected);
            assert_eq!(jit.state.regs, before.regs);
            assert_eq!(jit.state.eflags, before.eflags);
            assert_eq!(jit.state.regs, reference.state.regs);
            assert_eq!(jit.state.eflags, reference.state.eflags);
            jit.protect(DATA, 4096, READ | WRITE).unwrap();
            assert_eq!(
                jit.memory().read(DATA, 4).unwrap(),
                0x8000_0000u32.to_le_bytes()
            );
        }
    }
}

#[test]
fn setcc_can_write_a_page_without_read_permission_and_short_sbb_stays_native() {
    let bytes = assemble(|a| {
        a.sete(byte_ptr(edi)).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, Some(CompileStrategy::Speed));
    reset(&mut jit, 0x1234_5678, 0, 0xed7, DATA);
    jit.protect(DATA, 4096, WRITE).unwrap();
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(jit.state.eflags, 0xed7);
    jit.protect(DATA, 4096, READ | WRITE).unwrap();
    assert_eq!(jit.memory().read(DATA, 1).unwrap(), [1]);
    assert_eq!(jit.stats.interpreter_entries, 0);

    let bytes = assemble(|a| {
        a.sbb(al, cl).unwrap();
        a.sbb(ax, cx).unwrap();
        a.lock().sbb(dword_ptr(edi), ecx).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&bytes, Some(CompileStrategy::Speed));
    let mut reference = machine(&bytes, None);
    for m in [&mut jit, &mut reference] {
        reset(m, 0x1234_7fff, 0x8765_ffff, 0xed7, DATA);
        m.write(DATA, &0x8000_0000u32.to_le_bytes()).unwrap();
    }
    compare(&mut jit, &mut reference);
    assert_eq!(
        jit.memory().read(DATA, 4).unwrap(),
        reference.memory().read(DATA, 4).unwrap()
    );
    assert_eq!(jit.stats.interpreter_entries, 0);
}
