//! Real ARM64 native execution against Tier 0 and the existing Cranelift JIT.
//! Inputs are authored with the repository's assembler; no game data is used.
#![cfg(all(target_arch = "aarch64", target_os = "linux"))]
use adv_cpu::{CpuState, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
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
        a.bswap(eax).unwrap();
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
    assert_eq!(stats.fallback_instructions, 12, "BSWAP only");
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
        a.bswap(eax).unwrap();
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
    assert_eq!(tier.tier1_stats().fallback_instructions, 1);
}
