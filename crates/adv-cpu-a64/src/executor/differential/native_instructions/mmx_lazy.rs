use super::*;

fn configs() -> impl Iterator<Item = Config> {
    [false, true].into_iter().flat_map(|flagm| {
        [false, true].into_iter().flat_map(move |first| {
            [Checks::Adaptive, Checks::All, Checks::Slow]
                .into_iter()
                .map(move |checks| (Features { flagm }, first, checks))
        })
    })
}

fn prefix(a: &mut CodeAssembler) {
    a.movd(mm0, eax).unwrap();
    a.movd(mm2, edx).unwrap();
    a.punpcklbw(mm0, mm2).unwrap();
    a.pmullw(mm0, mm2).unwrap();
    a.psrlw(mm0, 3).unwrap();
    a.movq(mm7, mm0).unwrap();
    a.paddw(mm7, mm2).unwrap();
    a.packuswb(mm0, mm7).unwrap();
}

fn seeded(mut program: Program) -> Program {
    program.state.mmx = std::array::from_fn(|n| 0x123456789abcdef0 ^ n as u64);
    program.state.x87.regs = std::array::from_fn(|n| {
        adv_cpu_core::X87Register::from_parts(0x9876543210abcdef ^ n as u64, 0x3fff)
    });
    program.state.x87.status = 5 << 11;
    program
}

fn cuts(program: &Program, budgets: &[u64]) {
    for config in configs() {
        for &budget in budgets {
            let mut native = executor(program, config, false);
            let mut oracle = executor(program, config, true);
            assert_eq!(
                native.run(program.stop, budget).unwrap(),
                oracle.run(program.stop, budget).unwrap()
            );
            same(&native, &oracle);
            assert_eq!(
                native.run(program.stop, 1000).unwrap(),
                oracle.run(program.stop, 1000).unwrap()
            );
            same(&native, &oracle);
        }
    }
}

// spec: A64F-FP-4, A64-REG-5, A64-CTL-7, A64-VER-1
#[test]
fn mmx_prefix_survives_side_links_merges_calls_backedges_and_budget_refunds() {
    for taken in [0, 1] {
        let program = seeded(fixture(|a| {
            let mut repeat = a.create_label();
            let mut merge = a.create_label();
            let mut callee = a.create_label();
            let mut done = a.create_label();
            a.mov(ecx, 3).unwrap();
            a.set_label(&mut repeat).unwrap();
            prefix(a);
            a.cmp(edi, 0).unwrap();
            a.je(merge).unwrap();
            a.paddb(mm2, mm7).unwrap();
            a.set_label(&mut merge).unwrap();
            a.dec(ecx).unwrap();
            a.jnz(repeat).unwrap();
            // Exceed the unit size, forcing links with a dirty MMX prefix.
            for _ in 0..100 {
                a.paddw(mm7, mm2).unwrap();
            }
            a.call(callee).unwrap();
            a.mov(edx, CODE + 0x800).unwrap();
            a.call(edx).unwrap();
            a.jmp(done).unwrap();
            a.set_label(&mut callee).unwrap();
            a.pxor(mm0, mm7).unwrap();
            a.ret().unwrap();
            a.set_label(&mut done).unwrap();
            a.movq(qword_ptr(DATA as i32), mm0).unwrap();
        }));
        let mut program = program;
        program.state.regs[7] = taken;
        // A separately dispatched callee exercises real call/return boundaries.
        let mut callee = CodeAssembler::new(32).unwrap();
        callee.paddw(mm2, mm7).unwrap();
        callee.ret().unwrap();
        program.code.resize(0x800, 0x90);
        program
            .code
            .extend(callee.assemble(u64::from(CODE + 0x800)).unwrap());
        cuts(&program, &[1, 8, 13, 21, 38, 95, 96, 97, 150, 1000]);
    }
}

// spec: A64F-FP-4, A64-TR-4, A64-VER-1
#[test]
fn mmx_prefix_is_visible_to_helpers_x87_emms_and_saved_contexts() {
    for observer in 0..7 {
        let program = seeded(fixture(|a| {
            prefix(a);
            match observer {
                0 => a.fld(dword_ptr(DATA as i32)),
                1 => a.fnsave(ptr(DATA as i32)),
                2 => a.emms(),
                3 => a.fxsave(ptr(DATA as i32)),
                4 => a.pushad(),
                5 => a.fnstenv(ptr(DATA as i32)),
                _ => a.movaps(xmm0, xmmword_ptr((DATA + 1) as i32)),
            }
            .unwrap();
            a.movq(mm1, mm2).unwrap();
            a.movq(qword_ptr((DATA + 1024) as i32), mm7).unwrap();
        }));
        cuts(&program, &[1, 7, 8, 9, 10, 100]);
    }
}

// spec: A64F-FP-2 A64-REG-5 A64-MEM-7 A64-VER-1
#[test]
fn x87_value_images_cross_guest_pages_with_live_xmm() {
    for fx in [false, true] {
        let at = DATA + 0xff0;
        let mut program = fixture(|a| {
            a.pxor(xmm7, xmm1).unwrap();
            a.fld1().unwrap();
            if fx {
                a.fxsave(ptr(at as i32)).unwrap();
            } else {
                a.fnsave(ptr(at as i32)).unwrap();
            }
            a.fldz().unwrap();
            if fx {
                a.fxrstor(ptr(at as i32)).unwrap();
            } else {
                a.frstor(ptr(at as i32)).unwrap();
            }
            a.fnstsw(ax).unwrap();
            a.movups(xmmword_ptr((DATA + 0x300) as i32), xmm7).unwrap();
        });
        program.state.xmm[1] = [0x29; 16];
        program.state.xmm[7] = [0x92; 16];
        let config = (Features::default(), false, Checks::Adaptive);
        let mut native = executor(&program, config, false);
        let mut oracle = executor(&program, config, true);
        assert_eq!(native.run(program.stop, 100), oracle.run(program.stop, 100));
        same(&native, &oracle);
        assert_eq!(native.stats.interpreter_entries, 0);
        assert_eq!(native.native.as_ref().unwrap().diagnostics.helpers, 0);
    }
}

// spec: A64F-FP-4, A64-FLAG-4, A64-MEM-5, A64-VER-1
#[test]
fn mmx_dirty_subset_recovers_direct_and_checked_faults_and_reexecutes_memory_forms() {
    for kind in 0..5 {
        for crossing in [false, true] {
            let address = if crossing {
                DATA + DATA_LEN as u32 - 2
            } else {
                HOLE
            };
            let program = seeded(fixture(|a| {
                prefix(a);
                // Keep lazy arithmetic flags live at the memory fault as well.
                a.add(eax, 17).unwrap();
                match kind {
                    0 => a.mov(edi, dword_ptr(address as i32)),
                    1 => a.movd(dword_ptr(address as i32), mm0),
                    2 => a.movq(qword_ptr(address as i32), mm7),
                    3 => a.movq(mm0, qword_ptr(address as i32)),
                    _ => a.paddw(mm2, qword_ptr(address as i32)),
                }
                .unwrap();
                a.adc(edx, 0).unwrap();
                a.movq(qword_ptr(DATA as i32), mm7).unwrap();
            }));
            for config in configs() {
                let mut native = executor(&program, config, false);
                let mut oracle = executor(&program, config, true);
                let outcome = native.run(program.stop, 100).unwrap();
                assert!(matches!(
                    outcome,
                    RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                ));
                assert_eq!(outcome, oracle.run(program.stop, 100).unwrap());
                same(&native, &oracle);
                if !config.1 && config.2 == Checks::Adaptive {
                    assert!(native.native.as_ref().unwrap().unchecked_faults > 0);
                }
                for executor in [&mut native, &mut oracle] {
                    executor.protect(address & !4095, 8192, 3).unwrap();
                }
                assert_eq!(
                    native.run(program.stop, 100).unwrap(),
                    oracle.run(program.stop, 100).unwrap()
                );
                same(&native, &oracle);
            }
        }
    }
}

// spec: A64F-FP-4, A64-TR-4, A64-VER-1
#[test]
fn mmx_prefix_survives_fast_paths_with_conditional_helpers() {
    for kind in 0..5 {
        let program = seeded(fixture(|a| {
            prefix(a);
            match kind {
                0 => {
                    a.mov(eax, 123).unwrap();
                    a.cdq().unwrap();
                    a.idiv(ebx).unwrap();
                }
                1 | 2 => {
                    a.mov(edi, DATA + if kind == 1 { 0 } else { 1 }).unwrap();
                    a.lock().cmpxchg8b(qword_ptr(edi)).unwrap();
                }
                _ => {
                    a.mov(ecx, if kind == 3 { 1 } else { 5 }).unwrap();
                    a.mov(edi, DATA + 512).unwrap();
                    a.rep().movsb().unwrap();
                }
            }
            a.fxsave(ptr((DATA + 1024) as i32)).unwrap();
        }));
        cuts(&program, &[1, 8, 9, 11, 13, 100]);
    }
}

// spec: A64-MEM-7, A64F-FP-4
#[test]
fn checked_simd_accesses_complete_and_refuse_without_instruction_helpers() {
    let forms: [fn(&mut CodeAssembler); 15] = [
        |a| a.movd(mm0, dword_ptr(eax)).unwrap(),
        |a| a.movd(dword_ptr(eax), mm7).unwrap(),
        |a| a.movq(mm0, qword_ptr(eax)).unwrap(),
        |a| a.movq(qword_ptr(eax), mm7).unwrap(),
        |a| a.movdqu(xmm0, xmmword_ptr(eax)).unwrap(),
        |a| a.movdqu(xmmword_ptr(eax), xmm0).unwrap(),
        |a| a.punpcklbw(mm2, dword_ptr(eax)).unwrap(),
        |a| a.paddw(mm2, qword_ptr(eax)).unwrap(),
        |a| a.pmullw(xmm0, xmmword_ptr(eax)).unwrap(),
        |a| a.psrlw(mm2, qword_ptr(eax)).unwrap(),
        |a| a.movhps(xmm0, qword_ptr(eax)).unwrap(),
        |a| a.movhps(qword_ptr(eax), xmm0).unwrap(),
        |a| a.addss(xmm3, dword_ptr(eax)).unwrap(),
        |a| a.addsd(xmm4, qword_ptr(eax)).unwrap(),
        |a| a.cvtsi2ss(xmm3, dword_ptr(eax)).unwrap(),
    ];
    for (n, form) in forms.into_iter().enumerate() {
        for address in [DATA + 128, DATA + 4096 - 2, HOLE] {
            // The packed XMM arithmetic form retains its alignment exit.
            let address = if n == 8 { address & !15 } else { address };
            let mut program = seeded(fixture(|a| {
                prefix(a);
                a.pxor(xmm0, xmm1).unwrap();
                a.paddw(xmm0, xmm2).unwrap();
                // Keep lazy flags and both modified SIMD banks live at the access.
                a.add(edx, 17).unwrap();
                a.mov(eax, address).unwrap();
                form(a);
                a.adc(edx, 0).unwrap();
                a.paddw(mm7, mm2).unwrap();
                a.paddw(xmm0, xmm2).unwrap();
                a.movq(qword_ptr((DATA + 512) as i32), mm7).unwrap();
                a.movdqu(xmmword_ptr((DATA + 528) as i32), xmm0).unwrap();
            }));
            program.state.xmm = std::array::from_fn(|n| [17 * n as u8; 16]);
            program.state.xmm[3] = [0; 16];
            program.state.xmm[4] = [0; 16];
            program.state.mxcsr |= 1 << 5;
            for chunk in program.data.as_chunks_mut::<8>().0 {
                chunk.copy_from_slice(&1.0f64.to_le_bytes());
            }
            for flagm in [false, true] {
                let config = (Features { flagm }, false, Checks::Slow);
                let mut native = executor(&program, config, false);
                let mut oracle = executor(&program, config, true);
                let outcome = native.run(program.stop, 100).unwrap();
                assert_eq!(outcome, oracle.run(program.stop, 100).unwrap(), "form {n}");
                same(&native, &oracle);
                let diagnostics = &native.native.as_ref().unwrap().diagnostics;
                assert!(diagnostics.accesses > 0, "form {n} uses access");
                assert_eq!(diagnostics.helpers, 0, "form {n} avoids interpret");
                assert_eq!(diagnostics.refused, u64::from(address == HOLE), "form {n}");
                assert_eq!(
                    matches!(
                        outcome,
                        RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                    ),
                    address == HOLE,
                    "form {n}"
                );
            }
        }
    }
}

// spec: A64F-FP-4, A64-TR-5, A64-REG-5, A64-VER-1
#[test]
fn write_first_and_appended_mmx_loops_avoid_interpret_and_recover_cuts_and_faults() {
    for appended in [false, true] {
        for fault in [false, true] {
            if fault && appended {
                continue;
            }
            for accumulator in [false, true] {
                if accumulator && (appended || fault) {
                    continue;
                }
                let mut p = fixture(|a| {
                    let mut top = a.create_label();
                    if appended {
                        a.cmp(edx, 0).unwrap();
                        a.jne(top).unwrap();
                        a.add(eax, 1).unwrap();
                        a.ret().unwrap();
                    }
                    a.set_label(&mut top).unwrap();
                    a.movq(mm0, qword_ptr(esi)).unwrap();
                    a.paddw(mm0, qword_ptr(edi)).unwrap();
                    if accumulator {
                        a.paddw(mm1, mm0).unwrap();
                        a.movq(qword_ptr(ebx), mm1).unwrap();
                    } else {
                        a.movq(qword_ptr(ebx), mm0).unwrap();
                    }
                    a.add(esi, 8).unwrap();
                    a.add(edi, 8).unwrap();
                    a.add(ebx, 8).unwrap();
                    a.dec(ecx).unwrap();
                    a.jnz(top).unwrap();
                });
                // Stale non-MMX exponent on write-first mm0 after prior FP/emms use;
                // read-first accumulator mm1 already carries a valid MMX tag/exponent.
                p.state.x87.regs = std::array::from_fn(|n| {
                    adv_cpu_core::X87Register::from_parts(
                        0x1122334455667788 ^ n as u64,
                        if accumulator && n == 1 {
                            0xffff
                        } else {
                            0x3fff
                        },
                    )
                });
                p.state.mmx = std::array::from_fn(|n| 0x0102030405060708 * (n as u64 + 1));
                p.state.x87.status = 0;
                p.state.mmx_mode = 0;
                for (n, chunk) in p.data.as_chunks_mut::<8>().0.iter_mut().enumerate() {
                    chunk.copy_from_slice(
                        &(0x0001000200030004u64.wrapping_mul(n as u64 + 1)).to_le_bytes(),
                    );
                }
                p.state.regs[1] = 8; // ecx
                p.state.regs[2] = 1; // edx (take appended path)
                p.state.regs[6] = DATA; // esi
                p.state.regs[7] = DATA + 256; // edi
                p.state.regs[3] = if fault { DATA + 4096 - 16 } else { DATA + 512 }; // ebx

                for config in configs() {
                    for budget in [1, 2, 4, 8, 9, 10, 17, 18, 25, 200] {
                        let mut native = executor(&p, config, false);
                        let mut oracle = executor(&p, config, true);
                        let got = native.run(p.stop, budget).unwrap();
                        let expected = oracle.run(p.stop, budget).unwrap();
                        assert_eq!(
                            got, expected,
                            "appended={appended} fault={fault} acc={accumulator} budget={budget}"
                        );
                        same(&native, &oracle);
                        if matches!(got, RunOutcome::Budget) {
                            assert_eq!(
                                native.run(p.stop, 300).unwrap(),
                                oracle.run(p.stop, 300).unwrap()
                            );
                            same(&native, &oracle);
                        }
                        assert_eq!(
                            native.native.as_ref().unwrap().diagnostics.interprets,
                            0,
                            "appended={appended} fault={fault} acc={accumulator} config={config:?} budget={budget} histogram={:?} eip={:#x}",
                            native.histogram,
                            native.state.eip
                        );
                    }
                }
            }
        }
    }
}

// spec: A64A-FALL-2 A64F-FP-4 A64-REG-5
#[test]
fn tier1_mmx_region_loads_canonical_x87_aliases() {
    let mut program = fixture(|a| {
        let mut head = a.create_label();
        a.set_label(&mut head).unwrap();
        a.movd(mm0, dword_ptr(ebx)).unwrap();
        a.paddd(mm0, mm2).unwrap();
        a.movd(dword_ptr(ebx + 4), mm0).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(head).unwrap();
    });
    program.state.regs[1] = 10;
    program.state.x87.regs[2] =
        adv_cpu_core::X87Register::from_parts(0x8000_0000_0000_0001, 0x3fff);
    program.state.refresh_mmx();
    let config = (Features::default(), false, Checks::Adaptive);
    let mut actual = executor(&program, config, false);
    let mut reference = executor(&program, config, true);
    assert_eq!(
        actual.run(program.stop, 100),
        reference.run(program.stop, 100)
    );
    same(&actual, &reference);
    let native = actual.native.as_ref().unwrap();
    assert_eq!(native.diagnostics.interprets, 0);
    assert_eq!(native.diagnostics.helpers, 0);
}
