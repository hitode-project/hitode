use super::*;

fn instructions(p: &Program) -> impl Iterator<Item = iced_x86::Instruction> + '_ {
    iced_x86::Decoder::with_ip(32, &p.code, CODE.into(), iced_x86::DecoderOptions::NONE).into_iter()
}

fn pc(p: &Program, index: usize) -> u32 {
    instructions(p).nth(index).unwrap().ip32()
}

fn compare(native: &mut Executor, oracle: &mut Executor, p: &Program, budget: u64) -> RunOutcome {
    let outcome = native.run(p.stop, budget).unwrap();
    assert_eq!(outcome, oracle.run(p.stop, budget).unwrap());
    same(native, oracle);
    outcome
}

fn units(p: &Program, cpu: &Executor) -> usize {
    let engine = cpu.native.as_ref().unwrap();
    instructions(p)
        .filter(|i| engine.unit_address(i.ip32()).is_some())
        .count()
}

struct Spin;

impl adv_cpu_core::trap::ExecutionTrap for Spin {
    fn allow_spin_yield(&self) -> bool {
        true
    }

    fn call(&mut self, _: &mut CpuState, _: &mut Memory, vector: u32) -> Result<bool, String> {
        panic!("unexpected trap {vector}");
    }
}

// spec: A64-CTL-7, A64-CTL-15
#[test]
fn spin_backedges_publish_the_exact_taken_prefix() {
    for pause_only in [false, true] {
        let mut p = fixture(|a| {
            let mut top = a.create_label();
            a.set_label(&mut top).unwrap();
            if pause_only {
                a.pause().unwrap();
                a.jmp(top).unwrap();
            } else {
                a.mov(eax, dword_ptr(ebx + esi * 4 + 4)).unwrap();
                a.cmp(eax, edx).unwrap();
                a.jne(top).unwrap();
            }
        });
        p.state.regs[6] = 2;
        let prefix = if pause_only { 2 } else { 3 };
        for config in pixel_loops::configs().filter(|c| !c.1) {
            for budget in [prefix - 1, prefix, prefix + 20] {
                let mut native = executor(&p, config, false);
                let mut oracle = executor(&p, config, true);
                let expect = if budget < prefix {
                    RunOutcome::Budget
                } else {
                    RunOutcome::Reschedule
                };
                for cpu in [&mut native, &mut oracle] {
                    assert_eq!(
                        cpu.run_with_traps(p.stop, budget, &mut Spin, false)
                            .unwrap(),
                        expect
                    );
                }
                same(&native, &oracle);
                if budget >= prefix {
                    assert_eq!(
                        (native.state.eip, native.state.fuel),
                        (CODE, budget - prefix)
                    );
                }
                if budget > prefix {
                    assert_eq!(native.stats.native_entries, 1);
                    assert_eq!(native.stats.interpreter_entries, 0);
                }
            }
            if !pause_only {
                let offset = 12;
                p.data[offset..offset + 4].copy_from_slice(&p.state.regs[2].to_le_bytes());
                let mut native = executor(&p, config, false);
                let mut oracle = executor(&p, config, true);
                for cpu in [&mut native, &mut oracle] {
                    assert_eq!(
                        cpu.run_with_traps(p.stop, 20, &mut Spin, false).unwrap(),
                        RunOutcome::ReachedStop
                    );
                }
                same(&native, &oracle);
                p.data[offset..offset + 4].fill(0);
            }
        }
    }
}

// spec: A64-CTL-15
#[test]
fn counted_load_scans_do_not_request_a_handoff() {
    let p = fixture(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(ebx + ecx * 4)).unwrap();
        a.pause().unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
    });
    for config in pixel_loops::configs().filter(|c| !c.1) {
        let mut native = executor(&p, config, false);
        let mut oracle = executor(&p, config, true);
        for cpu in [&mut native, &mut oracle] {
            assert_eq!(
                cpu.run_with_traps(p.stop, 20, &mut Spin, false).unwrap(),
                RunOutcome::ReachedStop
            );
        }
        same(&native, &oracle);
    }
}

// spec: A64-TR-3
#[test]
fn loop_units_keep_alternating_arms_and_their_nested_paths() {
    let mut stop = 0;
    let mut p = fixture(|a| {
        let (mut top, mut alternate, mut nested, mut join) = (
            a.create_label(),
            a.create_label(),
            a.create_label(),
            a.create_label(),
        );
        a.set_label(&mut top).unwrap();
        a.paddb(xmm0, xmm1).unwrap();
        a.test(ecx, 1).unwrap();
        a.jne(alternate).unwrap();
        a.add(dword_ptr(ebx), 1).unwrap();
        a.nop().unwrap();
        a.set_label(&mut join).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        stop = a.instructions().len();
        a.nop().unwrap();
        a.set_label(&mut alternate).unwrap();
        a.test(ecx, 2).unwrap();
        a.jne(nested).unwrap();
        a.add(dword_ptr(ebx), 3).unwrap();
        a.jmp(join).unwrap();
        a.set_label(&mut nested).unwrap();
        a.add(dword_ptr(ebx), 5).unwrap();
        a.jmp(join).unwrap();
    });
    p.stop = pc(&p, stop);
    p.state.regs[1] = 1000;
    p.state.xmm[1] = [3; 16];
    for config in pixel_loops::configs() {
        for budget in [1, 5, 7, 8, 9, 10, 123, 6500, 10_000] {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            let outcome = compare(&mut native, &mut oracle, &p, budget);
            if budget == 10_000 && !config.1 {
                assert_eq!(outcome, RunOutcome::ReachedStop);
                assert_eq!(native.stats.native_entries, 1);
                assert_eq!(units(&p, &native), 1);
            }
        }
    }
}

// spec: A64-TR-3
#[test]
fn loop_units_inline_two_calls_in_a_long_nested_loop() {
    let mut stop = 0;
    let mut p = fixture(|a| {
        let (mut outer, mut inner, mut helper) =
            (a.create_label(), a.create_label(), a.create_label());
        a.mov(esi, 3).unwrap();
        a.set_label(&mut outer).unwrap();
        a.mov(ecx, 4).unwrap();
        a.set_label(&mut inner).unwrap();
        for _ in 0..86 {
            a.add(eax, edx).unwrap();
        }
        a.call(helper).unwrap();
        a.inc(edx).unwrap();
        a.call(helper).unwrap();
        for _ in 0..40 {
            a.add(eax, edx).unwrap();
        }
        a.dec(ecx).unwrap();
        a.jne(inner).unwrap();
        a.dec(esi).unwrap();
        a.jne(outer).unwrap();
        stop = a.instructions().len();
        a.nop().unwrap();
        a.set_label(&mut helper).unwrap();
        a.xor(eax, ecx).unwrap();
        a.inc(dword_ptr(ebx)).unwrap();
        a.ret().unwrap();
    });
    p.stop = pc(&p, stop);
    for config in pixel_loops::configs() {
        for budget in [1, 90, 93, 99, 139, 141, 280, 1000, 4000] {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            let outcome = compare(&mut native, &mut oracle, &p, budget);
            if budget == 4000 && !config.1 {
                assert_eq!(outcome, RunOutcome::ReachedStop);
                assert_eq!(units(&p, &native), 1);
            }
        }
    }
}

// spec: A64-TR-3
#[test]
fn loop_units_leave_a_nonreturning_arm_as_a_side_exit() {
    let mut side = 0;
    let mut p = fixture(|a| {
        let (mut top, mut ret) = (a.create_label(), a.create_label());
        a.push((CODE + 0x800) as i32).unwrap();
        a.set_label(&mut top).unwrap();
        a.cmp(ecx, 3).unwrap();
        a.je(ret).unwrap();
        a.add(eax, edx).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.jmp(u64::from(CODE + 0x800)).unwrap();
        a.set_label(&mut ret).unwrap();
        side = a.instructions().len();
        a.cpuid().unwrap();
        a.mov(dword_ptr(ebx), ecx).unwrap();
        a.ret().unwrap();
    });
    let side = pc(&p, side);
    p.stop = CODE + 0x800;
    p.state.regs[1] = 8;
    for config in pixel_loops::configs() {
        for budget in [1, 8, 22, 27, 29, 100] {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            compare(&mut native, &mut oracle, &p, budget);
            if budget == 100 && !config.1 {
                assert!(native.native.as_ref().unwrap().unit_address(side).is_some());
            }
        }
    }
}

// spec: A64-TR-3
#[test]
fn loop_units_recover_faults_and_budget_cuts_in_an_appended_arm() {
    let (mut stop, mut fault) = (0, 0);
    let mut p = fixture(|a| {
        let (mut top, mut alternate, mut join) =
            (a.create_label(), a.create_label(), a.create_label());
        a.set_label(&mut top).unwrap();
        a.test(ecx, 1).unwrap();
        a.jne(alternate).unwrap();
        for _ in 0..750 {
            a.add(dword_ptr(ebx), 1).unwrap();
        }
        a.set_label(&mut join).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        stop = a.instructions().len();
        a.nop().unwrap();
        a.set_label(&mut alternate).unwrap();
        a.add(dword_ptr(ebx), 7).unwrap();
        fault = a.instructions().len();
        a.mov(edx, dword_ptr(esi)).unwrap();
        a.add(dword_ptr(ebx), edx).unwrap();
        a.jmp(join).unwrap();
    });
    p.stop = pc(&p, stop);
    p.state.regs[6] = HOLE;
    let fault = pc(&p, fault);
    use crate::jit_map::{Part, enable_memory, units_of};
    enable_memory();
    for config in pixel_loops::configs() {
        for budget in [1, 753, 754, 756, 757, 758, 2000] {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            let outcome = compare(&mut native, &mut oracle, &p, budget);
            if budget == 757 {
                assert_eq!(outcome, RunOutcome::Budget);
                assert_eq!(native.state.eip, fault);
            }
            if budget == 2000 && !config.1 {
                assert_eq!(
                    outcome,
                    RunOutcome::ReadFault {
                        pc: fault,
                        address: HOLE
                    }
                );
                let engine = native.native.as_ref().unwrap();
                assert!(engine.unchecked_faults > 0 || engine.diagnostics.checked_faults > 0);
                assert_eq!(units(&p, &native), 1);
                if config.2 == Checks::All {
                    // The entry's countdown branch has to reach past this body.
                    let base = engine.unit_address(CODE).unwrap();
                    let (_, marks) = units_of(CODE)
                        .into_iter()
                        .rfind(|&(at, _)| at == base)
                        .unwrap();
                    assert!(marks.iter().find(|m| m.part == Part::Cold).unwrap().offset >= 32768);
                }
            }
        }
    }
}

// spec: A64-TR-3
#[test]
fn loop_units_end_before_a_long_straight_line_tail() {
    let mut tail = 0;
    let p = fixture(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(eax, edx).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        tail = a.instructions().len();
        for _ in 0..120 {
            a.inc(edi).unwrap();
        }
    });
    let tail = pc(&p, tail);
    for config in pixel_loops::configs() {
        for budget in [1, 5, 6, 7, 96, 103, 200] {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            compare(&mut native, &mut oracle, &p, budget);
            if budget == 200 && !config.1 {
                assert!(native.native.as_ref().unwrap().unit_address(tail).is_some());
            }
        }
    }
}

// spec: A64-TR-3
#[test]
fn rejected_wide_side_path_preserves_the_loop_exit() {
    let mut stop = 0;
    let mut p = fixture(|a| {
        let (mut top, mut wide) = (a.create_label(), a.create_label());
        a.set_label(&mut top).unwrap();
        a.test(edx, edx).unwrap();
        a.jz(wide).unwrap();
        // Leave one instruction of room in the weighted loop unit.
        for _ in 0..1019 {
            a.nop().unwrap();
        }
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        stop = a.instructions().len();
        a.nop().unwrap();

        a.set_label(&mut wide).unwrap();
        a.fld1().unwrap();
        a.fstp(qword_ptr(ebx)).unwrap();
        a.jmp(top).unwrap();
    });
    p.stop = pc(&p, stop);
    p.state.regs[1] = 1;
    p.state.regs[2] = 1;
    let config = (Features { flagm: false }, false, Checks::Adaptive);
    let mut native = executor(&p, config, false);
    let mut oracle = executor(&p, config, true);
    assert_eq!(
        compare(&mut native, &mut oracle, &p, 1500),
        RunOutcome::ReachedStop
    );
}
