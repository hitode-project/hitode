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
    let native_page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    for host in [4096, 16384]
        .into_iter()
        .filter(|host| host % native_page == 0)
    {
        let mut native = executor_on_host(program, config, false, host);
        let mut oracle = executor_on_host(program, config, true, host);
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

#[test]
#[ignore = "10M instruction microbenchmark"]
fn common_forms_microbenchmark() {
    use std::time::Instant;
    for form in 0..2 {
        let mut p = fixture(|a| {
            a.mov(ecx, 10_000_000u32).unwrap();
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            if form == 0 {
                a.push(eax).unwrap();
                a.pop(dword_ptr(ebx)).unwrap();
            } else {
                a.fld(tbyte_ptr(ebx)).unwrap();
                a.fstp(tbyte_ptr(ebx + 16)).unwrap();
            }
            a.dec(ecx).unwrap();
            a.jnz(top).unwrap();
        });
        p.data[..10]
            .copy_from_slice(&adv_cpu_core::X87Register::from_parts(0xa000000000000000, 0x4000).0);
        let mut cpu = executor(
            &p,
            (Features { flagm: false }, false, Checks::Adaptive),
            false,
        );
        // Compile before measuring.
        assert_eq!(cpu.run(p.stop, 100).unwrap(), RunOutcome::Budget);
        cpu.state = p.state.clone();
        let helpers = cpu.native.as_ref().unwrap().diagnostics.helpers;
        let now = Instant::now();
        assert_eq!(
            cpu.run(p.stop, 50_000_000).unwrap(),
            RunOutcome::ReachedStop
        );
        println!(
            "W11 form={form} ns/roundtrip={:.3} helpers={}",
            now.elapsed().as_nanos() as f64 / 10_000_000.0,
            cpu.native.as_ref().unwrap().diagnostics.helpers - helpers
        );
    }
}

// spec: A64-TR-4, A64-VER-1, A64-FLAG-4, A64-MEM-7
#[test]
fn common_stack_and_flags_match_at_cuts_and_faults() {
    for form in 0..5 {
        for value in [0u32, 0x202, 0x244cd5, 0xfffffeff] {
            let p = fixture(|a| {
                a.add(edx, ecx).unwrap();
                a.push(if form == 2 {
                    STACK_TOP as i32
                } else {
                    value as i32
                })
                .unwrap();
                match form {
                    0 => a.pop(dword_ptr(ebx)),
                    1 => a.pop(dword_ptr(esp + 4)),
                    2 => a.pop(esp),
                    3 => a.popfd(),
                    _ => a.pop(dword_ptr(ebx + 4094)),
                }
                .unwrap();
                if form != 2 {
                    a.pushfd().unwrap();
                    a.pop(eax).unwrap();
                }
                a.adc(edx, 0).unwrap();
            });
            for config in configs() {
                for budget in [1, 2, 3, 4, 100] {
                    check(&p, config, budget, form != 4 || config.2 != Checks::Slow);
                }
                if form != 2 {
                    for permission in [0, 1] {
                        let mut a = executor(&p, config, false);
                        let mut b = executor(&p, config, true);
                        for cpu in [&mut a, &mut b] {
                            cpu.protect(if form == 1 { STACK_TOP } else { DATA }, 4096, permission)
                                .unwrap();
                        }
                        assert_eq!(a.run(p.stop, 100).unwrap(), b.run(p.stop, 100).unwrap());
                        same(&a, &b);
                    }
                }
            }
        }
    }
}

// spec: A64F-FP-8, A64-VER-1, A64-MEM-7
#[test]
fn common_m80_roundtrips_cover_encodings_controls_and_faults() {
    use adv_cpu_core::X87Register as Ext;
    for value in [
        Ext::from_parts(0, 0),
        Ext::from_parts(0, 0x8000),
        Ext::from_parts(0xa000000000000000, 0x4000),
        Ext::from_parts(0x8000000000000001, 0x3fff),
        Ext::from_parts(0x8000000000000000, 1),
        Ext::from_parts(0x8000000000000000, 0x7fff),
        Ext::from_parts(0xc000000000000011, 0x7fff),
        Ext::from_parts(1, 0),
        Ext::from_parts(0x8000000000000000, 0),
    ] {
        for offset in [0u32, 4090] {
            let mut p = fixture(|a| {
                a.add(edx, ecx).unwrap();
                a.fld(tbyte_ptr(ebx + offset as i32)).unwrap();
                a.fstp(tbyte_ptr(ebx + 32 + offset as i32)).unwrap();
                a.adc(edx, 0).unwrap();
            });
            p.data[offset as usize..offset as usize + 10].copy_from_slice(&value.0);
            for control in [0x7f, 0x27f, 0x37f, 0x77f, 0xb7f, 0xf7f] {
                p.state.x87.control = control;
                for config in configs() {
                    for budget in [1, 2, 3, 100] {
                        check(
                            &p,
                            config,
                            budget,
                            offset == 0
                                && value.tag() != 2
                                && value.sign_exponent() & 0x7fff != 1
                                && config.2 != Checks::Slow,
                        );
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4, A64F-FP-2, A64-VER-1, A64-VER-2
#[test]
fn common_gs_x87_memory_forms_keep_native_runs() {
    let mut p = fixture(|a| {
        a.fld(dword_ptr(ebx).gs()).unwrap();
        a.fadd(qword_ptr(ebx + 8).gs()).unwrap();
        a.fstp(qword_ptr(ebx + 16).gs()).unwrap();
        a.fld(qword_ptr(ebx + 16).gs()).unwrap();
        a.fstp(dword_ptr(ebx + 24).gs()).unwrap();
    });
    p.data[..4].copy_from_slice(&1.5f32.to_bits().to_le_bytes());
    p.data[8..16].copy_from_slice(&2.25f64.to_bits().to_le_bytes());
    p.state.fs_base = DATA;
    for config in configs() {
        for budget in [1, 2, 3, 4, 5, 100] {
            check(&p, config, budget, config.2 != Checks::Slow);
        }
    }
}

// spec: A64F-FP-8, A64-VER-1
#[test]
fn common_x87_leaves_match_interpreter_in_runs() {
    use adv_cpu_core::X87Register as Ext;
    for form in 0..11 {
        for value in [
            Ext::from_parts(0xc000000000000000, 0x3ffd),
            Ext::from_parts(0, 0x8000),
            Ext::from_parts(0x8000000000000001, 0x3fff),
            Ext::from_parts(0x8000000000000000, 0x7fff),
            Ext::from_parts(0xc000000000000011, 0x7fff),
            Ext::from_parts(1, 0),
        ] {
            let mut p = fixture(|a| {
                a.fld(st0).unwrap();
                a.movq(qword_ptr(ebx + 64), xmm3).unwrap();
                match form {
                    0 => a.fsin(),
                    1 => a.fcos(),
                    2 => a.fsincos(),
                    3 => a.fptan(),
                    4 => a.fpatan(),
                    5 => a.f2xm1(),
                    6 => a.fyl2x(),
                    7 => a.fyl2xp1(),
                    8 => a.fscale(),
                    9 => a.fprem(),
                    _ => a.fprem1(),
                }
                .unwrap();
                a.movq(qword_ptr(ebx + 72), xmm3).unwrap();
                a.fchs().unwrap();
                a.fstp(tbyte_ptr(ebx)).unwrap();
            });
            p.state
                .x87
                .push(Ext::from_parts(0xa000000000000000, 0x4000));
            p.state.x87.push(value);
            p.state.refresh_mmx();
            p.state.xmm[3] = [0x5a; 16];
            for rc in 0..4 {
                for pc in [0, 0x200, 0x300] {
                    p.state.x87.control = 0x7f | pc | rc << 10;
                    for config in configs() {
                        for budget in [1, 2, 3, 100] {
                            check(&p, config, budget, false);
                            if !config.1 && budget == 100 {
                                let mut cpu = executor(&p, config, false);
                                if cpu.run(p.stop, 100).unwrap() != RunOutcome::ReachedStop {
                                    continue;
                                }
                                let forbidden = iced_x86::Decoder::new(32, &p.code, 0)
                                    .iter()
                                    .find(|i| {
                                        matches!(
                                            i.mnemonic(),
                                            iced_x86::Mnemonic::Fsin
                                                | iced_x86::Mnemonic::Fcos
                                                | iced_x86::Mnemonic::Fsincos
                                                | iced_x86::Mnemonic::Fptan
                                                | iced_x86::Mnemonic::Fpatan
                                                | iced_x86::Mnemonic::F2xm1
                                                | iced_x86::Mnemonic::Fyl2x
                                                | iced_x86::Mnemonic::Fyl2xp1
                                                | iced_x86::Mnemonic::Fscale
                                                | iced_x86::Mnemonic::Fprem
                                                | iced_x86::Mnemonic::Fprem1
                                        )
                                    })
                                    .unwrap()
                                    .code();
                                assert!(
                                    cpu.interpreted_forms(2)
                                        .iter()
                                        .all(|&(code, count)| code != forbidden || count == 0),
                                    "leaf {forbidden:?} used interpret: value={value:?} control={:#x} config={config:?}",
                                    p.state.x87.control
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4, A64-VER-1, A64-MEM-7
#[test]
fn common_strings_cover_directions_repeats_segments_and_boundaries() {
    for form in 0..15 {
        for prefix in [0u8, 0xf2, 0xf3] {
            for segment in [0u8, 0x64, 0x65] {
                for reverse in [false, true] {
                    for count in [0, 1, 7] {
                        let mut p = fixture(|a| {
                            a.add(edx, ecx).unwrap();
                            let mut bytes = Vec::new();
                            if prefix != 0 {
                                bytes.push(prefix);
                            }
                            if segment != 0 {
                                bytes.push(segment);
                            }
                            if form % 3 == 1 {
                                bytes.push(0x66);
                            }
                            bytes.push(
                                [
                                    0xa4, 0xa5, 0xa5, 0xaa, 0xab, 0xab, 0xac, 0xad, 0xad, 0xae,
                                    0xaf, 0xaf, 0xa6, 0xa7, 0xa7,
                                ][form],
                            );
                            a.db(&bytes).unwrap();
                            a.adc(edx, 0).unwrap();
                        });
                        p.data.fill(0x77);
                        p.state.regs[0] = 0x77777777;
                        p.state.regs[1] = count;
                        p.state.regs[6] = DATA + if reverse { 4104 } else { 4088 };
                        p.state.regs[7] = DATA + if reverse { 8200 } else { 8184 };
                        if segment == 0x64 {
                            p.state.fs_base = DATA;
                            p.state.regs[6] -= DATA;
                        }
                        p.state.eflags = (p.state.eflags & !0x400) | u32::from(reverse) << 10;
                        for config in configs() {
                            for budget in [1, 3, 100] {
                                check(&p, config, budget, prefix == 0 && config.2 != Checks::Slow);
                            }
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4, A64-VER-1, A64-VER-2, A64-FLAG-4
#[test]
fn common_integer_helpers_are_native_across_counts_and_widths() {
    for width in [1, 2, 4] {
        for form in 0..11 {
            if width == 1 && form >= 9 {
                continue;
            }
            for count in [0u32, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32] {
                for value in [0u32, 0x81, 0x8001, 0x80000001, u32::MAX] {
                    let mut p = fixture(|a| {
                        a.add(edx, ecx).unwrap();
                        macro_rules! operation {
                            ($dst:expr, $src:expr) => {
                                match form {
                                    0 => a.shl($dst, cl),
                                    1 => a.shr($dst, cl),
                                    2 => a.sar($dst, cl),
                                    3 => a.rol($dst, cl),
                                    4 => a.ror($dst, cl),
                                    5 => a.rcl($dst, cl),
                                    6 => a.rcr($dst, cl),
                                    7 => a.adc($dst, $src),
                                    8 => a.sbb($dst, $src),
                                    9 => a.shld($dst, $src, cl),
                                    _ => a.shrd($dst, $src, cl),
                                }
                                .unwrap()
                            };
                        }
                        match width {
                            1 => {
                                // Double shifts have no byte encoding.
                                match form {
                                    0 => a.shl(byte_ptr(ebx), cl),
                                    1 => a.shr(byte_ptr(ebx), cl),
                                    2 => a.sar(byte_ptr(ebx), cl),
                                    3 => a.rol(byte_ptr(ebx), cl),
                                    4 => a.ror(byte_ptr(ebx), cl),
                                    5 => a.rcl(byte_ptr(ebx), cl),
                                    6 => a.rcr(byte_ptr(ebx), cl),
                                    7 => a.adc(byte_ptr(ebx), al),
                                    _ => a.sbb(byte_ptr(ebx), al),
                                }
                                .unwrap();
                            }
                            2 => operation!(word_ptr(ebx), ax),
                            _ => operation!(dword_ptr(ebx), eax),
                        }
                        a.adc(edi, 0).unwrap();
                    });
                    p.state.regs[1] = count;
                    p.data[..4].copy_from_slice(&value.to_le_bytes());
                    for config in configs() {
                        for budget in [2, 100] {
                            check(&p, config, budget, config.2 != Checks::Slow);
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-ISA-6, A64-MEM-7, A64-VER-1, A64-VER-2
#[test]
fn common_locked_forms_use_memory_for_shared_pages_and_native_private_pages() {
    for host in [4096, 16384]
        .into_iter()
        .filter(|host| host % unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize == 0)
    {
        for form in 0..6 {
            let mut p = fixture(|a| {
                a.add(edx, ecx).unwrap();
                match form {
                    0 => a.lock().cmpxchg(dword_ptr(ebx), ecx),
                    1 => a.lock().cmpxchg8b(qword_ptr(ebx)),
                    2 => a.lock().xadd(dword_ptr(ebx), ecx),
                    3 => a.lock().adc(dword_ptr(ebx), ecx),
                    4 => a.lock().inc(byte_ptr(ebx)),
                    _ => a.lock().bts(dword_ptr(ebx), ecx),
                }
                .unwrap();
                a.adc(edi, 0).unwrap();
            });
            p.data[..8].copy_from_slice(&0x8765432181234567u64.to_le_bytes());
            if form == 5 {
                p.state.regs[1] = 4096 * 8;
            }
            for shared in [false, true] {
                for config in configs() {
                    let make = |interpreted| {
                        let mut cpu = executor_on_host(&p, config, interpreted, host);
                        if shared {
                            let (at, length) = if form == 5 {
                                (DATA + 4096, 4096)
                            } else {
                                (DATA, DATA_LEN)
                            };
                            cpu.memory.unmap(at, length).unwrap();
                            cpu.memory.map_shared_audio(at, length).unwrap();
                            cpu.memory.write(DATA, &p.data).unwrap();
                        }
                        cpu
                    };
                    let mut a = make(false);
                    let mut b = make(true);
                    assert_eq!(a.run(p.stop, 100).unwrap(), b.run(p.stop, 100).unwrap());
                    assert_eq!(
                        a.state.regs, b.state.regs,
                        "host={host} form={form} shared={shared} config={config:?}"
                    );
                    same(&a, &b);
                    if !config.1 && !shared && config.2 != Checks::Slow {
                        assert_eq!(a.native.as_ref().unwrap().diagnostics.helpers, 0);
                    }
                    if !config.1 && shared {
                        assert!(a.native.as_ref().unwrap().diagnostics.helpers > 0);
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4, A64-VER-1, A64-VER-2
#[test]
fn common_partial_stacks_and_segment_reads_match_on_both_host_sizes() {
    let p = fixture(|a| {
        a.pushad().unwrap();
        a.popad().unwrap();
        a.pusha().unwrap();
        a.popa().unwrap();
        a.push(ax).unwrap();
        a.pop(cx).unwrap();
        a.pushf().unwrap();
        a.popf().unwrap();
        a.lahf().unwrap();
        a.mov(edx, fs).unwrap();
        a.mov(word_ptr(ebx), gs).unwrap();
    });
    for host in [4096, 16384]
        .into_iter()
        .filter(|host| host % unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize == 0)
    {
        for config in configs() {
            for budget in [1, 3, 6, 8, 100] {
                let make = |interpreted| executor_on_host(&p, config, interpreted, host);
                let mut a = make(false);
                let mut b = make(true);
                assert_eq!(
                    a.run(p.stop, budget).unwrap(),
                    b.run(p.stop, budget).unwrap()
                );
                assert_eq!(
                    a.state.regs, b.state.regs,
                    "host={host} budget={budget} config={config:?}"
                );
                same(&a, &b);
                assert_eq!(a.run(p.stop, 100).unwrap(), b.run(p.stop, 100).unwrap());
                same(&a, &b);
            }
        }
    }
}

fn executor_on_host(p: &Program, config: Config, interpreted: bool, host: usize) -> Executor {
    let mut memory = Memory::window_at_host_size(BASE, LEN, host).unwrap();
    memory.map(BASE, LEN, 0).unwrap();
    load(&mut memory, p);
    let mut cpu = Executor::create(memory, true).unwrap();
    let native = cpu.native.as_mut().unwrap();
    native.features = config.0;
    native.first_visits = config.1;
    native.checks = config.2;
    cpu.state = p.state.clone();
    cpu.interpreter_only = interpreted;
    cpu
}

// spec: A64-TR-4, A64F-FP-5, A64-MEM-7, A64-VER-1, A64-VER-2
#[test]
fn common_remaining_simd_forms_match_values_controls_and_memory() {
    for form in 0..23 {
        for special in 0..9 {
            for memory in [false, true] {
                let mut p = fixture(|a| {
                    a.add(eax, ecx).unwrap();
                    macro_rules! op {
                        ($name:ident) => {
                            if memory {
                                a.$name(xmm0, xmmword_ptr(ebx))
                            } else {
                                a.$name(xmm0, xmm1)
                            }
                            .unwrap()
                        };
                    }
                    match form {
                        0 => op!(divpd),
                        1 => op!(sqrtps),
                        2 => op!(sqrtpd),
                        3 => {
                            if memory {
                                a.sqrtss(xmm0, dword_ptr(ebx))
                            } else {
                                a.sqrtss(xmm0, xmm1)
                            }
                            .unwrap();
                        }
                        4 => {
                            if memory {
                                a.sqrtsd(xmm0, qword_ptr(ebx))
                            } else {
                                a.sqrtsd(xmm0, xmm1)
                            }
                            .unwrap();
                        }
                        5 => op!(minpd),
                        6 => op!(maxpd),
                        7 => {
                            if memory {
                                a.minsd(xmm0, qword_ptr(ebx))
                            } else {
                                a.minsd(xmm0, xmm1)
                            }
                            .unwrap();
                        }
                        8 => {
                            if memory {
                                a.maxsd(xmm0, qword_ptr(ebx))
                            } else {
                                a.maxsd(xmm0, xmm1)
                            }
                            .unwrap();
                        }
                        9 => op!(cvtpd2dq),
                        10 => op!(cvttpd2dq),
                        11 => {
                            if memory {
                                a.cvtpd2pi(mm0, xmmword_ptr(ebx))
                            } else {
                                a.cvtpd2pi(mm0, xmm1)
                            }
                            .unwrap();
                        }
                        12 => {
                            if memory {
                                a.cvttpd2pi(mm0, xmmword_ptr(ebx))
                            } else {
                                a.cvttpd2pi(mm0, xmm1)
                            }
                            .unwrap();
                        }
                        13 => op!(cvtpd2ps),
                        14 => {
                            if memory {
                                a.cvtpi2ps(xmm0, qword_ptr(ebx))
                            } else {
                                a.cvtpi2ps(xmm0, mm1)
                            }
                            .unwrap();
                        }
                        15 => {
                            if memory {
                                a.cvtpi2pd(xmm0, qword_ptr(ebx))
                            } else {
                                a.cvtpi2pd(xmm0, mm1)
                            }
                            .unwrap();
                        }
                        16 => op!(rcpps),
                        17 => {
                            if memory {
                                a.rcpss(xmm0, dword_ptr(ebx))
                            } else {
                                a.rcpss(xmm0, xmm1)
                            }
                            .unwrap();
                        }
                        18 => op!(rsqrtps),
                        19 => {
                            if memory {
                                a.rsqrtss(xmm0, dword_ptr(ebx))
                            } else {
                                a.rsqrtss(xmm0, xmm1)
                            }
                            .unwrap();
                        }
                        20 => {
                            a.maskmovq(mm0, mm1).unwrap();
                        }
                        21 => {
                            a.maskmovdqu(xmm0, xmm1).unwrap();
                        }
                        _ => {
                            a.pextrw(word_ptr(ebx), xmm1, 3).unwrap();
                        }
                    }
                    a.adc(edx, 0).unwrap();
                });
                let double = matches!(form, 0 | 2 | 4..=13);
                let d = [
                    0.0f64.to_bits(),
                    (-0.0f64).to_bits(),
                    2.5f64.to_bits(),
                    (-3.5f64).to_bits(),
                    f64::INFINITY.to_bits(),
                    0x7ff8000000000011,
                    0x7ff0000000000001,
                    1,
                    0x41e0000000100000,
                ][special];
                let f = [
                    0.0f32.to_bits(),
                    (-0.0f32).to_bits(),
                    2.5f32.to_bits(),
                    (-3.5f32).to_bits(),
                    f32::INFINITY.to_bits(),
                    0x7fc00011,
                    0x7f800001,
                    1,
                    0x7f7fffff,
                ][special];
                for bytes in p.state.xmm[1].chunks_mut(if double { 8 } else { 4 }) {
                    if double {
                        bytes.copy_from_slice(&d.to_le_bytes());
                    } else {
                        bytes.copy_from_slice(&f.to_le_bytes());
                    }
                }
                p.state.mmx[1] = u64::from_le_bytes(p.state.xmm[1][..8].try_into().unwrap());
                p.state.xmm[0] = [0x5a; 16];
                p.data[..16].copy_from_slice(&p.state.xmm[1]);
                p.state.regs[7] = DATA + 4090;
                for mxcsr in [0x1f80, 0x1fa0, 0x3f80, 0x5f80, 0x7f80, 0x9fc0, 0x1f00] {
                    p.state.mxcsr = mxcsr;
                    for config in configs() {
                        for budget in [2, 100] {
                            check(&p, config, budget, false);
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-ISA-6, A64-VER-1
#[test]
fn common_rejected_lock_encodings_raise_ud() {
    struct Undefined;
    impl adv_cpu_core::trap::ExecutionTrap for Undefined {
        fn call(
            &mut self,
            state: &mut CpuState,
            _: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 6);
            assert_eq!(state.eip, CODE);
            Ok(true)
        }
    }
    for code in [
        &[0xf0, 0x01, 0xc8][..],
        &[0xf0, 0x0f, 0xc1, 0xc8],
        &[0xf0, 0xd1, 0x23],
    ] {
        let p = fixture(|a| {
            a.db(code).unwrap();
        });
        for config in configs() {
            let mut a = executor(&p, config, false);
            let mut b = executor(&p, config, true);
            let outcome = a
                .run_with_traps(p.stop, 100, &mut Undefined, false)
                .unwrap();
            assert_eq!(outcome, RunOutcome::Parked);
            assert_eq!(
                outcome,
                b.run_with_traps(p.stop, 100, &mut Undefined, false)
                    .unwrap()
            );
            same(&a, &b);
        }
    }
}

// spec: A64-TR-4, A64-MEM-7, A64-VER-1, A64-VER-2
#[test]
fn common_strings_preserve_partial_state_at_blocked_pages() {
    let native = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    for host in [4096, 16384].into_iter().filter(|host| host % native == 0) {
        for form in 0..15 {
            for reverse in [false, true] {
                for prefix in [0u8, 0xf2, 0xf3] {
                    let mut p = fixture(|a| {
                        let mut bytes = Vec::new();
                        if prefix != 0 {
                            bytes.push(prefix);
                        }
                        if form % 3 == 1 {
                            bytes.push(0x66);
                        }
                        bytes.push(
                            [
                                0xa4, 0xa5, 0xa5, 0xaa, 0xab, 0xab, 0xac, 0xad, 0xad, 0xae, 0xaf,
                                0xaf, 0xa6, 0xa7, 0xa7,
                            ][form],
                        );
                        a.db(&bytes).unwrap();
                    });
                    p.data.fill(0x77);
                    p.state.regs[0] = 0x77777777;
                    p.state.regs[1] = 7;
                    p.state.regs[6] = DATA + if reverse { 4100 } else { 4092 };
                    p.state.regs[7] = DATA + if reverse { 8196 } else { 8188 };
                    p.state.eflags = (p.state.eflags & !0x400) | u32::from(reverse) << 10;
                    for config in configs() {
                        for budget in [1, 3, 100] {
                            let mut a = executor_on_host(&p, config, false, host);
                            let mut b = executor_on_host(&p, config, true, host);
                            let source = matches!(form / 3, 0 | 2 | 4);
                            let page = DATA
                                + if source {
                                    if reverse { 0 } else { 4096 }
                                } else if reverse {
                                    4096
                                } else {
                                    8192
                                };
                            for cpu in [&mut a, &mut b] {
                                cpu.protect(page, 4096, 0).unwrap();
                            }
                            assert_eq!(
                                a.run(p.stop, budget).unwrap(),
                                b.run(p.stop, budget).unwrap()
                            );
                            same(&a, &b);
                            for cpu in [&mut a, &mut b] {
                                cpu.protect(page, 4096, 3).unwrap();
                            }
                            assert_eq!(a.run(p.stop, 100).unwrap(), b.run(p.stop, 100).unwrap());
                            same(&a, &b);
                        }
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "before/after helper census on the authored differential corpus"]
fn common_form_conversion_counts() {
    let forms: &[(&str, &[u8])] = &[
        ("pop_m32", &[0x8f, 0x03]),
        ("pop_esp", &[0x5c]),
        ("pushfd", &[0x9c]),
        ("popfd", &[0x9d]),
        ("pushf", &[0x66, 0x9c]),
        ("popf", &[0x66, 0x9d]),
        ("push_r16", &[0x66, 0x50]),
        ("push_m16", &[0x66, 0xff, 0x33]),
        ("push_imm16", &[0x66, 0x68, 0x34, 0x12]),
        ("push_imm8_word", &[0x66, 0x6a, 1]),
        ("pop_r16", &[0x66, 0x59]),
        ("pop_m16", &[0x66, 0x8f, 0x03]),
        ("pushad", &[0x60]),
        ("popad", &[0x61]),
        ("pusha", &[0x66, 0x60]),
        ("popa", &[0x66, 0x61]),
        ("lahf", &[0x9f]),
        ("lea_r16", &[0x66, 0x8d, 0x0b]),
        ("leave16", &[0x66, 0xc9]),
        ("read_segment32", &[0x8c, 0xe0]),
        ("read_segment16", &[0x66, 0x8c, 0xe0]),
        ("read_segment_memory", &[0x8c, 0x23]),
        ("fld_m80", &[0xdb, 0x2b]),
        ("fstp_m80", &[0xdb, 0x3b]),
        ("fsin", &[0xd9, 0xfe]),
        ("fcos", &[0xd9, 0xff]),
        ("fsincos", &[0xd9, 0xfb]),
        ("fptan", &[0xd9, 0xf2]),
        ("fpatan", &[0xd9, 0xf3]),
        ("f2xm1", &[0xd9, 0xf0]),
        ("fyl2x", &[0xd9, 0xf1]),
        ("fyl2xp1", &[0xd9, 0xf9]),
        ("fscale", &[0xd9, 0xfd]),
        ("fprem", &[0xd9, 0xf8]),
        ("fprem1", &[0xd9, 0xf5]),
        ("cmpxchg32_unaligned", &[0x0f, 0xb1, 0x4b, 1]),
        ("cmpxchg8b_unaligned", &[0x0f, 0xc7, 0x4b, 1]),
        ("lock_cmpxchg", &[0xf0, 0x0f, 0xb1, 0x0b]),
        ("lock_cmpxchg8b", &[0xf0, 0x0f, 0xc7, 0x0b]),
        ("lock_xadd", &[0xf0, 0x0f, 0xc1, 0x0b]),
        ("lock_adc", &[0xf0, 0x11, 0x0b]),
        ("lock_sbb", &[0xf0, 0x19, 0x0b]),
        ("adc_m8", &[0x10, 0x03]),
        ("adc_m16", &[0x66, 0x11, 0x03]),
        ("adc_m32", &[0x11, 0x03]),
        ("sbb_m8", &[0x18, 0x03]),
        ("sbb_m16", &[0x66, 0x19, 0x03]),
        ("sbb_m32", &[0x19, 0x03]),
        ("shl_m8_cl", &[0xd2, 0x23]),
        ("shr_m8_cl", &[0xd2, 0x2b]),
        ("sar_m8_cl", &[0xd2, 0x3b]),
        ("rol_m8_cl", &[0xd2, 0x03]),
        ("ror_m8_cl", &[0xd2, 0x0b]),
        ("rcl_m8_cl", &[0xd2, 0x13]),
        ("rcr_m8_cl", &[0xd2, 0x1b]),
        ("shl_m16_cl", &[0x66, 0xd3, 0x23]),
        ("shr_m16_cl", &[0x66, 0xd3, 0x2b]),
        ("sar_m16_cl", &[0x66, 0xd3, 0x3b]),
        ("rol_m16_cl", &[0x66, 0xd3, 0x03]),
        ("ror_m16_cl", &[0x66, 0xd3, 0x0b]),
        ("rcl_m16_cl", &[0x66, 0xd3, 0x13]),
        ("rcr_m16_cl", &[0x66, 0xd3, 0x1b]),
        ("shr_m32_imm", &[0xc1, 0x2b, 3]),
        ("rcl_m32_cl", &[0xd3, 0x13]),
        ("rcr_m32_cl", &[0xd3, 0x1b]),
        ("shld16_cl", &[0x66, 0x0f, 0xa5, 0x03]),
        ("shrd16_cl", &[0x66, 0x0f, 0xad, 0x03]),
        ("shld32_cl", &[0x0f, 0xa5, 0x03]),
        ("shrd32_cl", &[0x0f, 0xad, 0x03]),
        ("zero_count_store", &[0xc1, 0x23, 0]),
        ("fs_movs", &[0x64, 0xa5]),
        ("repne_stos", &[0xf2, 0xab]),
        ("gs_integer", &[0x65, 0x8b, 0x03]),
        ("gs_simd", &[0x65, 0x0f, 0x10, 0x03]),
        ("gs_fld_m80", &[0x65, 0xdb, 0x2b]),
        ("gs_fstp_m80", &[0x65, 0xdb, 0x3b]),
        ("divpd", &[0x66, 0x0f, 0x5e, 0xc1]),
        ("sqrtps", &[0x0f, 0x51, 0xc1]),
        ("sqrtpd", &[0x66, 0x0f, 0x51, 0xc1]),
        ("sqrtss", &[0xf3, 0x0f, 0x51, 0xc1]),
        ("sqrtsd", &[0xf2, 0x0f, 0x51, 0xc1]),
        ("minpd", &[0x66, 0x0f, 0x5d, 0xc1]),
        ("maxpd", &[0x66, 0x0f, 0x5f, 0xc1]),
        ("minsd", &[0xf2, 0x0f, 0x5d, 0xc1]),
        ("maxsd", &[0xf2, 0x0f, 0x5f, 0xc1]),
        ("cvtpd2dq", &[0xf2, 0x0f, 0xe6, 0xc1]),
        ("cvttpd2dq", &[0x66, 0x0f, 0xe6, 0xc1]),
        ("cvtpd2pi", &[0x66, 0x0f, 0x2d, 0xc1]),
        ("cvttpd2pi", &[0x66, 0x0f, 0x2c, 0xc1]),
        ("cvtpd2ps", &[0x66, 0x0f, 0x5a, 0xc1]),
        ("cvtpi2ps", &[0x0f, 0x2a, 0xc1]),
        ("cvtpi2pd", &[0x66, 0x0f, 0x2a, 0xc1]),
        ("rcpps", &[0x0f, 0x53, 0xc1]),
        ("rcpss", &[0xf3, 0x0f, 0x53, 0xc1]),
        ("rsqrtps", &[0x0f, 0x52, 0xc1]),
        ("rsqrtss", &[0xf3, 0x0f, 0x52, 0xc1]),
        ("maskmovq", &[0x0f, 0xf7, 0xc1]),
        ("maskmovdqu", &[0x66, 0x0f, 0xf7, 0xc1]),
        ("pextrw_memory", &[0x66, 0x0f, 0x3a, 0x15, 0x0b, 3]),
    ];
    for &(name, bytes) in forms {
        let mut p = fixture(|a| {
            a.db(bytes).unwrap();
        });
        p.data[..10]
            .copy_from_slice(&adv_cpu_core::X87Register::from_parts(0xc000000000000000, 0x3ffd).0);
        p.state.regs[1] = 3;
        p.state.regs[6] = DATA + 64;
        p.state.regs[7] = DATA + 128;
        p.state.regs[5] = STACK_TOP;
        if name == "fs_movs" {
            p.state.fs_base = DATA;
            p.state.regs[6] = 64;
        }
        p.state.x87.push(adv_cpu_core::X87Register::from_parts(
            0xa000000000000000,
            0x4000,
        ));
        p.state.x87.push(adv_cpu_core::X87Register::from_parts(
            0xc000000000000000,
            0x3ffd,
        ));
        p.state.refresh_mmx();
        for lane in p.state.xmm[0].chunks_mut(8) {
            lane.copy_from_slice(&1.25f64.to_bits().to_le_bytes());
        }
        for lane in p.state.xmm[1].chunks_mut(8) {
            lane.copy_from_slice(&4.0f64.to_bits().to_le_bytes());
        }
        p.state.mxcsr = 0x1fa0;
        let mut native = executor(
            &p,
            (Features { flagm: false }, false, Checks::Adaptive),
            false,
        );
        native.write(STACK_TOP, &STACK_TOP.to_le_bytes()).unwrap();
        // POPF receives masked flags without TF.
        if name == "popfd" || name == "popf" {
            native.write(STACK_TOP, &0x202u32.to_le_bytes()).unwrap();
        }
        let mut oracle = executor(
            &p,
            (Features { flagm: false }, false, Checks::Adaptive),
            true,
        );
        oracle.write(STACK_TOP, &STACK_TOP.to_le_bytes()).unwrap();
        if name == "popfd" || name == "popf" {
            oracle.write(STACK_TOP, &0x202u32.to_le_bytes()).unwrap();
        }
        native.count_tier0(true);
        let outcome = native.run(p.stop, 100).unwrap();
        assert_eq!(outcome, oracle.run(p.stop, 100).unwrap(), "{name}");
        same(&native, &oracle);
        let counts = native.retired_forms().unwrap();
        println!(
            "CENSUS {name} {} {} {} {outcome:?}",
            counts[1], counts[2], counts[3]
        );
    }
    for (name, opcode, word) in [
        ("movsb", 0xa4, false),
        ("movsw", 0xa5, true),
        ("movsd", 0xa5, false),
        ("stosb", 0xaa, false),
        ("stosw", 0xab, true),
        ("stosd", 0xab, false),
        ("lodsb", 0xac, false),
        ("lodsw", 0xad, true),
        ("lodsd", 0xad, false),
        ("scasb", 0xae, false),
        ("scasw", 0xaf, true),
        ("scasd", 0xaf, false),
        ("cmpsb", 0xa6, false),
        ("cmpsw", 0xa7, true),
        ("cmpsd", 0xa7, false),
    ] {
        let mut total = [0; 3];
        let mut unsupported = 0;
        let mut samples = 0;
        for repeat in [0, 0xf2, 0xf3] {
            for segment in [0, 0x26, 0x2e, 0x36, 0x3e, 0x64, 0x65] {
                for reverse in [false, true] {
                    for count in [0, 1, 7] {
                        let mut bytes = Vec::new();
                        bytes.extend(
                            [repeat, segment, if word { 0x66 } else { 0 }]
                                .into_iter()
                                .filter(|b| *b != 0),
                        );
                        bytes.push(opcode);
                        let mut p = fixture(|a| {
                            a.db(&bytes).unwrap();
                        });
                        p.data.fill(0x77);
                        p.state.regs[0] = 0x77777777;
                        p.state.regs[1] = count;
                        p.state.regs[6] = DATA + 64;
                        p.state.regs[7] = DATA + 128;
                        if segment == 0x64 {
                            p.state.fs_base = DATA;
                            p.state.regs[6] = 64;
                        }
                        p.state.eflags = (p.state.eflags & !0x400) | u32::from(reverse) << 10;
                        let config = (Features { flagm: false }, false, Checks::Adaptive);
                        let mut native = executor(&p, config, false);
                        let mut oracle = executor(&p, config, true);
                        native.count_tier0(true);
                        let outcome = native.run(p.stop, 100).unwrap();
                        assert_eq!(outcome, oracle.run(p.stop, 100).unwrap(), "{name}");
                        same(&native, &oracle);
                        let counts = native.retired_forms().unwrap();
                        for i in 0..3 {
                            total[i] += counts[i + 1];
                        }
                        unsupported +=
                            u32::from(matches!(outcome, RunOutcome::UnsupportedInstruction { .. }));
                        samples += 1;
                    }
                }
            }
        }
        println!(
            "STRING_CENSUS {name} {samples} {} {} {} unsupported={unsupported}",
            total[0], total[1], total[2]
        );
    }
}

// spec: A64F-FP-8, A64-MEM-7, A64-FLAG-4, A64-VER-1, A64-VER-2
#[test]
fn common_m80_faults_preserve_the_uncommitted_stack_and_store() {
    let native = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    for host in [4096, 16384].into_iter().filter(|host| host % native == 0) {
        for scenario in 0..4 {
            let src = if scenario == 2 { 4090 } else { 0 };
            let dst = if scenario == 3 { 4090 } else { 4096 };
            let mut p = fixture(|a| {
                a.fld(tbyte_ptr(ebx + src)).unwrap();
                a.fstp(tbyte_ptr(ebx + dst)).unwrap();
            });
            p.data[src as usize..src as usize + 10].copy_from_slice(
                &adv_cpu_core::X87Register::from_parts(0xc000000000000000, 0x3ffd).0,
            );
            let page = DATA + if scenario == 0 { 0 } else { 4096 };
            let permission = if scenario <= 2 {
                if scenario == 1 { 1 } else { 0 }
            } else {
                1
            };
            for config in configs() {
                let mut a = executor_on_host(&p, config, false, host);
                let mut b = executor_on_host(&p, config, true, host);
                for cpu in [&mut a, &mut b] {
                    cpu.protect(page, 4096, permission).unwrap();
                }
                assert_eq!(a.run(p.stop, 100).unwrap(), b.run(p.stop, 100).unwrap());
                same(&a, &b);
                for cpu in [&mut a, &mut b] {
                    cpu.protect(page, 4096, 3).unwrap();
                }
                assert_eq!(a.run(p.stop, 100).unwrap(), b.run(p.stop, 100).unwrap());
                same(&a, &b);
            }
        }
    }
}

// spec: A64-TR-4, A64-CTL-7, A64-VER-1
#[test]
fn common_pop_flags_setting_tf_single_steps_the_next_instruction() {
    for word in [false, true] {
        let p = fixture(|a| {
            if word {
                a.db(&[0x66, 0x68, 0x02, 0x03]).unwrap();
                a.popf().unwrap();
            } else {
                a.push(0x302i32).unwrap();
                a.popfd().unwrap();
            }
            a.inc(eax).unwrap();
        });
        for config in configs() {
            for budget in [1, 2, 3, 100] {
                check(&p, config, budget, false);
            }
        }
    }
}
