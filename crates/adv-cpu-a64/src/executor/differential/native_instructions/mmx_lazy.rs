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
