//! Real ARM64 native execution against Tier 0 and the existing Cranelift JIT.
//! Inputs are authored with the repository's assembler; no game data is used.
#![cfg(all(
    target_arch = "aarch64",
    any(target_os = "linux", target_os = "android")
))]
use adv_cpu::{CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
#[path = "tier1/x87.rs"]
mod x87;
const CODE: u32 = 0x10000;
const STOP: u32 = 0x1f000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x3f000;
fn assemble(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}
fn machine(code: &[u8], mode: u8) -> Executor {
    let mut m = Executor::new(CODE, 0x30000).unwrap();
    m.protect(CODE, 0x30000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if mode == 1 {
        m.set_region(Region::new(CODE, code.len())).unwrap();
    }
    if mode == 2 {
        m.set_arm64_tier1(true).unwrap();
    }
    reset(&mut m, [0; 8], 2);
    m
}
fn reset(m: &mut Executor, mut regs: [u32; 8], flags: u32) {
    regs[4] = STACK;
    m.state = CpuState {
        regs,
        eip: CODE,
        eflags: flags,
        ..Default::default()
    };
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}
fn equivalent(a: &Executor, b: &Executor, mask: u32) {
    assert_eq!(a.state.regs, b.state.regs);
    assert_eq!(a.state.eip, b.state.eip);
    assert_eq!(a.state.eflags & mask, b.state.eflags & mask);
    assert_eq!(a.state.x87, b.state.x87);
    assert_eq!(a.state.xmm, b.state.xmm);
    assert_eq!(a.state.mxcsr, b.state.mxcsr);
    assert_eq!(a.state.fs_base, b.state.fs_base);
    assert_eq!(
        a.memory().read_data(DATA, 4096).unwrap(),
        b.memory().read_data(DATA, 4096).unwrap()
    );
}

fn string_instruction(a: &mut CodeAssembler, operation: u32, width: u32, repeat: u32) {
    let a = match repeat {
        1 => a.rep(),
        2 => a.repne(),
        _ => a,
    };
    match (operation, width) {
        (0, 1) => a.movsb(),
        (0, 2) => a.movsw(),
        (0, _) => a.movsd(),
        (1, 1) => a.stosb(),
        (1, 2) => a.stosw(),
        (1, _) => a.stosd(),
        (_, 1) => a.lodsb(),
        (_, 2) => a.lodsw(),
        (_, _) => a.lodsd(),
    }
    .unwrap();
}

#[test]
fn strings_match_all_widths_directions_repeats_and_empty_counts() {
    for operation in 0..3 {
        for width in [1, 2, 4] {
            for repeat in 0..3 {
                for use_fs in [false, true] {
                    let code = assemble(|a| {
                        string_instruction(a, operation, width, repeat);
                        if use_fs {
                            let mut instructions = a.take_instructions();
                            instructions[0].set_segment_prefix(iced_x86::Register::FS);
                            for i in instructions {
                                a.add_instruction(i).unwrap();
                            }
                        }
                        a.ret().unwrap();
                    });
                    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                    machines[2].set_tier_verification(Some(true));
                    for count in [0, 1, 7] {
                        for flags in [0x8d7, 0xcd7] {
                            for m in &mut machines {
                                reset(
                                    m,
                                    [0x9876abcd, count, 0, 0, 0, 0, DATA + 64, DATA + 256],
                                    flags,
                                );
                                m.state.fs_base = if use_fs { 128 } else { 0 };
                                let data: Vec<_> = (0..512).map(|n| (n * 13) as u8).collect();
                                m.write(DATA, &data).unwrap();
                                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                            }
                            equivalent(&machines[0], &machines[1], u32::MAX);
                            equivalent(&machines[0], &machines[2], u32::MAX);
                        }
                    }
                    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                    assert_eq!(machines[2].stats.verification.mismatches, 0);
                }
            }
        }
    }
}

#[test]
fn rep_elements_resume_exactly_after_budget_stop_and_fault() {
    for operation in 0..3 {
        for width in [1, 2, 4] {
            for reverse in [false, true] {
                let code = assemble(|a| {
                    string_instruction(a, operation, width, 1);
                    a.ret().unwrap();
                });
                let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                for m in &mut machines {
                    let edge = if reverse {
                        DATA + 4096 + width
                    } else {
                        DATA + 4096 - 2 * width
                    };
                    let (src, dst) = if operation == 2 {
                        (edge, DATA + 128)
                    } else {
                        (
                            if reverse {
                                DATA + 4096 + 128
                            } else {
                                DATA + 128
                            },
                            edge,
                        )
                    };
                    reset(
                        m,
                        [0x9876abcd, 4, 0, 0, 0, 0, src, dst],
                        0x8d7 | u32::from(reverse) * 0x400,
                    );
                    m.write(DATA, &[0x35; 8192]).unwrap();
                }
                // Only Tier 0/1 promise exact element budgets, unlike block-charged Cranelift.
                for index in [0, 2] {
                    let m = &mut machines[index];
                    assert_eq!(m.run(CODE, 1).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(m.state.regs[1], 4);
                    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
                    assert_eq!(m.state.regs[1], 3);
                    assert_eq!(m.state.eip, CODE);
                }
                equivalent(&machines[0], &machines[2], u32::MAX);
                for m in &mut machines {
                    m.protect(if reverse { DATA } else { DATA + 4096 }, 4096, 0)
                        .unwrap();
                }
                let outcomes: Vec<_> = machines
                    .iter_mut()
                    .map(|m| m.run(STOP, 100).unwrap())
                    .collect();
                assert_eq!(outcomes[0], outcomes[1]);
                assert_eq!(outcomes[0], outcomes[2]);
                assert!(matches!(
                    outcomes[0],
                    RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                ));
                for m in &mut machines {
                    m.protect(DATA, 8192, READ | WRITE).unwrap();
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
                assert_eq!(machines[2].state.regs[1], 2);
                for m in &mut machines {
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
            }
        }
    }
}

#[test]
fn rep_executable_writes_commit_each_element_once() {
    for operation in 0..2 {
        let code = assemble(|a| {
            string_instruction(a, operation, 4, 1);
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for (mode, m) in machines.iter_mut().enumerate() {
            reset(m, [0x12345678, 3, 0, 0, 0, 0, DATA + 64, DATA], 2);
            m.write(DATA + 64, &[0x37; 12]).unwrap();
            m.protect(DATA, 4096, READ | WRITE | EXEC).unwrap();
            if mode != 1 {
                assert_eq!(m.run(STOP, 2).unwrap(), RunOutcome::Budget);
                assert_eq!(m.state.eip, CODE);
                assert_eq!(m.state.regs[1], 1);
            }
        }
        equivalent(&machines[0], &machines[2], u32::MAX);
        assert_eq!(machines[2].tier1_stats().native_instructions, 2);
        for m in &mut machines {
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
    }
}
#[test]
fn integer_templates_match_both_existing_tiers_at_flag_boundaries() {
    for operation in 0..12 {
        let code = assemble(|a| {
            match operation {
                0 => a.add(eax, ecx),
                1 => a.sub(eax, ecx),
                2 => a.cmp(eax, ecx),
                3 => a.and(eax, ecx),
                4 => a.or(eax, ecx),
                5 => a.xor(eax, ecx),
                6 => a.test(eax, ecx),
                7 => a.inc(eax),
                8 => a.dec(eax),
                9 => a.neg(eax),
                10 => a.not(eax),
                _ => a.lea(eax, dword_ptr(ebx + ecx * 4 + 16)),
            }
            .unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        let values = [
            0, 1, 15, 16, 255, 256, 0x7fffffff, 0x80000000, 0xfffffffe, 0xffffffff,
        ];
        for left in values {
            for right in values {
                for m in &mut machines {
                    reset(m, [left, right, 0, 0xfffffff0, 0, 0, 0, 0], 0x8d7);
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                }
                let mask = if (3..=6).contains(&operation) {
                    0x8c5
                } else {
                    0x8d5
                };
                equivalent(&machines[0], &machines[1], mask);
                equivalent(&machines[0], &machines[2], mask);
            }
        }
        assert!(machines[2].tier1_stats().native_instructions >= 100);
        // RET to the stop address is native too.
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
    }
}
#[test]
fn direct_loop_checked_memory_and_single_instruction_fallback_continue() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.mov(ecx, 12).unwrap();
        a.mov(eax, 0x12345678u32).unwrap();
        a.set_label(&mut top).unwrap();
        a.add(eax, 17).unwrap();
        a.rcl(eax, 1).unwrap();
        a.xor(edx, eax).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.mov(dword_ptr(DATA), edx).unwrap();
        a.mov(ebx, dword_ptr(DATA)).unwrap();
        a.lea(esi, dword_ptr(ebx + eax * 2 + 7)).unwrap();
        a.ret().unwrap();
    });
    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
    for m in &mut machines {
        assert_eq!(m.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&machines[0], &machines[1], 0x8d5);
    equivalent(&machines[0], &machines[2], 0x8d5);
    let stats = machines[2].tier1_stats();
    assert_eq!(stats.fallback_instructions, 12, "RCL only");
    assert_eq!(stats.inline_accesses, 3, "store, load and RET");
    assert_eq!(stats.checked_accesses, 0);
    assert!(stats.direct_transfers >= 12);
    assert_eq!(stats.saves, stats.restores);
    println!(
        "tier1 compile_ns={} code_bytes={} direct_transfers={} saves={} restores={} fallback={}",
        stats.compile_time.as_nanos(),
        stats.code_bytes,
        stats.direct_transfers,
        stats.saves,
        stats.restores,
        stats.fallback_instructions
    );
}
#[test]
fn every_conditional_branch_observes_canonical_flags() {
    for condition in 0..16 {
        let code = assemble(|a| {
            let mut yes = a.create_label();
            let mut end = a.create_label();
            a.cmp(eax, ecx).unwrap();
            match condition {
                0 => a.jo(yes),
                1 => a.jno(yes),
                2 => a.jb(yes),
                3 => a.jae(yes),
                4 => a.je(yes),
                5 => a.jne(yes),
                6 => a.jbe(yes),
                7 => a.ja(yes),
                8 => a.js(yes),
                9 => a.jns(yes),
                10 => a.jp(yes),
                11 => a.jnp(yes),
                12 => a.jl(yes),
                13 => a.jge(yes),
                14 => a.jle(yes),
                _ => a.jg(yes),
            }
            .unwrap();
            a.mov(ebx, 3).unwrap();
            a.jmp(end).unwrap();
            a.set_label(&mut yes).unwrap();
            a.mov(ebx, 7).unwrap();
            a.set_label(&mut end).unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for (a, b) in [
            (0, 0),
            (0, 1),
            (u32::MAX, 0),
            (0x80000000, 1),
            (0x7fffffff, u32::MAX),
        ] {
            for m in &mut machines {
                reset(m, [a, b, 0, 0, 0, 0, 0, 0], 2);
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            }
            equivalent(&machines[0], &machines[1], 0x8d5);
            equivalent(&machines[0], &machines[2], 0x8d5);
        }
    }
}
#[test]
fn faults_keep_committed_prefix_and_retry_only_the_faulting_access() {
    for write in [false, true] {
        let code = assemble(|a| {
            a.inc(ecx).unwrap();
            a.mov(dword_ptr(DATA), ecx).unwrap();
            a.add(eax, 7).unwrap();
            if write {
                a.mov(dword_ptr(DATA + 4094), eax).unwrap();
            } else {
                a.mov(ebx, dword_ptr(DATA + 4094)).unwrap();
            }
            a.ret().unwrap();
        });
        let mut reference = machine(&code, 0);
        let mut tier = machine(&code, 2);
        for m in [&mut reference, &mut tier] {
            m.protect(DATA + 4096, 4096, 0).unwrap();
        }
        let a = reference.run(STOP, 100).unwrap();
        let b = tier.run(STOP, 100).unwrap();
        assert_eq!(a, b);
        assert!(matches!(
            a,
            RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
        ));
        equivalent(&reference, &tier, 0x8d5);
        assert_eq!(tier.state.regs[1], 1);
        // The committed store stayed inline; only the refused access left.
        assert_eq!(tier.tier1_stats().inline_accesses, 1);
        assert_eq!(tier.tier1_stats().checked_accesses, 1);
        for m in [&mut reference, &mut tier] {
            m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&reference, &tier, 0x8d5);
        assert_eq!(tier.state.regs[1], 1);
    }
}
#[test]
fn budget_stop_and_self_modification_keep_native_entries_current() {
    let code = assemble(|a| {
        a.inc(eax).unwrap();
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    let mut reference = machine(&code, 0);
    let mut tier = machine(&code, 2);
    assert_eq!(tier.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(tier.state.regs[0], 1);
    assert_eq!(tier.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(tier.state.regs[0], 2);
    assert_eq!(tier.run(STOP, 1).unwrap(), RunOutcome::ReachedStop);
    let replacement = assemble(|a| {
        a.dec(eax).unwrap();
        a.dec(eax).unwrap();
        a.ret().unwrap();
    });
    for m in [&mut reference, &mut tier] {
        m.write(CODE, &replacement).unwrap();
        reset(m, [10, 0, 0, 0, 0, 0, 0, 0], 2);
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&reference, &tier, 0x8d5);
    assert_eq!(tier.state.regs[0], 8);
    assert!(tier.tier1_stats().invalidations > 0);
}

#[test]
fn checked_effective_addresses_wrap_and_reads_require_guest_permission() {
    let code = assemble(|a| {
        a.lea(eax, dword_ptr(ebx + ecx * 4 + 16)).unwrap();
        a.mov(edx, dword_ptr(ebx + ecx * 4 + 16)).unwrap();
        a.ret().unwrap();
    });
    let mut reference = machine(&code, 0);
    let mut tier = machine(&code, 2);
    for m in [&mut reference, &mut tier] {
        reset(m, [0, DATA / 4, 0, 0xfffffff0, 0, 0, 0, 0], 2);
        m.write(DATA, &123u32.to_le_bytes()).unwrap();
        m.protect(DATA, 4096, WRITE).unwrap();
    }
    assert_eq!(
        reference.run(STOP, 100).unwrap(),
        tier.run(STOP, 100).unwrap()
    );
    assert_eq!(tier.state.regs[0], DATA);
    assert_eq!(tier.state.regs[2], 0);
    for m in [&mut reference, &mut tier] {
        m.protect(DATA, 4096, READ | WRITE).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&reference, &tier, 0x8d5);
    assert_eq!(tier.state.regs[2], 123);
}

#[test]
fn single_step_and_rep_return_to_native_with_exact_continuations() {
    let code = assemble(|a| {
        a.inc(eax).unwrap();
        a.rep().movsb().unwrap();
        a.add(eax, 7).unwrap();
        a.ret().unwrap();
    });
    let mut reference = machine(&code, 0);
    let mut tier = machine(&code, 2);
    for m in [&mut reference, &mut tier] {
        reset(m, [0, 3, 0, 0, 0, 0, DATA, DATA + 32], 0x102);
        m.write(DATA, &[7, 8, 9]).unwrap();
    }
    assert_eq!(
        reference.run(STOP, 100).unwrap(),
        tier.run(STOP, 100).unwrap()
    );
    assert_eq!(tier.state.regs[0], 1);
    assert_eq!(tier.tier1_stats().native_entries, 0);
    for m in [&mut reference, &mut tier] {
        m.state.eflags &= !0x100;
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&reference, &tier, 0x8d5);
    assert_eq!(tier.memory().read(DATA + 32, 3).unwrap(), &[7, 8, 9]);
    assert!(tier.tier1_stats().native_instructions > 0);
}

#[test]
fn shared_executable_backing_stays_on_single_instruction_fallback() {
    use adv_cpu::backend::{Guest, Prot};
    let code = assemble(|a| {
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    let mut cpu = adv_cpu::JitCpu::new(STOP, false).unwrap();
    let _alias = cpu.map_shared_audio(CODE, 4096).unwrap();
    cpu.write(CODE, &code).unwrap();
    cpu.protect(CODE, 4096, Prot::RX).unwrap();
    cpu.map(STOP, 4096, Prot::RX).unwrap();
    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
    let m = cpu.executor_mut();
    m.set_arm64_tier1(true).unwrap();
    reset(m, [0; 8], 2);
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.tier1_stats().native_entries, 0);
    assert_eq!(m.tier1_stats().fallback_instructions, 2);
}

#[test]
fn guest_code_write_invalidates_the_next_native_entry() {
    let mut body = CodeAssembler::new(32).unwrap();
    body.mov(eax, 1).unwrap();
    body.ret().unwrap();
    let body = body.assemble(u64::from(CODE + 128)).unwrap();
    let prefix = assemble(|a| {
        a.inc(edx).unwrap();
        a.mov(dword_ptr(CODE + 129), 7u32).unwrap();
        a.jmp(u64::from(CODE + 128)).unwrap();
    });
    let mut reference = machine(&prefix, 0);
    let mut tier = machine(&prefix, 2);
    for m in [&mut reference, &mut tier] {
        m.write(CODE + 128, &body).unwrap();
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&reference, &tier, 0x8d5);
    assert_eq!(tier.state.regs[0], 7);
    assert_eq!(tier.state.regs[2], 1);
    assert!(tier.tier1_stats().invalidations > 0);
}
#[test]
fn tier_verification_counts_tier1_steps_through_loops_memory_and_fallbacks() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.mov(ecx, 12).unwrap();
        a.mov(eax, 0x12345678u32).unwrap();
        a.set_label(&mut top).unwrap();
        a.add(eax, 17).unwrap();
        a.rcl(eax, 1).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.mov(ebx, dword_ptr(edi)).unwrap();
        a.xor(edx, ebx).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.ret().unwrap();
    });
    let mut results = Vec::new();
    for verify in [false, true] {
        let mut m = machine(&code, 2);
        m.set_tier_verification(verify.then_some(true));
        reset(&mut m, [0, 0, 0, 0, 0, 0, 0, DATA], 2);
        assert_eq!(m.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
        let checks = &m.stats.verification;
        assert_eq!(checks.mismatches, 0, "{checks:?}");
        assert_eq!(checks.verified > 0, verify, "{checks:?}");
        assert!(m.tier1_stats().native_instructions > 0);
        results.push((
            m.state.regs,
            m.state.eflags & 0x8d5,
            m.memory().read(DATA, 4).unwrap().to_vec(),
        ));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn compile_latency_is_reported_for_both_native_tiers() {
    // Diagnostic only: host compile cost for the same straight-line unit.
    let code = assemble(|a| {
        for n in 0..30u32 {
            a.add(eax, ecx).unwrap();
            a.xor(edx, eax).unwrap();
            a.mov(dword_ptr(DATA + n * 4), edx).unwrap();
            a.mov(ebx, dword_ptr(DATA + n * 4)).unwrap();
        }
        a.ret().unwrap();
    });
    let mut tiers = [machine(&code, 1), machine(&code, 2)];
    for m in &mut tiers {
        reset(m, [3, 5, 0, 0, 0, 0, 0, 0], 2);
        assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&tiers[0], &tiers[1], 0x8d5);
    let tier1 = tiers[1].tier1_stats();
    assert_eq!(tier1.compilations, 1);
    assert_eq!(tier1.inline_accesses, 61);
    println!(
        "compile latency (121 guest instructions): cranelift_ns={} tier1_ns={} tier1_code_bytes={}",
        tiers[0].stats.compile_time.as_nanos(),
        tier1.compile_time.as_nanos(),
        tier1.code_bytes
    );
}

#[test]
fn calls_stack_and_memory_operands_match_both_existing_tiers() {
    let code = assemble(|a| {
        let mut leaf = a.create_label();
        let mut top = a.create_label();
        a.mov(ecx, 6).unwrap();
        a.set_label(&mut top).unwrap();
        a.push(ecx).unwrap();
        a.push(0x80u32).unwrap();
        a.call(leaf).unwrap();
        a.add(edx, eax).unwrap();
        a.dec(ecx).unwrap();
        a.jne(top).unwrap();
        a.push(esp).unwrap();
        a.pop(ebp).unwrap();
        a.ret().unwrap();
        // leaf(flag, n): sums sign- and zero-extended bytes, releases its args.
        a.set_label(&mut leaf).unwrap();
        a.push(ebx).unwrap();
        a.mov(ebx, dword_ptr(esp + 12)).unwrap();
        a.movzx(eax, byte_ptr(ebx * 4 + DATA)).unwrap();
        a.movsx(esi, word_ptr(ebx * 2 + DATA + 64)).unwrap();
        a.add(eax, esi).unwrap();
        a.add(eax, dword_ptr(esp + 8)).unwrap();
        a.xor(eax, dword_ptr(DATA + 128)).unwrap();
        a.cmp(dword_ptr(esp + 12), 3).unwrap();
        a.test(dword_ptr(DATA + 128), edi).unwrap();
        a.pop(ebx).unwrap();
        a.ret_1(8).unwrap();
    });
    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
    let data: Vec<u8> = (0..160u32).map(|n| (n * 37 + 11) as u8).collect();
    for m in &mut machines {
        m.write(DATA, &data).unwrap();
        reset(m, [0, 0, 0, 0, 0, 0, 0, 0x5a5a], 2);
        assert_eq!(m.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&machines[0], &machines[1], 0x8d5);
    equivalent(&machines[0], &machines[2], 0x8d5);
    assert_eq!(
        machines[2].memory().read_data(STACK - 64, 64).unwrap(),
        machines[0].memory().read_data(STACK - 64, 64).unwrap()
    );
    let stats = machines[2].tier1_stats();
    assert_eq!(stats.fallback_instructions, 0);
    assert!(stats.inline_accesses >= 6 * 10, "{stats:?}");
}

#[test]
fn refused_stack_accesses_rerun_exactly_in_tier0() {
    let code = assemble(|a| {
        a.inc(eax).unwrap();
        a.push(eax).unwrap();
        a.inc(eax).unwrap();
        a.call(u64::from(CODE + 64)).unwrap();
    });
    let mut reference = machine(&code, 0);
    let mut tier = machine(&code, 2);
    for m in [&mut reference, &mut tier] {
        m.protect(STACK - 4096, 4096, READ).unwrap();
        reset(m, [0; 8], 2);
    }
    let outcome = reference.run(STOP, 100).unwrap();
    assert!(
        matches!(outcome, RunOutcome::WriteFault { .. }),
        "{outcome:?}"
    );
    assert_eq!(tier.run(STOP, 100).unwrap(), outcome);
    equivalent(&reference, &tier, 0x8d5);
    assert_eq!(
        tier.state.regs[4], STACK,
        "ESP is unchanged by the refused push"
    );
    // INC ran natively; the refused PUSH retired nothing in either tier.
    let stats = tier.tier1_stats();
    assert_eq!(stats.native_instructions, 1, "{stats:?}");
    assert_eq!(stats.inline_accesses, 0, "{stats:?}");
}

fn rmw_instruction(a: &mut CodeAssembler, width: u32, operation: u32, source: u32) {
    macro_rules! emit {
        ($mem:expr, $reg:expr) => {{
            let mem = $mem;
            match operation {
                0 => {
                    if source >= 2 {
                        a.add(mem, if source == 2 { -1 } else { 0x81 })
                    } else {
                        a.add(mem, $reg)
                    }
                }
                1 => {
                    if source >= 2 {
                        a.sub(mem, if source == 2 { -1 } else { 0x81 })
                    } else {
                        a.sub(mem, $reg)
                    }
                }
                2 => {
                    if source >= 2 {
                        a.and(mem, if source == 2 { -1 } else { 0x81 })
                    } else {
                        a.and(mem, $reg)
                    }
                }
                3 => {
                    if source >= 2 {
                        a.or(mem, if source == 2 { -1 } else { 0x81 })
                    } else {
                        a.or(mem, $reg)
                    }
                }
                4 => {
                    if source >= 2 {
                        a.xor(mem, if source == 2 { -1 } else { 0x81 })
                    } else {
                        a.xor(mem, $reg)
                    }
                }
                5 => a.inc(mem),
                6 => a.dec(mem),
                7 => a.not(mem),
                8 => a.neg(mem),
                9 => {
                    if source >= 2 {
                        a.adc(mem, if source == 2 { -1 } else { 0x81 })
                    } else {
                        a.adc(mem, $reg)
                    }
                }
                10 => {
                    if source >= 2 {
                        a.sbb(mem, if source == 2 { -1 } else { 0x81 })
                    } else {
                        a.sbb(mem, $reg)
                    }
                }
                _ => unreachable!(),
            }
            .unwrap();
        }};
    }
    match width {
        1 if source == 1 => emit!(byte_ptr(edi), ch),
        1 => emit!(byte_ptr(edi), cl),
        2 => emit!(word_ptr(edi), cx),
        4 => emit!(dword_ptr(edi), ecx),
        _ => unreachable!(),
    }
}

#[test]
fn memory_rmw_matches_both_tiers_at_each_width_and_flag_boundary() {
    for width in [1, 2, 4] {
        let sign = 1u32 << (width * 8 - 1);
        let mask = u32::MAX >> (32 - width * 8);
        for operation in 0..11 {
            for source in 0..4 {
                let code = assemble(|a| {
                    rmw_instruction(a, width, operation, source);
                    a.ret().unwrap();
                });
                let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                for left in [0, 1, 15, 16, sign - 1, sign, mask - 1, mask] {
                    for right in [0, 1, sign, mask] {
                        for flags in [2, 0xcd7] {
                            for m in &mut machines {
                                // Distinct surrounding bytes detect accidental wide stores.
                                m.write(DATA, &[0xa5; 12]).unwrap();
                                m.write(DATA + 3, &left.to_le_bytes()[..width as usize])
                                    .unwrap();
                                let source_value = if width == 1 && source == 1 {
                                    0xabcd_00a5 | right << 8
                                } else {
                                    right | !mask
                                };
                                reset(m, [0, source_value, 7, 9, 0, 11, 13, DATA + 3], flags);
                                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                            }
                            let flags_mask = if (2..=4).contains(&operation) {
                                !0x10
                            } else {
                                u32::MAX
                            };
                            equivalent(&machines[0], &machines[1], flags_mask);
                            equivalent(&machines[0], &machines[2], u32::MAX);
                            assert_eq!(machines[2].stats.verification.mismatches, 0);
                        }
                    }
                }
                let stats = machines[2].tier1_stats();
                assert_eq!(
                    stats.fallback_instructions, 0,
                    "{width}/{operation}/{source}: {stats:?}"
                );
                assert_eq!(stats.native_instructions, 128);
                assert_eq!(stats.inline_accesses, 192);
            }
        }
    }
}

#[test]
fn memory_rmw_refusals_preserve_state_and_retry_only_the_faulting_instruction() {
    for width in [1, 2, 4] {
        for operation in 0..11 {
            for write in [false, true] {
                let code = assemble(|a| {
                    a.inc(esi).unwrap();
                    rmw_instruction(a, width, operation, 0);
                    a.ret().unwrap();
                });
                let address = DATA + 4096 - width / 2;
                let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                for m in &mut machines {
                    reset(m, [7, 1, 9, 11, 0, 13, 0, address], 0xcd7);
                    m.write(address, &0x7fff_ffffu32.to_le_bytes()).unwrap();
                    m.protect(DATA + 4096, 4096, if write { READ } else { 0 })
                        .unwrap();
                }
                let outcome = machines[0].run(STOP, 100).unwrap();
                assert!(
                    if write {
                        matches!(outcome, RunOutcome::WriteFault { .. })
                    } else {
                        matches!(outcome, RunOutcome::ReadFault { .. })
                    },
                    "{outcome:?}"
                );
                for m in &mut machines[1..] {
                    assert_eq!(m.run(STOP, 100).unwrap(), outcome);
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
                let stats = machines[2].tier1_stats();
                assert_eq!(stats.native_instructions, 1, "{stats:?}");
                assert_eq!(stats.inline_accesses, u64::from(write), "{stats:?}");
                assert_eq!(stats.checked_accesses, 0, "new forms must use FALLBACK");
                for m in &mut machines {
                    m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
                    assert_eq!(
                        m.memory().read_data(address, 4).unwrap().as_ref(),
                        &0x7fff_ffffu32.to_le_bytes()
                    );
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(m.state.regs[6], 1, "committed prefix must not repeat");
                }
                equivalent(&machines[0], &machines[1], 0x8c5);
                equivalent(&machines[0], &machines[2], u32::MAX);
                for m in &machines[1..] {
                    assert_eq!(
                        machines[0].memory().read_data(address, 4).unwrap(),
                        m.memory().read_data(address, 4).unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn memory_rmw_executable_store_commits_once_and_charges_one_unit() {
    for width in [1, 2, 4] {
        for operation in 0..11 {
            let code = assemble(|a| {
                rmw_instruction(a, width, operation, 2);
                a.mov(esi, 1).unwrap();
                a.ret().unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            for (mode, m) in machines.iter_mut().enumerate() {
                reset(m, [0, 1, 0, 0, 0, 0, 0, DATA], 0xcd7);
                m.write(DATA, &0x7fff_ffffu32.to_le_bytes()).unwrap();
                m.protect(DATA, 4096, READ | WRITE | EXEC).unwrap();
                // Cranelift charges basic blocks; only Tier 0 and Tier 1
                // share the single-instruction budget boundary.
                if mode == 1 {
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                } else {
                    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
                    assert_eq!(m.state.regs[6], 0);
                }
            }
            equivalent(&machines[0], &machines[2], u32::MAX);
            let stats = machines[2].tier1_stats();
            assert_eq!(stats.native_instructions, 1, "{stats:?}");
            assert_eq!(stats.inline_accesses, 2, "{stats:?}");
            assert_eq!(stats.fallback_instructions, 0, "{stats:?}");
            for m in &mut machines {
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(m.state.regs[6], 1);
            }
            equivalent(&machines[0], &machines[1], !0x10);
            equivalent(&machines[0], &machines[2], u32::MAX);
        }
    }
}

#[test]
fn memory_rmw_invalidates_modified_native_successor() {
    for width in [1, 2, 4] {
        let code = assemble(|a| {
            match width {
                1 => a.add(byte_ptr(CODE + 129), 1),
                2 => a.add(word_ptr(CODE + 129), 1),
                _ => a.add(dword_ptr(CODE + 129), 1),
            }
            .unwrap();
            a.jmp(u64::from(CODE + 128)).unwrap();
        });
        let mut body = CodeAssembler::new(32).unwrap();
        body.mov(eax, 127).unwrap();
        body.ret().unwrap();
        let body = body.assemble(u64::from(CODE + 128)).unwrap();
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for m in &mut machines {
            m.write(CODE + 128, &body).unwrap();
            m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.state.regs[0], 128, "execute the modified immediate");
        }
        equivalent(&machines[0], &machines[1], 0x8d5);
        equivalent(&machines[0], &machines[2], u32::MAX);
        let stats = machines[2].tier1_stats();
        assert!(stats.invalidations > 0, "{stats:?}");
        assert_eq!(stats.native_instructions, 4);
        assert_eq!(stats.fallback_instructions, 0);
    }
}

fn indirect_instruction(a: &mut CodeAssembler, call: bool, memory: bool) {
    match (call, memory) {
        (false, false) => a.jmp(eax),
        (false, true) => a.jmp(dword_ptr(edi)),
        (true, false) => a.call(eax),
        (true, true) => a.call(dword_ptr(edi)),
    }
    .unwrap();
}

#[test]
fn indirect_branches_dispatch_runtime_targets_and_preserve_flags() {
    for call in [false, true] {
        for memory in [false, true] {
            let code = assemble(|a| {
                indirect_instruction(a, call, memory);
                a.inc(ebx).unwrap();
                a.ret().unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            machines[2].set_tier_verification(Some(true));
            for target in [CODE + 128, CODE + 192] {
                let mut body = CodeAssembler::new(32).unwrap();
                body.inc(edx).unwrap();
                body.ret().unwrap();
                let body = body.assemble(u64::from(target)).unwrap();
                for m in &mut machines {
                    m.write(target, &body).unwrap();
                    m.write(DATA, &target.to_le_bytes()).unwrap();
                    reset(m, [target, 3, 5, 7, 0, 11, 13, DATA], 0xcd7);
                    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
                    assert_eq!(m.state.eip, target);
                    assert_eq!(m.state.eflags, 0xcd7);
                    assert_eq!(m.state.regs[4], STACK - if call { 4 } else { 0 });
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
                for m in &mut machines {
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(m.state.regs[2], 6);
                    assert_eq!(m.state.regs[3], if call { 8 } else { 7 });
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
                for m in &machines[1..] {
                    assert_eq!(
                        machines[0].memory().read_data(STACK - 4, 8).unwrap(),
                        m.memory().read_data(STACK - 4, 8).unwrap()
                    );
                }
            }
            let stats = machines[2].tier1_stats();
            assert_eq!(stats.fallback_instructions, 0, "{stats:?}");
            assert_eq!(stats.native_instructions, if call { 10 } else { 6 });
            assert_eq!(stats.direct_transfers, 0, "indirect branches dispatch");
            assert!(machines[2].stats.verification.verified > 0);
            assert_eq!(machines[2].stats.verification.mismatches, 0);
        }
    }
}

#[test]
fn indirect_memory_target_refusal_does_not_push_or_retire() {
    for call in [false, true] {
        let code = assemble(|a| {
            a.inc(esi).unwrap();
            indirect_instruction(a, call, true);
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for m in &mut machines {
            reset(m, [0, 3, 5, 7, 0, 11, 0, DATA + 4094], 0xcd7);
            m.write(DATA + 4094, &STOP.to_le_bytes()).unwrap();
            m.write(STACK - 4, &0xa5a5_a5a5u32.to_le_bytes()).unwrap();
            m.protect(DATA + 4096, 4096, 0).unwrap();
        }
        let outcome = machines[0].run(STOP, 100).unwrap();
        assert!(matches!(outcome, RunOutcome::ReadFault { .. }));
        for m in &mut machines[1..] {
            assert_eq!(m.run(STOP, 100).unwrap(), outcome);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        let stats = machines[2].tier1_stats();
        assert_eq!(stats.native_instructions, 1);
        assert_eq!(stats.inline_accesses, 0);
        assert_eq!(stats.checked_accesses, 0);
        for m in &mut machines {
            assert_eq!(m.state.regs[4], STACK);
            assert_eq!(
                m.memory().read_data(STACK - 4, 4).unwrap().as_ref(),
                &0xa5a5_a5a5u32.to_le_bytes()
            );
            m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.state.regs[6], 1);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
    }
}

#[test]
fn indirect_call_push_refusal_preserves_target_stack_and_flags() {
    for memory in [false, true] {
        let code = assemble(|a| {
            a.inc(esi).unwrap();
            indirect_instruction(a, true, memory);
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for m in &mut machines {
            reset(m, [STOP, 3, 5, 7, 0, 11, 0, DATA], 0xcd7);
            m.state.regs[4] = STACK + 2;
            m.write(DATA, &STOP.to_le_bytes()).unwrap();
            m.write(STACK - 2, &0xa5a5_a5a5u32.to_le_bytes()).unwrap();
            m.protect(STACK, 4096, READ).unwrap();
        }
        let outcome = machines[0].run(STOP, 100).unwrap();
        assert!(matches!(outcome, RunOutcome::WriteFault { .. }));
        for m in &mut machines[1..] {
            assert_eq!(m.run(STOP, 100).unwrap(), outcome);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        let stats = machines[2].tier1_stats();
        assert_eq!(stats.native_instructions, 1);
        assert_eq!(stats.inline_accesses, u64::from(memory));
        assert_eq!(stats.checked_accesses, 0);
        for m in &mut machines {
            assert_eq!(m.state.regs[4], STACK + 2);
            assert_eq!(
                m.memory().read_data(STACK - 2, 4).unwrap().as_ref(),
                &0xa5a5_a5a5u32.to_le_bytes()
            );
            m.protect(STACK, 4096, READ | WRITE).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.state.regs[4], STACK - 2);
            assert_eq!(m.state.regs[6], 1);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
    }
}

#[test]
fn indirect_call_resolves_esp_alias_before_executable_stack_write() {
    for memory in [false, true] {
        let code = assemble(|a| {
            if memory {
                a.call(dword_ptr(esp - 4))
            } else {
                a.call(esp)
            }
            .unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        for m in &mut machines {
            reset(m, [1, 3, 5, 7, 0, 11, 13, DATA], 0xcd7);
            m.write(STACK - 4, &STACK.to_le_bytes()).unwrap();
            m.protect(STACK - 4096, 8192, READ | WRITE | EXEC).unwrap();
            assert_eq!(m.run(STACK, 1).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.state.regs[4], STACK - 4);
            assert_eq!(m.state.eflags, 0xcd7);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        for m in &machines[1..] {
            assert_eq!(
                machines[0].memory().read_data(STACK - 4, 4).unwrap(),
                m.memory().read_data(STACK - 4, 4).unwrap()
            );
        }
        let stats = machines[2].tier1_stats();
        assert_eq!(stats.native_instructions, 1);
        assert_eq!(stats.inline_accesses, 1 + u64::from(memory));
        assert_eq!(stats.fallback_instructions, 0);
        assert_eq!(machines[2].stats.verification.mismatches, 0);
    }
}

#[test]
fn carry_register_and_memory_sources_match_full_flags() {
    for subtract in [false, true] {
        for source in 0..5 {
            let code = assemble(|a| {
                match (subtract, source) {
                    (false, 0) => a.adc(eax, ecx),
                    (true, 0) => a.sbb(eax, ecx),
                    (false, 1) => a.adc(eax, -1),
                    (true, 1) => a.sbb(eax, -1),
                    (false, 2) => a.adc(eax, 0x7fffffff),
                    (true, 2) => a.sbb(eax, 0x7fffffff),
                    (false, 3) => a.adc(eax, dword_ptr(edi)),
                    (true, 3) => a.sbb(eax, dword_ptr(edi)),
                    (false, _) => a.adc(eax, eax),
                    (true, _) => a.sbb(eax, eax),
                }
                .unwrap();
                a.ret().unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            machines[2].set_tier_verification(Some(true));
            let values = [0, 1, 15, 16, 0x7fffffff, 0x80000000, 0xfffffffe, u32::MAX];
            for left in values {
                for right in values {
                    for flags in [2, 3, 0xcd6, 0xcd7] {
                        for m in &mut machines {
                            reset(m, [left, right, 5, 7, 0, 11, 13, DATA], flags);
                            m.write(DATA, &right.to_le_bytes()).unwrap();
                            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                        }
                        equivalent(&machines[0], &machines[1], u32::MAX);
                        equivalent(&machines[0], &machines[2], u32::MAX);
                    }
                }
            }
            assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
            assert_eq!(machines[2].stats.verification.mismatches, 0);
        }
    }
}
fn shift_instruction(
    a: &mut CodeAssembler,
    operation: u32,
    width: u32,
    form: u32,
    immediate: Option<u32>,
) {
    macro_rules! emit {
        ($dst:expr, $count:expr) => {
            match operation {
                0 => a.shl($dst, $count),
                1 => a.shr($dst, $count),
                2 => a.sar($dst, $count),
                3 => a.rol($dst, $count),
                _ => a.ror($dst, $count),
            }
            .unwrap()
        };
    }
    macro_rules! count {
        ($dst:expr) => {
            if let Some(n) = immediate {
                emit!($dst, n);
            } else {
                emit!($dst, cl);
            }
        };
    }
    match (width, form) {
        (1, 0) => count!(al),
        (2, 0) => count!(ax),
        (4, 0) => count!(eax),
        (1, 1) => count!(byte_ptr(edi)),
        (2, 1) => count!(word_ptr(edi)),
        (4, 1) => count!(dword_ptr(edi)),
        (1, 2) => count!(ah),
        (1, _) => count!(cl),
        (2, _) => count!(cx),
        (4, _) => count!(ecx),
        _ => unreachable!(),
    }
}

#[test]
fn shifts_and_rotates_match_all_flags_and_masked_counts() {
    for operation in 0..5 {
        for width in [1, 2, 4] {
            for form in 0..4 {
                if form == 2 && width != 1 {
                    continue;
                }
                for immediate in [false, true] {
                    let counts = [0, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 255];
                    // One code unit per count only for the immediate form.
                    for encoded in if immediate { counts.to_vec() } else { vec![0] } {
                        let code = assemble(|a| {
                            shift_instruction(
                                a,
                                operation,
                                width,
                                form,
                                immediate.then_some(encoded),
                            );
                            a.ret().unwrap();
                        });
                        let mut machines =
                            [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                        machines[2].set_tier_verification(Some(true));
                        let sign = 1u32 << (width * 8 - 1);
                        for value in [0, 1, sign - 1, sign, sign + 1, u32::MAX] {
                            for count in if immediate {
                                vec![encoded]
                            } else {
                                counts.to_vec()
                            } {
                                for flags in [2, 0xcd7] {
                                    for m in &mut machines {
                                        reset(
                                            m,
                                            [
                                                value,
                                                (value & !255) | count,
                                                5,
                                                7,
                                                0,
                                                11,
                                                13,
                                                DATA + 3,
                                            ],
                                            flags,
                                        );
                                        m.write(DATA, &[0xa5; 12]).unwrap();
                                        m.write(DATA + 3, &value.to_le_bytes()[..width as usize])
                                            .unwrap();
                                        assert_eq!(
                                            m.run(STOP, 100).unwrap(),
                                            RunOutcome::ReachedStop
                                        );
                                    }
                                    equivalent(&machines[0], &machines[1], u32::MAX);
                                    equivalent(&machines[0], &machines[2], u32::MAX);
                                }
                            }
                        }
                        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                        assert_eq!(machines[2].stats.verification.mismatches, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn memory_shifts_check_zero_count_permissions_and_preserve_refused_state() {
    for operation in 0..5 {
        for width in [1, 2, 4] {
            for count in [0, 1, 32, 255] {
                for write in [false, true] {
                    let code = assemble(|a| {
                        a.inc(esi).unwrap();
                        shift_instruction(a, operation, width, 1, None);
                        a.ret().unwrap();
                    });
                    let address = DATA + 4096 - width / 2;
                    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                    for m in &mut machines {
                        reset(m, [7, count, 9, 11, 0, 13, 0, address], 0xcd7);
                        m.write(address, &0x80008080u32.to_le_bytes()).unwrap();
                        m.protect(DATA + 4096, 4096, if write { READ } else { 0 })
                            .unwrap();
                    }
                    let outcome = machines[0].run(STOP, 100).unwrap();
                    assert!(if write {
                        matches!(outcome, RunOutcome::WriteFault { .. })
                    } else {
                        matches!(outcome, RunOutcome::ReadFault { .. })
                    });
                    for m in &mut machines[1..] {
                        assert_eq!(m.run(STOP, 100).unwrap(), outcome);
                    }
                    equivalent(&machines[0], &machines[1], u32::MAX);
                    equivalent(&machines[0], &machines[2], u32::MAX);
                    assert_eq!(machines[2].tier1_stats().native_instructions, 1);
                    for m in &mut machines {
                        m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
                        assert_eq!(
                            m.memory().read_data(address, 4).unwrap().as_ref(),
                            &0x80008080u32.to_le_bytes()
                        );
                        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                        assert_eq!(m.state.regs[6], 1);
                    }
                    equivalent(&machines[0], &machines[1], u32::MAX);
                    equivalent(&machines[0], &machines[2], u32::MAX);
                    for m in &machines[1..] {
                        assert_eq!(
                            machines[0].memory().read_data(address, 4).unwrap(),
                            m.memory().read_data(address, 4).unwrap()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn zero_memory_shift_does_not_write_executable_page_or_advance_epoch() {
    for count in [0, 32] {
        let code = assemble(|a| {
            a.shl(dword_ptr(edi), cl).unwrap();
            a.ret().unwrap();
        });
        let mut m = machine(&code, 2);
        reset(&mut m, [0, count, 0, 0, 0, 0, 0, CODE], 0xcd7);
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.eflags, 0xcd7);
        assert_eq!(
            m.memory().read_data(CODE, code.len()).unwrap().as_ref(),
            code.as_slice()
        );
        let stats = m.tier1_stats();
        assert_eq!(stats.inline_accesses, 2, "one read and RET; no write");
        assert_eq!(stats.invalidations, 0);
        assert_eq!(stats.fallback_instructions, 0);
    }
}

macro_rules! set_condition {
    ($a:expr, $condition:expr, $dst:expr) => {
        match $condition {
            0 => $a.seto($dst),
            1 => $a.setno($dst),
            2 => $a.setb($dst),
            3 => $a.setae($dst),
            4 => $a.sete($dst),
            5 => $a.setne($dst),
            6 => $a.setbe($dst),
            7 => $a.seta($dst),
            8 => $a.sets($dst),
            9 => $a.setns($dst),
            10 => $a.setp($dst),
            11 => $a.setnp($dst),
            12 => $a.setl($dst),
            13 => $a.setge($dst),
            14 => $a.setle($dst),
            _ => $a.setg($dst),
        }
        .unwrap()
    };
}

macro_rules! cmov_condition {
    ($a:expr, $condition:expr, $dst:expr, $src:expr) => {
        match $condition {
            0 => $a.cmovo($dst, $src),
            1 => $a.cmovno($dst, $src),
            2 => $a.cmovb($dst, $src),
            3 => $a.cmovae($dst, $src),
            4 => $a.cmove($dst, $src),
            5 => $a.cmovne($dst, $src),
            6 => $a.cmovbe($dst, $src),
            7 => $a.cmova($dst, $src),
            8 => $a.cmovs($dst, $src),
            9 => $a.cmovns($dst, $src),
            10 => $a.cmovp($dst, $src),
            11 => $a.cmovnp($dst, $src),
            12 => $a.cmovl($dst, $src),
            13 => $a.cmovge($dst, $src),
            14 => $a.cmovle($dst, $src),
            _ => $a.cmovg($dst, $src),
        }
        .unwrap()
    };
}

#[test]
fn setcc_and_cmovcc_cover_every_condition_and_preserve_flags() {
    for condition in 0..16 {
        for form in 0..8 {
            let code = assemble(|a| {
                match form {
                    0 => set_condition!(a, condition, al),
                    1 => set_condition!(a, condition, ah),
                    2 => set_condition!(a, condition, byte_ptr(edi)),
                    3 => cmov_condition!(a, condition, eax, ecx),
                    4 => cmov_condition!(a, condition, eax, dword_ptr(edi)),
                    5 => cmov_condition!(a, condition, ax, cx),
                    6 => cmov_condition!(a, condition, ax, word_ptr(edi)),
                    _ => cmov_condition!(a, condition, eax, eax),
                }
                a.ret().unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            machines[2].set_tier_verification(Some(true));
            for flags in 0..32 {
                let flags = 0x412
                    | (flags & 1)
                    | (flags & 2) << 1
                    | (flags & 4) << 4
                    | (flags & 8) << 4
                    | (flags & 16) << 7;
                for m in &mut machines {
                    reset(
                        m,
                        [0xabcd1234, 0x8765fedc, 7, 9, 0, 11, 13, DATA + 3],
                        flags,
                    );
                    m.write(DATA, &[0xa5; 12]).unwrap();
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(m.state.eflags, flags);
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
            }
            assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
            assert_eq!(machines[2].stats.verification.mismatches, 0);
        }
    }
}

#[test]
fn conditional_memory_faults_read_false_cmov_and_retry_setcc() {
    for form in 0..3 {
        for flags in [2, 0x42] {
            let code = assemble(|a| {
                a.mov(esi, 17).unwrap();
                match form {
                    0 => a.sete(byte_ptr(edi)),
                    1 => a.cmove(eax, dword_ptr(edi)),
                    _ => a.cmove(ax, word_ptr(edi)),
                }
                .unwrap();
                a.ret().unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            let address = DATA + if form == 0 { 4096 } else { 4095 };
            for m in &mut machines {
                reset(m, [0xabcd1234, 3, 5, 7, 0, 11, 0, address], flags);
                m.write(address, &0x8765fedcu32.to_le_bytes()).unwrap();
                m.protect(DATA + 4096, 4096, if form == 0 { READ } else { 0 })
                    .unwrap();
            }
            let outcome = machines[0].run(STOP, 100).unwrap();
            assert!(if form == 0 {
                matches!(outcome, RunOutcome::WriteFault { .. })
            } else {
                matches!(outcome, RunOutcome::ReadFault { .. })
            });
            for m in &mut machines[1..] {
                assert_eq!(m.run(STOP, 100).unwrap(), outcome);
            }
            equivalent(&machines[0], &machines[1], u32::MAX);
            equivalent(&machines[0], &machines[2], u32::MAX);
            assert_eq!(machines[2].tier1_stats().native_instructions, 1);
            for m in &mut machines {
                m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            }
            equivalent(&machines[0], &machines[1], u32::MAX);
            equivalent(&machines[0], &machines[2], u32::MAX);
            for m in &machines[1..] {
                assert_eq!(
                    machines[0].memory().read_data(address, 4).unwrap(),
                    m.memory().read_data(address, 4).unwrap()
                );
            }
        }
    }
}
#[test]
fn signed_multiply_matches_overflow_and_preserves_undefined_flags() {
    for form in 0..7 {
        let code = assemble(|a| {
            match form {
                0 => a.imul_2(eax, ecx),
                1 => a.imul_2(eax, dword_ptr(edi)),
                2 => a.imul_3(eax, ecx, -1),
                3 => a.imul_3(eax, dword_ptr(edi), -127),
                4 => a.imul_3(eax, ecx, 0x7fffffff),
                5 => a.imul_3(eax, dword_ptr(edi), 0x80000000u32 as i32),
                _ => a.imul_2(eax, eax),
            }
            .unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        let values = [
            0,
            1,
            2,
            0xffff,
            0x10000,
            0x7fffffff,
            0x80000000,
            0xfffffffe,
            u32::MAX,
        ];
        for left in values {
            for right in values {
                for flags in [2, 0xcd7] {
                    for m in &mut machines {
                        reset(m, [left, right, 5, 7, 0, 11, 13, DATA], flags);
                        m.write(DATA, &right.to_le_bytes()).unwrap();
                        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    }
                    equivalent(&machines[0], &machines[1], u32::MAX);
                    equivalent(&machines[0], &machines[2], u32::MAX);
                }
            }
        }
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
        assert_eq!(machines[2].stats.verification.mismatches, 0);
    }
}

#[test]
fn register_extensions_exchange_and_cdq_preserve_aliases_and_flags() {
    for form in 0..16 {
        let code = assemble(|a| {
            match form {
                0 => a.cdq(),
                1 => a.movzx(eax, cl),
                2 => a.movzx(eax, ch),
                3 => a.movzx(eax, cx),
                4 => a.movsx(eax, cl),
                5 => a.movsx(eax, ch),
                6 => a.movsx(eax, cx),
                7 => a.movsx(ax, ah),
                8 => a.movzx(ax, ah),
                9 => a.xchg(eax, ecx),
                10 => a.xchg(ax, cx),
                11 => a.xchg(al, ch),
                12 => a.xchg(al, ah),
                13 => a.xchg(eax, eax),
                14 => a.movsx(eax, ah),
                _ => a.lea(eax, dword_ptr(ebx + ecx * 8 + 0x7fffffff)),
            }
            .unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        let values = [
            0,
            1,
            0x7f,
            0x80,
            0xff,
            0x7fff,
            0x8000,
            0xffff,
            0x7fffffff,
            0x80000000,
            u32::MAX,
        ];
        for left in values {
            for right in values {
                for m in &mut machines {
                    reset(
                        m,
                        [left, right, 0x12345678, 0xfffffff0, 0, 11, 13, DATA],
                        0xcd7,
                    );
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(m.state.eflags, 0xcd7);
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
            }
        }
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
        assert_eq!(machines[2].stats.verification.mismatches, 0);
    }
}

#[test]
fn multiply_and_carry_memory_source_faults_preserve_destination_and_flags() {
    for form in 0..4 {
        let code = assemble(|a| {
            a.mov(esi, 17).unwrap();
            match form {
                0 => a.imul_2(eax, dword_ptr(edi)),
                1 => a.imul_3(eax, dword_ptr(edi), -1),
                2 => a.adc(eax, dword_ptr(edi)),
                _ => a.sbb(eax, dword_ptr(edi)),
            }
            .unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for m in &mut machines {
            reset(m, [0x80000000, 3, 5, 7, 0, 11, 0, DATA + 4095], 0xcd7);
            m.write(DATA + 4095, &u32::MAX.to_le_bytes()).unwrap();
            m.protect(DATA + 4096, 4096, 0).unwrap();
        }
        let outcome = machines[0].run(STOP, 100).unwrap();
        assert!(matches!(outcome, RunOutcome::ReadFault { .. }));
        for m in &mut machines[1..] {
            assert_eq!(m.run(STOP, 100).unwrap(), outcome);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        assert_eq!(machines[2].tier1_stats().native_instructions, 1);
        for m in &mut machines {
            m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
    }
}

fn double_shift_instruction(
    a: &mut CodeAssembler,
    left: bool,
    word: bool,
    memory: bool,
    count: Option<u32>,
) {
    match (left, word, memory, count) {
        (true, true, true, Some(n)) => a.shld(word_ptr(edi), dx, n),
        (true, false, true, Some(n)) => a.shld(dword_ptr(edi), edx, n),
        (true, true, false, Some(n)) => a.shld(ax, dx, n),
        (true, false, false, Some(n)) => a.shld(eax, edx, n),
        (false, true, true, Some(n)) => a.shrd(word_ptr(edi), dx, n),
        (false, false, true, Some(n)) => a.shrd(dword_ptr(edi), edx, n),
        (false, true, false, Some(n)) => a.shrd(ax, dx, n),
        (false, false, false, Some(n)) => a.shrd(eax, edx, n),
        (true, true, true, None) => a.shld(word_ptr(edi), dx, cl),
        (true, false, true, None) => a.shld(dword_ptr(edi), edx, cl),
        (true, true, false, None) => a.shld(ax, dx, cl),
        (true, false, false, None) => a.shld(eax, edx, cl),
        (false, true, true, None) => a.shrd(word_ptr(edi), dx, cl),
        (false, false, true, None) => a.shrd(dword_ptr(edi), edx, cl),
        (false, true, false, None) => a.shrd(ax, dx, cl),
        (false, false, false, None) => a.shrd(eax, edx, cl),
    }
    .unwrap();
}

#[test]
fn double_shifts_match_concatenation_flags_and_all_masked_counts() {
    for left in [false, true] {
        for word in [false, true] {
            for memory in [false, true] {
                for immediate in [false, true] {
                    for count in [0, 1, 2, 15, 16, 17, 31, 32, 33, 255] {
                        let code = assemble(|a| {
                            double_shift_instruction(
                                a,
                                left,
                                word,
                                memory,
                                immediate.then_some(count),
                            );
                            a.ret().unwrap();
                        });
                        let mut machines =
                            [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                        machines[2].set_tier_verification(Some(true));
                        for value in [0, 1, 0x80018001, 0x7fff7fff, u32::MAX] {
                            for source in [0, 0xabcde123, u32::MAX] {
                                for flags in [2, 0xcd7] {
                                    for m in &mut machines {
                                        reset(
                                            m,
                                            [value, count, source, 0, 0, 0, 0, DATA + 3],
                                            flags,
                                        );
                                        m.write(DATA + 3, &value.to_le_bytes()).unwrap();
                                        assert_eq!(
                                            m.run(STOP, 100).unwrap(),
                                            RunOutcome::ReachedStop
                                        );
                                    }
                                    equivalent(&machines[0], &machines[1], u32::MAX);
                                    equivalent(&machines[0], &machines[2], u32::MAX);
                                }
                            }
                        }
                        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                        assert_eq!(machines[2].stats.verification.mismatches, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn double_shift_memory_refusal_and_zero_count_do_not_commit() {
    for left in [false, true] {
        for word in [false, true] {
            for count in [0, 1, 17, 32] {
                let code = assemble(|a| {
                    double_shift_instruction(a, left, word, true, None);
                    a.ret().unwrap();
                });
                for permission in [0, READ, READ | WRITE | EXEC] {
                    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                    for m in &mut machines {
                        reset(m, [0, count, 0x87561234, 0, 0, 0, 0, DATA + 4095], 0xcd7);
                        m.write(DATA + 4095, &0xabcd8765u32.to_le_bytes()).unwrap();
                        m.protect(DATA + 4096, 4096, permission).unwrap();
                    }
                    let outcomes: Vec<_> = machines
                        .iter_mut()
                        .map(|m| m.run(STOP, 100).unwrap())
                        .collect();
                    assert_eq!(outcomes[0], outcomes[1]);
                    assert_eq!(outcomes[0], outcomes[2]);
                    for m in &mut machines {
                        m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
                    }
                    equivalent(&machines[0], &machines[1], u32::MAX);
                    equivalent(&machines[0], &machines[2], u32::MAX);
                    for m in &mut machines {
                        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    }
                    equivalent(&machines[0], &machines[1], u32::MAX);
                    equivalent(&machines[0], &machines[2], u32::MAX);
                }
            }
        }
    }
}

#[test]
fn counter_branches_preserve_flags_and_wrap_the_selected_counter() {
    for operation in 0..5 {
        use iced_x86::Code;
        let code = counter_branch_program(match operation {
            0 => Code::Jecxz_rel8_32,
            1 => Code::Jcxz_rel8_32,
            2 => Code::Loop_rel8_32_ECX,
            3 => Code::Loope_rel8_32_ECX,
            _ => Code::Loopne_rel8_32_ECX,
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        for count in [0, 1, 2, 0x10000, 0xffff0001, u32::MAX] {
            for flags in [2, 0xcd7, 0xc97] {
                for m in &mut machines {
                    reset(m, [0, count, 0, 0, 0, 0, 0, 0], flags);
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(m.state.eflags, flags);
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
            }
        }
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
        assert_eq!(machines[2].stats.verification.mismatches, 0);
    }
}

#[test]
fn pushad_popad_preserve_original_esp_and_skip_saved_esp() {
    for pop in [false, true] {
        let code = assemble(|a| {
            if pop { a.popad() } else { a.pushad() }.unwrap();
            a.jmp(u64::from(STOP)).unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        for m in &mut machines {
            reset(m, [3, 5, 7, 11, 0, 13, 17, 19], 0xcd7);
            m.state.regs[4] = DATA + 128;
            let words: Vec<_> = (0u32..8)
                .flat_map(|n| (n * 0x11223344).to_le_bytes())
                .collect();
            m.write(DATA + 128, &words).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
        assert_eq!(machines[2].stats.verification.mismatches, 0);
        assert_eq!(
            machines[2].state.regs[4],
            if pop { DATA + 160 } else { DATA + 96 }
        );
        if !pop {
            assert_eq!(
                machines[2].memory().read(DATA + 108, 4).unwrap(),
                &(DATA + 128).to_le_bytes()
            );
        }
    }
}

#[test]
fn all_register_stack_faults_preserve_every_register_and_memory() {
    for pop in [false, true] {
        for split in [1, 4, 12, 16, 28, 31] {
            let code = assemble(|a| {
                if pop { a.popad() } else { a.pushad() }.unwrap();
                a.jmp(u64::from(STOP)).unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            for m in &mut machines {
                reset(m, [3, 5, 7, 11, 0, 13, 17, 19], 0xcd7);
                m.state.regs[4] = if pop {
                    DATA + 4096 - split
                } else {
                    DATA + 4096 + split
                };
                m.write(DATA, &[0xa5; 8192]).unwrap();
                m.protect(DATA + 4096, 4096, 0).unwrap();
            }
            let original = machines[2].state.regs;
            let outcomes: Vec<_> = machines
                .iter_mut()
                .map(|m| m.run(STOP, 100).unwrap())
                .collect();
            assert_eq!(outcomes[0], outcomes[1]);
            assert_eq!(outcomes[0], outcomes[2]);
            assert_eq!(machines[2].state.regs, original);
            assert_eq!(machines[2].state.eip, CODE);
            for m in &mut machines {
                m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
            }
            equivalent(&machines[0], &machines[1], u32::MAX);
            equivalent(&machines[0], &machines[2], u32::MAX);
            assert_eq!(
                machines[2].memory().read(DATA, 8192).unwrap(),
                &[0xa5; 8192]
            );
            for m in &mut machines {
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            }
            equivalent(&machines[0], &machines[1], u32::MAX);
            equivalent(&machines[0], &machines[2], u32::MAX);
        }
    }
}

#[test]
fn pushad_executable_stack_write_retires_once() {
    let code = assemble(|a| {
        a.pushad().unwrap();
        a.jmp(u64::from(STOP)).unwrap();
    });
    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
    for (mode, m) in machines.iter_mut().enumerate() {
        reset(m, [3, 5, 7, 11, 0, 13, 17, 19], 0xcd7);
        m.state.regs[4] = DATA + 128;
        m.protect(DATA, 4096, READ | WRITE | EXEC).unwrap();
        if mode != 1 {
            assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
        }
    }
    equivalent(&machines[0], &machines[2], u32::MAX);
    assert_eq!(machines[2].tier1_stats().native_instructions, 1);
    for m in &mut machines {
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&machines[0], &machines[1], u32::MAX);
    equivalent(&machines[0], &machines[2], u32::MAX);
    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
}

fn wide_arithmetic_instruction(a: &mut CodeAssembler, operation: u32, form: u32) {
    match (operation, form) {
        (0, 0) => a.mul(ebx),
        (0, 1) => a.mul(eax),
        (0, 2) => a.mul(edx),
        (0, _) => a.mul(dword_ptr(edi)),
        (1, 0) => a.imul(ebx),
        (1, 1) => a.imul(eax),
        (1, 2) => a.imul(edx),
        (1, _) => a.imul(dword_ptr(edi)),
        (2, 0) => a.div(ebx),
        (2, 1) => a.div(eax),
        (2, 2) => a.div(edx),
        (2, _) => a.div(dword_ptr(edi)),
        (_, 0) => a.idiv(ebx),
        (_, 1) => a.idiv(eax),
        (_, 2) => a.idiv(edx),
        (_, _) => a.idiv(dword_ptr(edi)),
    }
    .unwrap();
}

#[test]
fn full_width_multiply_divide_match_results_flags_and_divide_errors() {
    let values = [0, 1, 2, 0x7fffffff, 0x80000000, 0xfffffffe, u32::MAX];
    for operation in 0..4 {
        for form in 0..4 {
            let code = assemble(|a| {
                wide_arithmetic_instruction(a, operation, form);
                a.ret().unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            machines[2].set_tier_verification(Some(true));
            for low in values {
                for high in values {
                    for divisor in values {
                        for flags in [2, 0xcd7] {
                            for m in &mut machines {
                                reset(m, [low, 0, high, divisor, 0, 0, 0, DATA], flags);
                                m.write(DATA, &divisor.to_le_bytes()).unwrap();
                            }
                            let before = machines[2].state.regs;
                            let outcomes: Vec<_> = machines
                                .iter_mut()
                                .map(|m| m.run(STOP, 100).unwrap())
                                .collect();
                            assert_eq!(outcomes[0], outcomes[1]);
                            assert_eq!(outcomes[0], outcomes[2]);
                            equivalent(&machines[0], &machines[1], u32::MAX);
                            equivalent(&machines[0], &machines[2], u32::MAX);
                            if outcomes[0] != RunOutcome::ReachedStop {
                                assert_eq!(
                                    outcomes[0],
                                    RunOutcome::Trap {
                                        pc: CODE,
                                        vector: 0
                                    }
                                );
                                assert_eq!(machines[2].state.regs, before);
                                assert_eq!(machines[2].state.eip, CODE);
                                assert_eq!(machines[2].state.eflags, flags);
                            }
                        }
                    }
                }
            }
            assert_eq!(
                machines[2].tier1_stats().fallback_instructions,
                0,
                "faults do not retire"
            );
            if operation == 2 && form == 2 {
                // DIV EDX always overflows, or divides by zero when EDX is zero.
                assert_eq!(machines[2].tier1_stats().native_instructions, 0);
            } else {
                assert!(machines[2].tier1_stats().native_instructions > 0);
            }
            assert_eq!(machines[2].stats.verification.mismatches, 0);
        }
    }
}

#[test]
fn wide_arithmetic_memory_fault_precedes_divide_error_and_preserves_inputs() {
    for operation in 0..4 {
        let code = assemble(|a| {
            wide_arithmetic_instruction(a, operation, 3);
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for m in &mut machines {
            reset(m, [0, 0, u32::MAX, 0, 0, 0, 0, DATA + 4095], 0xcd7);
            m.protect(DATA + 4096, 4096, 0).unwrap();
        }
        let outcomes: Vec<_> = machines
            .iter_mut()
            .map(|m| m.run(STOP, 100).unwrap())
            .collect();
        assert!(matches!(outcomes[0], RunOutcome::ReadFault { .. }));
        assert_eq!(outcomes[0], outcomes[1]);
        assert_eq!(outcomes[0], outcomes[2]);
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
    }
}

fn narrow_alu_instruction(a: &mut CodeAssembler, operation: u32, form: u32, immediate: bool) {
    macro_rules! emit {
        ($dst:expr, $src:expr) => {
            match operation {
                0 => {
                    if immediate {
                        a.add($dst, -1)
                    } else {
                        a.add($dst, $src)
                    }
                }
                1 => {
                    if immediate {
                        a.sub($dst, -1)
                    } else {
                        a.sub($dst, $src)
                    }
                }
                2 => {
                    if immediate {
                        a.and($dst, -1)
                    } else {
                        a.and($dst, $src)
                    }
                }
                3 => {
                    if immediate {
                        a.or($dst, -1)
                    } else {
                        a.or($dst, $src)
                    }
                }
                4 => {
                    if immediate {
                        a.xor($dst, -1)
                    } else {
                        a.xor($dst, $src)
                    }
                }
                5 => {
                    if immediate {
                        a.cmp($dst, -1)
                    } else {
                        a.cmp($dst, $src)
                    }
                }
                6 => {
                    if immediate {
                        a.test($dst, -1)
                    } else {
                        a.test($dst, $src)
                    }
                }
                7 => a.inc($dst),
                8 => a.dec($dst),
                9 => a.neg($dst),
                _ => a.not($dst),
            }
            .unwrap()
        };
    }
    match form {
        0 => emit!(al, dh),
        1 => emit!(ah, dl),
        2 => emit!(ah, al),
        _ => emit!(ax, dx),
    }
}

#[test]
fn narrow_alu_preserves_other_lanes_and_matches_full_flags() {
    for operation in 0..11 {
        for form in 0..4 {
            for immediate in [false, true] {
                let code = assemble(|a| {
                    narrow_alu_instruction(a, operation, form, immediate);
                    a.ret().unwrap();
                });
                let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                machines[2].set_tier_verification(Some(true));
                for left in [0, 1, 15, 16, 127, 128, 255, 0x7fff, 0x8000, 0xffff] {
                    for right in [0, 1, 15, 16, 127, 128, 255, 0x7fff, 0x8000, 0xffff] {
                        for flags in [2, 0xcd7] {
                            let eax_value = 0xabcd0000
                                | if form == 1 || form == 2 {
                                    (left & 255) << 8 | right & 255
                                } else {
                                    left
                                };
                            let edx_value = if form == 0 { (right & 255) << 8 } else { right };
                            for m in &mut machines {
                                reset(m, [eax_value, 0, edx_value, 0, 0, 0, 0, 0], flags);
                                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                            }
                            // Cranelift uses a different value for undefined logical AF.
                            let mask = if matches!(operation, 2 | 3 | 4 | 6) {
                                !0x10
                            } else {
                                u32::MAX
                            };
                            equivalent(&machines[0], &machines[1], mask);
                            equivalent(&machines[0], &machines[2], u32::MAX);
                        }
                    }
                }
                assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                assert_eq!(machines[2].stats.verification.mismatches, 0);
            }
        }
    }
}

fn bit_instruction(a: &mut CodeAssembler, operation: u32, word: bool, immediate: Option<u32>) {
    macro_rules! emit {
        ($dst:expr, $index:expr) => {
            match operation {
                0 => a.bt($dst, $index),
                1 => a.bts($dst, $index),
                2 => a.btr($dst, $index),
                _ => a.btc($dst, $index),
            }
            .unwrap()
        };
    }
    match (word, immediate) {
        (true, Some(n)) => emit!(ax, n),
        (false, Some(n)) => emit!(eax, n),
        (true, None) => emit!(ax, dx),
        (false, None) => emit!(eax, edx),
    }
}

#[test]
fn register_bits_mask_indices_and_only_replace_carry() {
    for operation in 0..4 {
        for word in [false, true] {
            for immediate in [false, true] {
                for index in [0, 1, 15, 16, 31, 32, 63, 255] {
                    let code = assemble(|a| {
                        bit_instruction(a, operation, word, immediate.then_some(index));
                        a.ret().unwrap();
                    });
                    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                    machines[2].set_tier_verification(Some(true));
                    for value in [0, 1, 0xabcd8001, u32::MAX] {
                        for flags in [2, 0xcd7] {
                            for m in &mut machines {
                                reset(m, [value, 0, index, 0, 0, 0, 0, 0], flags);
                                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                            }
                            equivalent(&machines[0], &machines[1], u32::MAX);
                            equivalent(&machines[0], &machines[2], u32::MAX);
                        }
                    }
                    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                    assert_eq!(machines[2].stats.verification.mismatches, 0);
                }
            }
        }
    }
}

#[test]
fn scans_preserve_zero_destination_and_bswap_preserves_flags() {
    for operation in 0..2 {
        for word in [false, true] {
            for form in 0..3 {
                let code = assemble(|a| {
                    match (operation, word, form) {
                        (0, true, 0) => a.bsf(ax, dx),
                        (0, false, 0) => a.bsf(eax, edx),
                        (0, true, 1) => a.bsf(ax, ax),
                        (0, false, 1) => a.bsf(eax, eax),
                        (0, true, _) => a.bsf(ax, word_ptr(edi)),
                        (0, false, _) => a.bsf(eax, dword_ptr(edi)),
                        (_, true, 0) => a.bsr(ax, dx),
                        (_, false, 0) => a.bsr(eax, edx),
                        (_, true, 1) => a.bsr(ax, ax),
                        (_, false, 1) => a.bsr(eax, eax),
                        (_, true, _) => a.bsr(ax, word_ptr(edi)),
                        (_, false, _) => a.bsr(eax, dword_ptr(edi)),
                    }
                    .unwrap();
                    a.bswap(edx).unwrap();
                    a.ret().unwrap();
                });
                let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                machines[2].set_tier_verification(Some(true));
                for value in [0, 1, 2, 0x8000, 0x10000, 0x80000000, 0x12345678, u32::MAX] {
                    for flags in [2, 0xcd7] {
                        for m in &mut machines {
                            reset(
                                m,
                                [
                                    if form == 1 { value } else { 0xabcde123 },
                                    0,
                                    value,
                                    0,
                                    0,
                                    0,
                                    0,
                                    DATA,
                                ],
                                flags,
                            );
                            m.write(DATA, &value.to_le_bytes()).unwrap();
                            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                        }
                        equivalent(&machines[0], &machines[1], u32::MAX);
                        equivalent(&machines[0], &machines[2], u32::MAX);
                    }
                }
                assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                assert_eq!(machines[2].stats.verification.mismatches, 0);
            }
        }
    }
}

#[test]
fn bit_scan_memory_fault_preserves_zero_flag_and_destination() {
    let code = assemble(|a| {
        a.bsf(eax, dword_ptr(edi)).unwrap();
        a.ret().unwrap();
    });
    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
    for m in &mut machines {
        reset(m, [0xabcde123, 0, 0, 0, 0, 0, 0, DATA + 4095], 0xcd7);
        m.protect(DATA + 4096, 4096, 0).unwrap();
    }
    let outcomes: Vec<_> = machines
        .iter_mut()
        .map(|m| m.run(STOP, 100).unwrap())
        .collect();
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0], outcomes[2]);
    equivalent(&machines[0], &machines[1], u32::MAX);
    equivalent(&machines[0], &machines[2], u32::MAX);
}

#[test]
fn overlapping_rep_is_verified_at_each_retired_element() {
    for reverse in [false, true] {
        let code = assemble(|a| {
            a.rep().movsd().unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        for m in &mut machines {
            reset(
                m,
                [
                    0,
                    3,
                    0,
                    0,
                    0,
                    0,
                    DATA + 64,
                    if reverse { DATA + 60 } else { DATA + 68 },
                ],
                0x8d7 | u32::from(reverse) * 0x400,
            );
            m.write(DATA, &(0u8..128).collect::<Vec<_>>()).unwrap();
        }
        for left in [2, 1, 0] {
            for index in [0, 2] {
                assert_eq!(machines[index].run(STOP, 1).unwrap(), RunOutcome::Budget);
                assert_eq!(machines[index].state.regs[1], left);
            }
            equivalent(&machines[0], &machines[2], u32::MAX);
        }
        assert!(machines[2].stats.verification.verified >= 3);
        assert_eq!(machines[2].stats.verification.inconclusive, 0);
        for m in &mut machines {
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
    }
}

#[test]
fn word_counter_loops_preserve_upper_ecx_and_wrap_without_flags() {
    use iced_x86::Code;
    for opcode in [
        Code::Loop_rel8_32_CX,
        Code::Loope_rel8_32_CX,
        Code::Loopne_rel8_32_CX,
    ] {
        let code = counter_branch_program(opcode);
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        for count in [0, 1, 2, 0x12340000, 0x12340001, u32::MAX] {
            for flags in [2, 0xcd7] {
                for m in &mut machines {
                    reset(m, [0, count, 0, 0, 0, 0, 0, 0], flags);
                    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(
                        m.state.regs[1],
                        count & 0xffff0000 | count.wrapping_sub(1) & 0xffff
                    );
                }
                equivalent(&machines[0], &machines[1], u32::MAX);
                equivalent(&machines[0], &machines[2], u32::MAX);
            }
        }
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
    }
}

#[test]
fn double_shift_and_bit_indices_read_aliases_before_writing() {
    for operation in 0..4 {
        let code = assemble(|a| {
            match operation {
                0 => a.shld(ecx, ecx, cl),
                1 => a.shrd(cx, cx, cl),
                2 => a.btc(ecx, ecx),
                _ => a.bts(cx, cx),
            }
            .unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        machines[2].set_tier_verification(Some(true));
        for count in [0, 1, 15, 16, 17, 31, 32, 33, 255] {
            for m in &mut machines {
                reset(m, [0, 0x9876cd00 | count, 0, 0, 0, 0, 0, 0], 0xcd7);
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            }
            equivalent(&machines[0], &machines[1], u32::MAX);
            equivalent(&machines[0], &machines[2], u32::MAX);
        }
        assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
    }
}

#[test]
fn string_fs_wraps_and_word_address_form_stays_in_tier0() {
    use iced_x86::{Instruction, Register, RepPrefixKind};
    for word in [false, true] {
        let code = assemble(|a| {
            a.add_instruction(
                Instruction::with_lodsw(
                    if word { 16 } else { 32 },
                    Register::FS,
                    RepPrefixKind::Repe,
                )
                .unwrap(),
            )
            .unwrap();
            a.ret().unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for m in &mut machines {
            reset(
                m,
                [
                    0xabcd0000,
                    if word { 0x12340002 } else { 2 },
                    0,
                    0,
                    0,
                    0,
                    0xfffffffe,
                    0,
                ],
                0xcd7,
            );
            m.state.fs_base = if word {
                DATA.wrapping_sub(0xfffe)
            } else {
                DATA + 2
            };
            m.write(DATA - 2, &[0x34, 0x12, 0x78, 0x56]).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        assert_eq!(machines[2].state.regs[0], 0xabcd1234);
        assert_eq!(
            machines[2].tier1_stats().fallback_instructions,
            if word { 2 } else { 0 }
        );
    }
}

fn counter_branch_program(opcode: iced_x86::Code) -> Vec<u8> {
    let mut code = assemble(|a| {
        a.add_instruction(
            iced_x86::Instruction::with_branch(opcode, u64::from(CODE + 16)).unwrap(),
        )
        .unwrap();
        a.mov(eax, 1).unwrap();
        a.ret().unwrap();
    });
    let mut padding = CodeAssembler::new(32).unwrap();
    for _ in code.len()..16 {
        padding.nop().unwrap();
    }
    code.extend(
        padding
            .assemble(u64::from(CODE) + code.len() as u64)
            .unwrap(),
    );
    let mut body = CodeAssembler::new(32).unwrap();
    body.mov(eax, 2).unwrap();
    body.ret().unwrap();
    code.extend(body.assemble(u64::from(CODE + 16)).unwrap());
    code
}

#[test]
fn narrow_moves_preserve_partial_registers_flags_and_exact_memory_width() {
    let code = assemble(|a| {
        a.mov(al, ah).unwrap();
        a.mov(ch, al).unwrap();
        a.mov(ax, cx).unwrap();
        a.mov(bh, 0xe7u32).unwrap();
        a.mov(si, 0x98abu32).unwrap();
        a.mov(byte_ptr(DATA), ah).unwrap();
        a.mov(word_ptr(DATA + 1), si).unwrap();
        a.mov(byte_ptr(DATA + 3), 0xf3u32).unwrap();
        a.mov(word_ptr(DATA + 4), 0xcdefu32).unwrap();
        a.mov(dh, byte_ptr(DATA + 4095)).unwrap();
        a.mov(bp, word_ptr(DATA + 4094)).unwrap();
        a.ret().unwrap();
    });
    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
    machines[2].set_tier_verification(Some(true));
    for flags in [2, 0x8d7, 0xcd7] {
        for m in &mut machines {
            reset(
                m,
                [
                    0x1234abcd, 0xa5b5c5d5, 0x87654321, 0xffeeccdd, 0, 0xfeed0000, 0xbeef0000, 0,
                ],
                flags,
            );
            m.write(DATA, &[0x5a; 4096]).unwrap();
            m.protect(DATA + 4096, 4096, 0).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        assert_eq!(machines[2].state.regs[5], 0xfeed5a5a);
        assert_eq!(
            machines[2].memory().read_data(DATA, 7).unwrap().as_ref(),
            &[0xab, 0xab, 0x98, 0xf3, 0xef, 0xcd, 0x5a]
        );
    }
    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
    assert_eq!(machines[2].stats.verification.mismatches, 0);
}

#[test]
fn narrow_move_faults_preserve_the_instruction_and_resume_with_fs() {
    for width in [1, 2] {
        for store in [false, true] {
            let code = assemble(|a| {
                a.inc(ebx).unwrap();
                match (width, store) {
                    (1, false) => a.mov(ah, byte_ptr(esi).fs()),
                    (2, false) => a.mov(ax, word_ptr(esi).fs()),
                    (1, true) => a.mov(byte_ptr(esi).fs(), ah),
                    _ => a.mov(word_ptr(esi).fs(), ax),
                }
                .unwrap();
                a.ret().unwrap();
            });
            let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
            for m in &mut machines {
                reset(m, [0x1234abcd, 0, 0, 0, 0, 0, u32::MAX - 1, 0], 0x8d7);
                m.state.fs_base = DATA + 4096 + 3 - width;
                m.write(DATA + 4094, &[0x55; 4]).unwrap();
                m.protect(DATA + 4096, 4096, if store { READ } else { 0 })
                    .unwrap();
            }
            let expected = machines[0].run(STOP, 100).unwrap();
            assert!(matches!(
                expected,
                RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
            ));
            for m in &mut machines[1..] {
                assert_eq!(m.run(STOP, 100).unwrap(), expected);
            }
            equivalent(&machines[0], &machines[1], u32::MAX);
            equivalent(&machines[0], &machines[2], u32::MAX);
            assert_eq!(machines[2].state.regs[0], 0x1234abcd);
            for m in &mut machines {
                m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            }
            equivalent(&machines[0], &machines[1], u32::MAX);
            equivalent(&machines[0], &machines[2], u32::MAX);
        }
    }
}

#[test]
fn narrow_code_stores_revalidate_before_executing_the_successor() {
    let replacement = assemble(|a| a.inc(ebx).unwrap());
    assert_eq!(replacement.len(), 1);
    let code = assemble(|a| {
        a.mov(byte_ptr(CODE + 32), u32::from(replacement[0]))
            .unwrap();
        a.jmp(u64::from(CODE + 32)).unwrap();
        for _ in 0..32 {
            a.nop().unwrap();
        }
        a.ret().unwrap();
    });
    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
    for m in &mut machines {
        m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&machines[0], &machines[1], u32::MAX);
    equivalent(&machines[0], &machines[2], u32::MAX);
    assert_eq!(machines[2].state.regs[3], 1);
    assert!(machines[2].tier1_stats().invalidations > 0);
}

fn narrow_alu(a: &mut CodeAssembler, width: u32, operation: u32, source: u32) {
    macro_rules! emit {
        ($reg:expr, $other:expr, $mem:expr) => {{
            macro_rules! binary {
                ($name:ident) => {
                    match source {
                        0 => a.$name($reg, $other),
                        1 => a.$name($reg, -1i32),
                        _ => a.$name($reg, $mem),
                    }
                };
            }
            match operation {
                0 => binary!(add),
                1 => binary!(adc),
                2 => binary!(sub),
                3 => binary!(sbb),
                4 => binary!(and),
                5 => binary!(or),
                6 => binary!(xor),
                7 => binary!(cmp),
                _ => match source {
                    0 => a.test($reg, $other),
                    1 => a.test($reg, -1i32),
                    _ => a.test($mem, $reg),
                },
            }
            .unwrap();
        }};
    }
    if width == 1 {
        emit!(ah, bh, byte_ptr(DATA + 4095));
    } else {
        emit!(ax, bx, word_ptr(DATA + 4094));
    }
}

#[test]
fn narrow_register_alu_memory_sources_and_carry_match_all_tiers() {
    for width in [1, 2] {
        for operation in 0..9 {
            for source in 0..3 {
                let code = assemble(|a| {
                    narrow_alu(a, width, operation, source);
                    a.ret().unwrap();
                });
                let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                machines[2].set_tier_verification(Some(true));
                for (left, right) in [
                    (0, 0),
                    (0, 0xffff),
                    (0x7f7f, 0x0101),
                    (0x8080, 0xffff),
                    (0xffff, 0x0101),
                    (0x7fff, 1),
                    (0x8000, 1),
                ] {
                    for flags in [0x8d6, 0x8d7] {
                        for m in &mut machines {
                            reset(
                                m,
                                [0x12340000 | left, 0, 0, 0x56780000 | right, 0, 0, 0, 0],
                                flags,
                            );
                            m.write(DATA + 4094, &(right as u16).to_le_bytes()).unwrap();
                            m.protect(DATA + 4096, 4096, 0).unwrap();
                            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                        }
                        // Cranelift may clear undefined logical AF; Tier 1 must
                        // still match Tier 0 preservation, including that bit.
                        let mask = if matches!(operation, 4 | 5 | 6 | 8) {
                            !0x10
                        } else {
                            u32::MAX
                        };
                        equivalent(&machines[0], &machines[1], mask);
                        equivalent(&machines[0], &machines[2], u32::MAX);
                    }
                }
                assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                assert_eq!(machines[2].stats.verification.mismatches, 0);
            }
        }
    }
}

#[test]
fn narrow_memory_comparisons_are_read_only_and_fault_before_flags_change() {
    for width in [1, 2] {
        for immediate in [false, true] {
            for test in [false, true] {
                let code = assemble(|a| {
                    match (width, immediate, test) {
                        (1, false, false) => a.cmp(byte_ptr(DATA + 4095), ah),
                        (1, true, false) => a.cmp(byte_ptr(DATA + 4095), -1i32),
                        (1, false, true) => a.test(byte_ptr(DATA + 4095), ah),
                        (1, true, true) => a.test(byte_ptr(DATA + 4095), 0x80u32),
                        (_, false, false) => a.cmp(word_ptr(DATA + 4095), ax),
                        (_, true, false) => a.cmp(word_ptr(DATA + 4095), -1i32),
                        (_, false, true) => a.test(word_ptr(DATA + 4095), ax),
                        _ => a.test(word_ptr(DATA + 4095), 0x8000u32),
                    }
                    .unwrap();
                    a.ret().unwrap();
                });
                let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
                for m in &mut machines {
                    reset(m, [0xa5a58080, 0, 0, 0, 0, 0, 0, 0], 0xcd7);
                    m.write(DATA + 4095, &[0x80, 0x7f]).unwrap();
                    m.protect(DATA, 4096, READ).unwrap();
                    m.protect(DATA + 4096, 4096, 0).unwrap();
                }
                let expected = machines[0].run(STOP, 100).unwrap();
                for m in &mut machines[1..] {
                    assert_eq!(m.run(STOP, 100).unwrap(), expected);
                }
                // TEST leaves AF undefined only after successful retirement.
                let mask = if test && width == 1 { !0x10 } else { u32::MAX };
                equivalent(&machines[0], &machines[1], mask);
                equivalent(&machines[0], &machines[2], u32::MAX);
                if width == 2 {
                    assert!(matches!(expected, RunOutcome::ReadFault { .. }));
                    assert_eq!(machines[2].state.eflags, 0xcd7);
                    for m in &mut machines {
                        m.protect(DATA + 4096, 4096, READ).unwrap();
                        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                    }
                    equivalent(
                        &machines[0],
                        &machines[1],
                        if test { !0x10 } else { u32::MAX },
                    );
                    equivalent(&machines[0], &machines[2], u32::MAX);
                } else {
                    assert_eq!(expected, RunOutcome::ReachedStop);
                    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
                }
            }
        }
    }
}

#[test]
fn memory_push_uses_original_esp_and_leave_commits_after_the_read() {
    let code = assemble(|a| {
        a.push(dword_ptr(esp)).unwrap();
        a.pop(edx).unwrap();
        a.push(dword_ptr(esp - 4)).unwrap();
        a.pop(ecx).unwrap();
        a.push(dword_ptr(esi).fs()).unwrap();
        a.pop(eax).unwrap();
        a.leave().unwrap();
        a.ret().unwrap();
    });
    let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
    machines[2].set_tier_verification(Some(true));
    for m in &mut machines {
        reset(m, [0, 0, 0, 0, 0, DATA + 256, u32::MAX - 1, 0], 0xcd7);
        m.state.fs_base = DATA + 2;
        m.write(DATA, &0x12345678u32.to_le_bytes()).unwrap();
        m.write(DATA + 256, &0x87654321u32.to_le_bytes()).unwrap();
        m.write(DATA + 260, &STOP.to_le_bytes()).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    equivalent(&machines[0], &machines[1], u32::MAX);
    equivalent(&machines[0], &machines[2], u32::MAX);
    assert_eq!(&machines[2].state.regs[..3], &[0x12345678, STOP, STOP]);
    assert_eq!(machines[2].state.regs[4], DATA + 264);
    assert_eq!(machines[2].state.regs[5], 0x87654321);
    assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
}

#[test]
fn memory_push_and_leave_faults_keep_stack_registers_restartable() {
    for operation in 0..3 {
        let code = assemble(|a| {
            if operation == 2 {
                a.leave().unwrap();
            } else {
                a.push(dword_ptr(DATA)).unwrap();
            }
            a.jmp(u64::from(STOP)).unwrap();
        });
        let mut machines = [machine(&code, 0), machine(&code, 1), machine(&code, 2)];
        for m in &mut machines {
            reset(m, [0, 0, 0, 0, 0, DATA, 0, 0], 0x8d7);
            m.write(DATA, &0x12345678u32.to_le_bytes()).unwrap();
            if operation != 1 {
                m.protect(DATA, 4096, 0).unwrap();
            }
            // With both accesses denied, PUSH must fault on its source first.
            m.protect(STACK - 4096, 4096, READ).unwrap();
        }
        let expected = machines[0].run(STOP, 100).unwrap();
        assert!(matches!(
            expected,
            RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
        ));
        for m in &mut machines[1..] {
            assert_eq!(m.run(STOP, 100).unwrap(), expected);
        }
        assert_eq!(machines[2].state.regs[4], STACK);
        assert_eq!(machines[2].state.regs[5], DATA);
        for m in &mut machines {
            m.protect(DATA, 4096, READ | WRITE).unwrap();
            m.protect(STACK - 4096, 4096, READ | WRITE).unwrap();
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
        for m in &mut machines {
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
        equivalent(&machines[0], &machines[2], u32::MAX);
    }
}
