use super::*;

// spec: A64-ISA-5, A64-FLAG-4, A64-MEM-5, A64-MEM-7, A64-VER-1
#[test]
fn cmpxchg32_preserves_aliases_flags_and_faults_with_checked_memory() {
    for form in 0..6 {
        for value in [0u32, 0x7fffffff, 0x80000000, 0xffffffff] {
            for equal in [false, true] {
                let mut p = fixture(|a| {
                    a.add(edi, ebx).unwrap();
                    match form {
                        0 => a.cmpxchg(edx, ecx),
                        1 => a.cmpxchg(eax, ecx),
                        2 => a.cmpxchg(edx, eax),
                        3 => a.cmpxchg(edx, edx),
                        4 => a.cmpxchg(dword_ptr(esi), ecx),
                        _ => a.lock().cmpxchg(dword_ptr(esi), ecx),
                    }
                    .unwrap();
                    a.adc(edi, 0).unwrap();
                });
                p.state.regs[0] = if equal { value } else { value ^ 0x80000000 };
                p.state.regs[2] = value;
                p.data[..4].copy_from_slice(&value.to_le_bytes());
                for config in configs() {
                    for budget in [1, 2, 100] {
                        check(&p, config, budget, form < 4 || config.2 != Checks::Slow);
                    }
                    if form >= 4 {
                        for offset in [0, 1, 4094] {
                            let mut p = Program {
                                code: p.code.clone(),
                                stop: p.stop,
                                data: p.data.clone(),
                                state: p.state.clone(),
                            };
                            p.state.regs[6] = DATA + offset;
                            p.data[offset as usize..offset as usize + 4]
                                .copy_from_slice(&value.to_le_bytes());
                            let mut native = executor(&p, config, false);
                            let mut oracle = executor(&p, config, true);
                            let page = (DATA + offset + 3) & !4095;
                            native.protect(page, 4096, adv_cpu_core::READ).unwrap();
                            oracle.protect(page, 4096, adv_cpu_core::READ).unwrap();
                            assert_eq!(
                                native.run(p.stop, 100).unwrap(),
                                oracle.run(p.stop, 100).unwrap()
                            );
                            same(&native, &oracle);
                        }
                    }
                }
            }
        }
    }
}

// spec: A64F-FP-1, A64F-FP-2, A64F-FP-3, A64-MEM-7, A64-VER-1
#[test]
fn fild64_loads_all_integer_bits_and_preserves_fault_and_exception_order() {
    for value in [
        0i64,
        1,
        -1,
        (1 << 53) - 1,
        (1 << 53) + 1,
        -(1 << 53) - 1,
        i64::MIN,
        i64::MAX,
    ] {
        let mut p = fixture(|a| {
            a.add(eax, ecx).unwrap();
            a.fild(qword_ptr(ebx)).unwrap();
        });
        p.data[..8].copy_from_slice(&value.to_le_bytes());
        for control in [0x007f, 0x027f, 0x037f, 0x077f, 0x0b7f, 0x0f7f] {
            p.state.x87.control = control;
            for config in configs() {
                for budget in [1, 2, 100] {
                    check(&p, config, budget, config.2 != Checks::Slow);
                }
            }
        }
    }
    for scenario in 0..4 {
        let mut p = fixture(|a| {
            a.nop().unwrap();
            a.fild(qword_ptr(ebx)).unwrap();
        });
        match scenario {
            0 => p.state.x87.tag = 0,
            1 => {
                p.state.x87.status = 0x8081;
                p.state.x87.control &= !1;
            }
            2 => p.state.mmx_mode = 1,
            _ => p.state.regs[3] = DATA + DATA_LEN as u32 - 4,
        }
        for config in configs() {
            check(&p, config, 100, false);
        }
    }
}

// spec: A64-REG-3, A64-FLAG-4, A64-VER-1
#[test]
fn cbw_keeps_the_upper_accumulator_and_arithmetic_flags() {
    for value in [0u32, 0x7f, 0x80, 0xff] {
        let mut p = fixture(|a| {
            a.add(edi, ebx).unwrap();
            a.cbw().unwrap();
            a.adc(edi, 0).unwrap();
        });
        p.state.regs[0] = 0xabcd1200 | value;
        for config in configs() {
            for budget in [1, 2, 3, 100] {
                check(&p, config, budget, true);
            }
        }
    }
}

// spec: A64-MEM-2, A64-MEM-4, A64-MEM-7, A64-MEM-8, A64F-FP-4, A64-VER-1
#[test]
fn checked_simd_moves_use_ordinary_aliases_and_follow_permission_changes() {
    let p = fixture(|a| {
        a.add(eax, edx).unwrap();
        a.movq(xmm0, qword_ptr(ebx)).unwrap();
        a.movdqa(xmmword_ptr(ebx + 16), xmm0).unwrap();
        a.movdqu(xmm1, xmmword_ptr(ebx + 32)).unwrap();
        a.movss(dword_ptr(ebx + 48), xmm1).unwrap();
        a.movq(mm0, qword_ptr(ebx + 64)).unwrap();
        a.movq(qword_ptr(ebx + 72), mm0).unwrap();
    });
    for flagm in [false, true] {
        for checks in [Checks::All, Checks::Slow] {
            let config = (Features { flagm }, false, checks);
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            for cpu in [&mut native, &mut oracle] {
                cpu.protect(DATA + 4096, DATA_LEN - 4096, 0).unwrap();
            }
            for permissions in [3, 1, 0, 3] {
                for cpu in [&mut native, &mut oracle] {
                    cpu.protect(DATA, 4096, permissions).unwrap();
                    cpu.state = p.state.clone();
                }
                let before = native.native.as_ref().unwrap().diagnostics.helpers;
                let interpreted = native.stats.interpreter_entries;
                assert_eq!(
                    native.run(p.stop, 100).unwrap(),
                    oracle.run(p.stop, 100).unwrap()
                );
                same(&native, &oracle);
                if permissions == 3 && checks != Checks::Slow {
                    assert_eq!(native.native.as_ref().unwrap().diagnostics.helpers, before);
                    assert_eq!(native.stats.interpreter_entries, interpreted);
                }
            }
        }
    }
}

fn configs() -> impl Iterator<Item = Config> {
    [false, true].into_iter().flat_map(|flagm| {
        [false, true].into_iter().flat_map(move |first| {
            [Checks::Adaptive, Checks::All, Checks::Slow]
                .into_iter()
                .map(move |checks| (Features { flagm }, first, checks))
        })
    })
}

fn check(program: &Program, config: Config, budget: u64, native_only: bool) {
    let mut native = executor(program, config, false);
    let mut oracle = executor(program, config, true);
    let actual = native.run(program.stop, budget).unwrap();
    assert_eq!(actual, oracle.run(program.stop, budget).unwrap());
    same(&native, &oracle);
    if native_only && !config.1 && actual == RunOutcome::ReachedStop {
        assert!(native.stats.native_entries > 0);
        assert_eq!(native.stats.interpreter_entries, 0);
        assert_eq!(native.native.as_ref().unwrap().diagnostics.helpers, 0);
    }
    if actual == RunOutcome::Budget {
        assert_eq!(
            native.run(program.stop, 1000).unwrap(),
            oracle.run(program.stop, 1000).unwrap()
        );
        same(&native, &oracle);
    }
}

// spec: A64-FLAG-2, A64-FLAG-3, A64-FLAG-4, A64-VER-1
#[test]
fn narrow_steps_keep_carry_and_match_at_budget_cuts() {
    for value in [
        0, 1, 0xf, 0x10, 0x7f, 0x80, 0xff, 0x100, 0x7fff, 0x8000, 0xffff,
    ] {
        let mut program = fixture(|a| {
            a.nop().unwrap();
            a.inc(al).unwrap();
            a.dec(ah).unwrap();
            a.inc(bp).unwrap();
            a.dec(sp).unwrap();
            a.lock().inc(byte_ptr(ebx)).unwrap();
            a.lock().dec(word_ptr(ebx + 2)).unwrap();
            // Consume a carry in both add and subtract senses around steps.
            a.adc(edx, 0).unwrap();
            a.add(ecx, -1).unwrap();
            a.dec(byte_ptr(ebx + 4)).unwrap();
            a.inc(cx).unwrap();
            a.adc(edx, 0).unwrap();
        });
        program.state.regs[0] = 0xa5a50000 | value | ((value & 0xff) << 8);
        program.state.regs[1] = value;
        program.state.regs[4] = STACK_TOP | value;
        program.state.regs[5] = DATA | value;
        program.data[..2].copy_from_slice(&(value as u16).to_le_bytes());
        program.data[2..4].copy_from_slice(&(value as u16).to_le_bytes());
        program.data[4] = value as u8;
        for cf in [0, 1] {
            program.state.eflags = (program.state.eflags & !1) | cf;
            for config in configs() {
                for budget in [1, 3, 6, 9, 100] {
                    check(&program, config, budget, true);
                }
            }
        }
    }
}

// spec: A64-FLAG-3, A64-FLAG-4, A64-MEM-5, A64-MEM-7, A64-VER-1
#[test]
fn narrow_steps_cross_pages_fault_without_commits_and_retry() {
    for (address, word, protected) in [
        (DATA + 0xfff, true, DATA + 0x1000),
        (DATA + DATA_LEN as u32 - 1, true, DATA + DATA_LEN as u32),
        (DATA, false, DATA),
    ] {
        for increment in [false, true] {
            for locked in [false, true] {
                let program = fixture(|a| {
                    a.add(eax, ecx).unwrap();
                    if locked {
                        let _ = a.lock();
                    }
                    match (increment, word) {
                        (true, true) => a.inc(word_ptr(address as i32)),
                        (false, true) => a.dec(word_ptr(address as i32)),
                        (true, false) => a.inc(byte_ptr(address as i32)),
                        (false, false) => a.dec(byte_ptr(address as i32)),
                    }
                    .unwrap();
                    a.adc(edx, 0).unwrap();
                });
                for config in configs() {
                    for permission in [0, 1, 3] {
                        let mut native = executor(&program, config, false);
                        let mut oracle = executor(&program, config, true);
                        for executor in [&mut native, &mut oracle] {
                            executor.protect(protected, 4096, 3).unwrap();
                            executor.write(address, &[0xff, 0x7f]).unwrap();
                            executor.protect(protected, 4096, permission).unwrap();
                        }
                        let outcome = native.run(program.stop, 100).unwrap();
                        assert_eq!(outcome, oracle.run(program.stop, 100).unwrap());
                        for executor in [&mut native, &mut oracle] {
                            executor.protect(protected, 4096, 3).unwrap();
                        }
                        same(&native, &oracle);
                        if permission != 3 {
                            assert_eq!(
                                native.run(program.stop, 100).unwrap(),
                                oracle.run(program.stop, 100).unwrap()
                            );
                            same(&native, &oracle);
                        }
                    }
                }
            }
        }
    }
}

// spec: A64F-FP-4, A64-REG-5, A64-VER-1
#[test]
fn byte_sign_masks_cover_both_register_classes_and_biased_destinations() {
    for mask in [0, 0xaaaa, 0x5555, 0xffff]
        .into_iter()
        .chain((0..16).map(|bit| 1 << bit))
    {
        let mut program = fixture(|a| {
            a.add(eax, ecx).unwrap();
            a.pmovmskb(ebp, mm0).unwrap();
            a.pmovmskb(esp, xmm1).unwrap();
            a.pmovmskb(edx, mm7).unwrap();
            a.pmovmskb(eax, xmm7).unwrap();
            a.emms().unwrap();
        });
        for (lane, byte) in program.state.xmm[1].iter_mut().enumerate() {
            *byte = if mask & (1 << lane) != 0 { 0xff } else { 0x7f };
        }
        program.state.xmm[7] = program.state.xmm[1];
        program.state.mmx[0] = u64::from_le_bytes(program.state.xmm[1][..8].try_into().unwrap());
        program.state.mmx[7] = program.state.mmx[0];
        program.state.x87.status = 5 << 11;
        for config in configs() {
            for budget in [1, 2, 3, 4, 100] {
                check(&program, config, budget, true);
            }
        }
    }
}

// spec: A64F-FP-5, A64-REG-5, A64-VER-1
#[test]
fn packed_doubles_check_both_lanes_exceptions_and_mxcsr() {
    for form in 0..3 {
        for value in [
            0.0f64.to_bits(),
            (-0.0f64).to_bits(),
            2.5f64.to_bits(),
            1,
            f64::MAX.to_bits(),
            f64::INFINITY.to_bits(),
            0x7ff8000000000011,
            0x7ff0000000000001,
        ] {
            for upper in [false, true] {
                let mut program = fixture(|a| {
                    a.add(eax, ecx).unwrap();
                    match form {
                        0 => a.addpd(xmm0, xmm1),
                        1 => a.mulpd(xmm0, xmm1),
                        _ => a.subpd(xmm0, xmm1),
                    }
                    .unwrap();
                    a.movupd(xmmword_ptr(ebx), xmm0).unwrap();
                    a.movapd(xmm2, xmm0).unwrap();
                });
                for lane in program.state.xmm[0].chunks_mut(8) {
                    lane.copy_from_slice(&1.25f64.to_bits().to_le_bytes());
                }
                program.state.xmm[1] = program.state.xmm[0];
                let at = if upper { 8 } else { 0 };
                program.state.xmm[1][at..at + 8].copy_from_slice(&value.to_le_bytes());
                for control in [0x1f80, 0x1fa0, 0x3f80, 0x1f00, 0x1e80] {
                    program.state.mxcsr = control;
                    for config in configs() {
                        let budgets: &[u64] = if value == 2.5f64.to_bits() && control == 0x1f80 {
                            &[1, 2, 3, 100]
                        } else {
                            &[100]
                        };
                        for &budget in budgets {
                            check(
                                &program,
                                config,
                                budget,
                                value == 2.5f64.to_bits()
                                    && control == 0x1f80
                                    && matches!(config.2, Checks::Adaptive),
                            );
                        }
                    }
                }
            }
        }
    }
}

// spec: A64F-FP-1, A64F-FP-2, A64F-FP-3, A64-VER-1
#[test]
fn x87_round_scale_and_examine_match_all_rounding_modes_and_specials() {
    use adv_cpu_core::X87Register as Ext;
    let values = [
        Ext::from_parts(0, 0),
        Ext::from_parts(0, 0x8000),
        Ext::from_parts(0xa000000000000000, 0x4000), // 2.5
        Ext::from_parts(0xe000000000000000, 0xc000), // -3.5
        Ext::from_parts(0xc000000000000000, 0x3ffd), // 0.375
        Ext::from_parts(0xc000000000000000, 0xbffd),
        Ext::from_parts(0x8000000000000000, 0x7fff),
        Ext::from_parts(0x8000000000000000, 0xffff),
        Ext::from_parts(0xc000000000000011, 0x7fff),
        Ext::from_parts(0x8000000000000001, 0x7fff),
        Ext::from_parts(1, 0),
        Ext::from_parts(0x8000000000000000, 0),
        Ext::from_parts(0x1234567890, 0x4000), // Unsupported encoding.
        Ext::from_parts(0x8000000000000001, 0x3fff), // Beyond binary64 precision.
        Ext::from_parts(0x8000000000000000, 1),
        Ext::from_parts(0x8000000000000000, 0x7ffe),
    ];
    for form in 0..3 {
        for (n, &value) in values.iter().enumerate() {
            let mut program = fixture(|a| {
                a.add(eax, ecx).unwrap();
                match form {
                    0 => a.frndint(),
                    1 => a.fscale(),
                    _ => a.fxam(),
                }
                .unwrap();
                a.fnstsw(ax).unwrap();
            });
            program.state.x87.set_top(5);
            program.state.x87.regs[5] = value;
            program.state.x87.set_physical_tag(5, value.tag());
            program.state.x87.regs[6] = values[3]; // trunc(-3.5) = -3
            program.state.x87.set_physical_tag(6, 0);
            program.state.x87.status |= 0x4700;
            program.state.x87.ip = 0x1234;
            program.state.x87.opcode = 0x345;
            program.state.refresh_mmx();
            for rc in 0..4 {
                for pc in [0, 0x200, 0x300] {
                    program.state.x87.control = 0x7f | pc | (rc << 10);
                    for config in configs() {
                        let budgets: &[u64] = if n == 2 { &[1, 2, 3, 100] } else { &[100] };
                        for &budget in budgets {
                            check(&program, config, budget, n == 2);
                        }
                    }
                }
            }
        }
    }
    // Empty slots, pending exceptions, MMX mode, unmasked precision and
    // extreme or special scale counts must preserve the helper's result.
    for form in 0..3 {
        for scenario in 0..12 {
            let mut program = fixture(|a| {
                a.nop().unwrap();
                match form {
                    0 => a.frndint(),
                    1 => a.fscale(),
                    _ => a.fxam(),
                }
                .unwrap();
            });
            program.state.x87.regs[0] = values[2];
            program.state.x87.set_physical_tag(0, 0);
            program.state.x87.regs[1] = values[2];
            program.state.x87.set_physical_tag(1, 0);
            match scenario {
                0 => program.state.x87.set_physical_tag(0, 3),
                1 => program.state.x87.set_physical_tag(1, 3),
                2 => {
                    program.state.x87.status = 0x80a0;
                    program.state.x87.control &= !0x20;
                }
                3 => program.state.mmx_mode = 1,
                4 => program.state.x87.control &= !0x20,
                11 => program.state.x87.regs[0] = values[15],
                _ => program.state.x87.regs[1] = values[[6, 7, 8, 10, 14, 15][scenario - 5]],
            }
            program.state.refresh_mmx();
            if scenario == 3 {
                program.state.mmx_mode = 1;
            }
            for config in configs() {
                for budget in [1, 2, 100] {
                    check(&program, config, budget, false);
                }
            }
        }
    }
}

// spec: A64F-FP-4, A64-VER-1
#[test]
fn mmx_byte_masks_deliver_pending_x87_exceptions_before_state_changes() {
    let mut program = fixture(|a| {
        a.nop().unwrap();
        a.pmovmskb(esp, mm0).unwrap();
    });
    program.state.x87.status = 0x81;
    program.state.x87.control &= !1;
    for config in configs() {
        for budget in [1, 2, 100] {
            check(&program, config, budget, false);
        }
    }
}
