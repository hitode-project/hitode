//! Exercise instruction commits, faults, retries and fuel through the native owner.
use super::*;

fn fixture(build: impl FnOnce(&mut CodeAssembler)) -> Program {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.nop().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    Program {
        stop: CODE + code.len() as u32 - 1,
        code,
        data: vec![0; DATA_LEN],
        state: CpuState {
            eip: CODE,
            eflags: 0xed7,
            regs: [0x81234567, 2, 0x87654321, DATA, STACK_TOP, DATA, DATA, 0],
            ..Default::default()
        },
    }
}

fn executor(program: &Program, config: Config, interpreted: bool) -> Executor {
    let mut executor = Executor::new(BASE, LEN).unwrap();
    let native = executor.native.as_mut().unwrap();
    native.features = config.0;
    native.first_visits = config.1;
    native.checks = config.2;
    load(executor.memory_mut(), program);
    executor.state = program.state.clone();
    executor.interpreter_only = interpreted;
    executor
}

fn same(actual: &Executor, expected: &Executor) {
    let (a, b) = (&actual.state, &expected.state);
    assert_eq!(a.regs, b.regs, "registers");
    assert_eq!(a.eip, b.eip, "eip");
    assert_eq!(a.eflags, b.eflags, "eflags");
    assert_eq!(a.xmm, b.xmm, "xmm");
    assert_eq!(a.mxcsr, b.mxcsr, "mxcsr");
    assert_eq!(a.mmx, b.mmx, "mmx");
    assert_eq!(a.x87, b.x87, "x87");
    assert_eq!(a.fuel, b.fuel, "fuel");
    assert_eq!(a.fault_addr, b.fault_addr, "fault address");
    assert_eq!(
        snapshot(actual.memory()),
        snapshot(expected.memory()),
        "memory"
    );
}

fn compare_instruction(
    scalar: bool,
    double: bool,
    a: &mut CodeAssembler,
    source: Option<u32>,
    predicate: i32,
) {
    macro_rules! compare {
        ($op:ident, $memory:ident) => {
            if let Some(address) = source {
                a.$op(xmm0, $memory(address as i32), predicate)
            } else {
                a.$op(xmm0, xmm1, predicate)
            }
            .unwrap()
        };
    }
    match (scalar, double) {
        (false, false) => compare!(cmpps, xmmword_ptr),
        (true, false) => compare!(cmpss, dword_ptr),
        (false, true) => compare!(cmppd, xmmword_ptr),
        (true, true) => compare!(cmpsd_3, qword_ptr),
    }
}

#[test]
fn bit_scans_biased_cl_shifts_and_comparisons_run_without_helpers_and_resume_at_every_budget() {
    for flagm in [false, true] {
        for count in [0, 1, 2, 32, 33, 34] {
            let mut program = fixture(|a| {
                a.add(eax, edx).unwrap();
                a.bsf(edx, dword_ptr(ebx)).unwrap();
                a.bsr(bp, word_ptr(ebx + 4)).unwrap();
                a.bsf(eax, eax).unwrap();
                a.bsr(cx, cx).unwrap();
                a.mov(ecx, count).unwrap();
                a.mov(esp, DATA << (count & 31)).unwrap();
                a.shr(esp, cl).unwrap();
                // These accesses require the high window bias to survive the shift.
                a.mov(edi, dword_ptr(esp)).unwrap();
                a.mov(ebp, DATA << (count & 31)).unwrap();
                a.shr(ebp, cl).unwrap();
                a.mov(esi, dword_ptr(ebp)).unwrap();
                for scalar in [false, true] {
                    for double in [false, true] {
                        compare_instruction(scalar, double, a, None, 0);
                    }
                }
            });
            program.data[..4].copy_from_slice(&0x80000001u32.to_le_bytes());
            program.data[4..6].copy_from_slice(&0x8000u16.to_le_bytes());
            let instructions =
                iced_x86::Decoder::new(32, &program.code, 0).iter().count() as u64 - 1;
            let config = (Features { flagm }, false, Checks::Adaptive);
            for budget in 1..=instructions + 1 {
                let mut native = executor(&program, config, false);
                let mut oracle = executor(&program, config, true);
                let outcome = native.run(program.stop, budget).unwrap();
                assert_eq!(outcome, oracle.run(program.stop, budget).unwrap());
                same(&native, &oracle);
                // A short budget may use the owner's one-instruction path, but
                // a complete unit must perform every fixture instruction natively.
                if budget > instructions {
                    assert!(native.stats.native_entries > 0);
                    assert_eq!(native.stats.interpreter_entries, 0);
                    assert_eq!(native.native.as_ref().unwrap().diagnostics.helpers, 0);
                    assert_eq!(native.state.fuel, budget - instructions);
                } else {
                    assert_eq!(
                        native.run(program.stop, 100).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    assert_eq!(
                        oracle.run(program.stop, 100).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    same(&native, &oracle);
                }
            }
        }
    }
}

// spec: A64-FLAG-4
#[test]
fn bit_scan_and_scalar_compare_cross_page_faults_commit_only_the_prefix_and_retry_after_mapping() {
    for kind in 0..9 {
        let width = match kind {
            0 | 1 => 2,
            2 | 3 | 4 | 6 | 7 => 4,
            _ => 8,
        };
        let address = DATA + DATA_LEN as u32 - width / 2;
        let mut program = fixture(|a| {
            a.add(eax, ecx).unwrap();
            match kind {
                0 => a.bsf(bp, word_ptr(address as i32)).unwrap(),
                1 => a.bsr(sp, word_ptr(address as i32)).unwrap(),
                2 => a.bsf(ebp, dword_ptr(address as i32)).unwrap(),
                3 => a.bsr(esp, dword_ptr(address as i32)).unwrap(),
                4 => compare_instruction(true, false, a, Some(address), 0),
                5 => compare_instruction(true, true, a, Some(address), 0),
                6 => a.leave().unwrap(),
                7 => a.bt(dword_ptr(address as i32), 31).unwrap(),
                _ => a.movsd_2(xmm0, qword_ptr(address as i32)).unwrap(),
            }
            a.setz(al).unwrap();
        });
        program.state.regs[5] = address;
        for flagm in [false, true] {
            for checks in [Checks::Adaptive, Checks::All, Checks::Slow] {
                let config = (Features { flagm }, false, checks);
                let mut native = executor(&program, config, false);
                let mut oracle = executor(&program, config, true);
                let expected = oracle.run(program.stop, 100).unwrap();
                assert!(
                    matches!(expected, RunOutcome::ReadFault { address: at, .. } if at == DATA + DATA_LEN as u32)
                );
                assert_eq!(native.run(program.stop, 100).unwrap(), expected);
                same(&native, &oracle);
                assert_eq!(native.state.fuel, 99);
                assert_eq!(native.state.regs[4], program.state.regs[4]);
                assert_eq!(native.state.regs[5], program.state.regs[5]);
                assert_eq!(native.state.xmm, program.state.xmm);
                // A mapping change invalidates the failed unit. Retrying the
                // current instruction must neither repeat nor lose the prefix.
                for executor in [&mut native, &mut oracle] {
                    executor.protect(DATA + DATA_LEN as u32, 4096, 3).unwrap();
                    let value = 0x80000001u64.to_le_bytes();
                    executor.write(address, &value[..width as usize]).unwrap();
                }
                assert_eq!(
                    native.run(program.stop, 100).unwrap(),
                    RunOutcome::ReachedStop
                );
                assert_eq!(
                    oracle.run(program.stop, 100).unwrap(),
                    RunOutcome::ReachedStop
                );
                same(&native, &oracle);
                assert_eq!(native.state.fuel, 98);
            }
        }
    }
}

// spec: A64-FP-5 A64-FLAG-6
#[test]
fn sse_conversions_double_arithmetic_and_flag_compares_match_the_owner() {
    for form in 0..13 {
        for scenario in 0..14 {
            for flagm in [false, true] {
                let source = match scenario {
                    10 => Some(DATA + 1),
                    11 => Some(HOLE - 2),
                    _ => None,
                };
                let mut program = fixture(|a| {
                    a.add(eax, ecx).unwrap();
                    macro_rules! op {
                        ($name:ident, $ptr:ident) => {
                            if let Some(address) = source {
                                a.$name(xmm0, $ptr(address as i32))
                            } else {
                                a.$name(xmm0, if scenario == 12 { xmm0 } else { xmm1 })
                            }
                            .unwrap()
                        };
                    }
                    match form {
                        0 => op!(cvtdq2ps, xmmword_ptr),
                        1 => op!(cvtdq2pd, qword_ptr),
                        2 => op!(cvtps2pd, qword_ptr),
                        3 => op!(cvtss2sd, dword_ptr),
                        4 => op!(cvtsd2ss, qword_ptr),
                        5 => op!(addsd, qword_ptr),
                        6 => op!(subsd, qword_ptr),
                        7 => op!(mulsd, qword_ptr),
                        8 => op!(divsd, qword_ptr),
                        9 => op!(comiss, dword_ptr),
                        10 => op!(ucomiss, dword_ptr),
                        11 => op!(comisd, qword_ptr),
                        _ => op!(ucomisd, qword_ptr),
                    }
                    a.setz(al).unwrap();
                    a.setb(cl).unwrap();
                });
                let double = matches!(form, 4..=8 | 11..=12);
                let value = if form < 2 {
                    match scenario {
                        0 => 7,
                        1 => i32::MIN as u64,
                        2 => i32::MAX as u64,
                        _ => 16_777_217,
                    }
                } else if double {
                    match scenario {
                        0 | 10..=13 => 2.5f64.to_bits(),
                        1 => (-0.0f64).to_bits(),
                        2 => 0x7ff8000000000001,
                        3 | 7 => 0x7ff0000000000001,
                        4 | 8 => 1,
                        5 => f64::MAX.to_bits(),
                        6 => f64::INFINITY.to_bits(),
                        _ => (1.0f64 / 3.0).to_bits(),
                    }
                } else {
                    match scenario {
                        0 | 10..=13 => 2.5f32.to_bits() as u64,
                        1 => (-0.0f32).to_bits() as u64,
                        2 => 0x7fc00001,
                        3 | 7 => 0x7f800001,
                        4 | 8 => 1,
                        5 => f32::MAX.to_bits() as u64,
                        6 => f32::INFINITY.to_bits() as u64,
                        _ => (1.0f32 / 3.0).to_bits() as u64,
                    }
                };
                let width = if double { 8 } else { 4 };
                program.state.xmm[0].fill(0xa5);
                program.state.xmm[0][..width].copy_from_slice(
                    &if double {
                        1.25f64.to_bits()
                    } else {
                        1.25f32.to_bits() as u64
                    }
                    .to_le_bytes()[..width],
                );
                for lane in program.state.xmm[1].chunks_mut(width) {
                    lane.copy_from_slice(&value.to_le_bytes()[..width]);
                }
                if scenario == 0 && form <= 3 {
                    if form < 2 {
                        program.state.xmm[1][4..8].copy_from_slice(&(-17i32).to_le_bytes());
                    }
                    program.state.xmm[1][8..12].copy_from_slice(&0x7f800001u32.to_le_bytes());
                    program.state.xmm[1][12..16].copy_from_slice(&1u32.to_le_bytes());
                }
                program.data[1..17].copy_from_slice(&program.state.xmm[1]);
                program.state.mxcsr = match scenario {
                    7 => 0x1f00,
                    8 => 0x1e80,
                    9 => 0x3f80,
                    13 => 0x1fa0,
                    _ => 0x1f80,
                };
                let config = (Features { flagm }, false, Checks::Adaptive);
                let budgets: &[u64] = if scenario == 0 {
                    &[1, 2, 3, 100]
                } else {
                    &[100]
                };
                for &budget in budgets {
                    let mut native = executor(&program, config, false);
                    let mut oracle = executor(&program, config, true);
                    let outcome = native.run(program.stop, budget).unwrap();
                    assert_eq!(
                        outcome,
                        oracle.run(program.stop, budget).unwrap(),
                        "form={form}, scenario={scenario}, budget={budget}"
                    );
                    same(&native, &oracle);
                    if scenario == 0 && budget == 100 {
                        assert_eq!(native.stats.interpreter_entries, 0, "form={form}");
                        assert_eq!(
                            native.native.as_ref().unwrap().diagnostics.helpers,
                            0,
                            "form={form}, helpers={:?}",
                            native.interpreted_forms(2)
                        );
                    }
                }
            }
        }
    }
}

// spec: A64-FP-5
#[test]
fn comparison_exceptions_alignment_and_scalar_upper_lanes_match_the_owner() {
    for scalar in [false, true] {
        for double in [false, true] {
            let width = if double { 8 } else { 4 };
            let snan = if double {
                0x7ff0000000000001u64
            } else {
                0x7f800001u64
            };
            let qnan = if double {
                0x7ff8000000000001u64
            } else {
                0x7fc00001u64
            };
            for scenario in 0..9 {
                for predicate in 0..8 {
                    let source = match scenario {
                        5 => Some(DATA + 1),
                        6 => Some(HOLE),
                        _ => None,
                    };
                    let mut program = fixture(|a| {
                        a.add(eax, ecx).unwrap();
                        compare_instruction(scalar, double, a, source, predicate | 0xf8);
                        a.setz(al).unwrap();
                    });
                    program.state.xmm[0].fill(0x55);
                    let value = match scenario {
                        0 | 7 => snan,
                        1 => qnan,
                        2 | 8 => 1,
                        _ => 0,
                    };
                    let lane = if scenario == 3 { width } else { 0 };
                    let value = if scenario == 3 { snan } else { value };
                    program.state.xmm[1][lane..lane + width]
                        .copy_from_slice(&value.to_le_bytes()[..width]);
                    program.state.mxcsr = match scenario {
                        7 => 0x1f00, // Invalid unmasked.
                        8 => 0x1e80, // Denormal unmasked.
                        _ => 0x1f80,
                    };
                    for flagm in [false, true] {
                        let config = (Features { flagm }, false, Checks::Adaptive);
                        let mut native = executor(&program, config, false);
                        let mut oracle = executor(&program, config, true);
                        let outcome = native.run(program.stop, 100).unwrap();
                        assert_eq!(
                            outcome,
                            oracle.run(program.stop, 100).unwrap(),
                            "scalar={scalar}, double={double}, scenario={scenario}, predicate={predicate}"
                        );
                        same(&native, &oracle);
                        if scenario == 3 && scalar {
                            assert_eq!(outcome, RunOutcome::ReachedStop);
                            assert_eq!(native.state.mxcsr, 0x1f80);
                            assert_eq!(native.stats.interpreter_entries, 0);
                            assert_eq!(native.native.as_ref().unwrap().diagnostics.helpers, 0);
                        }
                        if scenario == 5 && !scalar {
                            assert!(matches!(outcome, RunOutcome::Trap { vector: 13, .. }));
                            assert_eq!(native.state.xmm, program.state.xmm);
                            assert_eq!(native.state.fuel, 99);
                        }
                        if scenario == 6 {
                            assert!(matches!(
                                outcome,
                                RunOutcome::ReadFault { address: HOLE, .. }
                            ));
                            assert_eq!(native.state.xmm, program.state.xmm);
                            assert_eq!(native.state.fuel, 99);
                        }
                        if scenario == 7 || scenario == 8 {
                            assert!(matches!(outcome, RunOutcome::Trap { vector: 19, .. }));
                            assert_eq!(native.state.xmm, program.state.xmm);
                            assert_eq!(native.state.fuel, 99);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn tf_and_rf_keep_precise_boundaries_for_new_native_forms() {
    for form in 0..4 {
        for flag in [1 << 8, 1 << 16] {
            let mut program = fixture(|a| match form {
                0 => a.bsf(eax, edx).unwrap(),
                1 => a.shr(ebp, cl).unwrap(),
                2 => compare_instruction(false, false, a, None, 0),
                _ => compare_instruction(true, true, a, None, 0),
            });
            program.state.eflags |= flag;
            let config = (Features { flagm: false }, false, Checks::Adaptive);
            let mut native = executor(&program, config, false);
            let mut oracle = executor(&program, config, true);
            let outcome = native.run(program.stop, 100).unwrap();
            assert_eq!(outcome, oracle.run(program.stop, 100).unwrap());
            same(&native, &oracle);
            assert_eq!(native.stats.native_entries, 0);
            assert_eq!(native.state.fuel, 99);
            if flag == 1 << 8 {
                assert!(matches!(outcome, RunOutcome::Trap { vector: 1, .. }));
            } else {
                assert_eq!(outcome, RunOutcome::ReachedStop);
                assert_eq!(native.state.eflags & flag, 0);
            }
        }
    }
}

// spec: A64-CTL-10
#[test]
fn rewritten_bit_scan_and_comparison_forms_invalidate_the_old_native_unit() {
    let make = |reverse| {
        fixture(|a| {
            a.add(eax, ecx).unwrap();
            if reverse {
                a.bsr(ebp, edx)
            } else {
                a.bsf(ebp, edx)
            }
            .unwrap();
            a.cmpps(xmm0, xmm1, if reverse { 4 } else { 0 }).unwrap();
        })
    };
    let programs = [make(false), make(true)];
    assert_eq!(programs[0].stop, programs[1].stop);
    for flagm in [false, true] {
        let config = (Features { flagm }, false, Checks::Adaptive);
        let mut native = executor(&programs[0], config, false);
        native.protect(CODE, 0x1_0000, 7).unwrap();
        for program in programs.iter().cycle().take(4) {
            native.write(CODE, &program.code).unwrap();
            native.state = program.state.clone();
            let mut oracle = executor(program, config, true);
            assert_eq!(
                native.run(program.stop, 100).unwrap(),
                RunOutcome::ReachedStop
            );
            assert_eq!(
                oracle.run(program.stop, 100).unwrap(),
                RunOutcome::ReachedStop
            );
            same(&native, &oracle);
            assert_eq!(native.stats.interpreter_entries, 0);
            assert_eq!(native.native.as_ref().unwrap().diagnostics.helpers, 0);
        }
        assert!(native.stats.invalidations >= 3);
    }
}

// spec: A64-REG-5, A64-REG-6, A64-FLAG-2, A64-FLAG-4, A64-VER-1
#[test]
fn scalar_moves_word_extraction_bit_tests_and_leave_match_at_budget_cuts() {
    let mut program = fixture(|a| {
        a.add(eax, edx).unwrap();
        a.movsd_2(xmm0, xmm1).unwrap();
        a.movsd_2(qword_ptr(ebx), xmm0).unwrap();
        a.movsd_2(xmm2, qword_ptr(ebx)).unwrap();
        a.pextrw(ebp, xmm0, 15).unwrap();
        a.pextrw(ecx, xmm2, 3).unwrap();
        a.bt(dword_ptr(ebx), 63).unwrap();
        a.setb(al).unwrap();
        a.bt(ecx, 32).unwrap();
        a.setb(dl).unwrap();
        a.mov(ebp, ebx).unwrap();
        a.leave().unwrap();
        a.mov(dword_ptr(esp), eax).unwrap();
    });
    program.state.xmm[0] = [0x55; 16];
    program.state.xmm[1] = [0x81; 16];
    program.state.xmm[2] = [0xaa; 16];
    let instructions = iced_x86::Decoder::new(32, &program.code, 0).iter().count() as u64 - 1;
    for flagm in [false, true] {
        for first in [false, true] {
            for checks in [Checks::Adaptive, Checks::All, Checks::Slow] {
                for budget in 1..=instructions + 1 {
                    let config = (Features { flagm }, first, checks);
                    let mut native = executor(&program, config, false);
                    let mut oracle = executor(&program, config, true);
                    assert_eq!(
                        native.run(program.stop, budget).unwrap(),
                        oracle.run(program.stop, budget).unwrap()
                    );
                    same(&native, &oracle);
                    if !first && checks == Checks::Adaptive && budget > instructions {
                        assert_eq!(native.native.as_ref().unwrap().diagnostics.helpers, 0);
                    }
                }
            }
        }
    }
}

// spec: A64-MEM-1, A64-MEM-2, A64-MEM-4, A64-MEM-7, A64-MEM-8
#[test]
fn checked_ordinary_pages_avoid_helpers_and_follow_permission_changes() {
    let program = fixture(|a| {
        a.add(eax, edx).unwrap();
        a.mov(dword_ptr(ebx), eax).unwrap();
        a.movsx(ecx, byte_ptr(ebx)).unwrap();
        a.movzx(edx, word_ptr(ebx)).unwrap();
        a.mov(dword_ptr(ebx + 4), ecx).unwrap();
        a.mov(esi, dword_ptr(ebx + 4)).unwrap();
    });
    for flagm in [false, true] {
        let config = (Features { flagm }, false, Checks::All);
        let mut native = executor(&program, config, false);
        let mut oracle = executor(&program, config, true);
        // An inaccessible neighbour blocks the entire 16 KiB window page.
        for cpu in [&mut native, &mut oracle] {
            cpu.protect(DATA + 4096, DATA_LEN - 4096, 0).unwrap();
        }
        for permissions in [3, 1, 0, 3] {
            for cpu in [&mut native, &mut oracle] {
                cpu.protect(DATA, 4096, permissions).unwrap();
                cpu.state = program.state.clone();
            }
            assert_eq!(
                native.run(program.stop, 100).unwrap(),
                oracle.run(program.stop, 100).unwrap()
            );
            same(&native, &oracle);
            if permissions == 3 {
                assert_eq!(native.native.as_ref().unwrap().diagnostics.accesses, 0);
            }
            native.native.as_mut().unwrap().diagnostics.accesses = 0;
        }
    }
}
