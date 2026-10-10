mod support;
use adv_cpu::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x1f000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x3f000;

fn assemble(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(code: &[u8], jit: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x30000).unwrap();
    m.protect(CODE, 0x30000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.protect(CODE, 0x1000, READ | EXEC).unwrap();
    m.protect(STOP, 0x1000, READ | EXEC).unwrap();
    if jit {
        m.set_region(Region::new(CODE, code.len())).unwrap();
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
        ..CpuState::default()
    };
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn equal(jit: &mut Executor, uc: &mut Executor, mask: u32) {
    assert_eq!(jit.run(STOP, 1_000_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 1_000_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(jit.state.xmm, uc.state.xmm);
    assert_eq!(jit.state.eip, uc.state.eip);
    assert_eq!(jit.state.eflags & mask, uc.state.eflags & mask);
    assert_eq!(
        jit.memory().read(DATA, 4096).unwrap(),
        uc.memory().read(DATA, 4096).unwrap()
    );
}

#[test]
fn integer_flags_match_unicorn_at_overflow_and_carry_boundaries() {
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
        let mut jit = machine(&code, true);
        let mut uc = machine(&code, false);
        let values = [
            0, 1, 15, 16, 255, 256, 0x7fffffff, 0x80000000, 0xfffffffe, 0xffffffff,
        ];
        for &a in &values {
            for &b in &values {
                let regs = [a, b, 0, 0xfffffff0, 0, 0, 0, 0];
                reset(&mut jit, regs, 0x8d7);
                reset(&mut uc, regs, 0x8d7);
                equal(
                    &mut jit,
                    &mut uc,
                    if (3..=6).contains(&operation) {
                        0x8c5
                    } else {
                        0x8d5
                    },
                );
            }
        }
        support::x64_shape!(assert_eq!(
            jit.stats.interpreter_entries, 0,
            "operation {operation}"
        ));
    }
}

#[test]
fn direct_loop_and_memory_keep_execution_in_one_native_region() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(eax, dword_ptr(esi)).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    for m in [&mut jit, &mut uc] {
        m.write(
            DATA,
            &(0..64u32).flat_map(u32::to_le_bytes).collect::<Vec<_>>(),
        )
        .unwrap();
        reset(m, [0, 64, 0, 0, 0, 0, DATA, DATA + 1024], 2);
    }
    equal(&mut jit, &mut uc, 0x8d5);
    support::x64_shape!(assert_eq!(jit.stats.native_entries, 1));
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

#[test]
fn calls_returns_and_indirect_targets_stay_native() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        let mut sub = a.create_label();
        a.set_label(&mut top).unwrap();
        a.call(sub).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        a.set_label(&mut sub).unwrap();
        a.add(eax, 3).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    reset(&mut jit, [0, 100, 0, 0, 0, 0, 0, 0], 2);
    reset(&mut uc, [0, 100, 0, 0, 0, 0, 0, 0], 2);
    equal(&mut jit, &mut uc, 0x8d5);
    assert_eq!(jit.state.regs[0], 300);
    support::x64_shape!(assert_eq!(jit.stats.native_entries, 1));
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

#[test]
fn interpreted_instruction_returns_without_replaying_native_writes() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.inc(dword_ptr(edi)).unwrap();
        // PAUSE stands in for any instruction the native compiler leaves to the
        // interpreter; these tests need another one if it ever compiles.
        a.pause().unwrap();
        a.add(eax, 1).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    let regs = [0x12345678, 5, 0, 0, 0, 0, 0, DATA];
    reset(&mut jit, regs, 2);
    reset(&mut uc, regs, 2);
    equal(&mut jit, &mut uc, 0x8d5);
    assert_eq!(jit.memory().read(DATA, 4).unwrap(), 5u32.to_le_bytes());
    // The cold interpreter may include successors in each window. Count real
    // work while the memory and architectural comparisons catch any replay.
    support::x64_shape!(assert!(jit.stats.interpreter_entries >= 5));
    support::x64_shape!(assert!(jit.stats.interpreter_instructions >= 5));
    support::x64_shape!(assert!(jit.stats.native_entries >= 5));
}

#[test]
fn faulting_rmw_keeps_registers_flags_and_memory_before_the_instruction() {
    let code = assemble(|a| {
        a.add(eax, 7).unwrap();
        a.add(dword_ptr(edi), eax).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    jit.protect(DATA + 4096, 4096, READ).unwrap();
    reset(&mut jit, [9, 0, 0, 0, 0, 0, 0, DATA + 4094], 2);
    let outcome = jit.run(STOP, 100).unwrap();
    assert!(matches!(outcome, RunOutcome::WriteFault { address, .. } if address == DATA + 4096));
    assert_eq!(jit.state.regs[0], 16);
    assert_eq!(jit.memory().read(DATA + 4094, 4).unwrap(), [0; 4]);
}

#[test]
fn faults_keep_arithmetic_flags_that_later_code_can_read() {
    let code = assemble(|a| {
        a.add(eax, 7).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.setc(al).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    jit.protect(DATA + 4096, 4096, READ).unwrap();
    reset(&mut jit, [9, 0, 0, 0, 0, 0, 0, DATA + 4094], 2);
    let outcome = jit.run(STOP, 100).unwrap();
    assert!(matches!(outcome, RunOutcome::WriteFault { address, .. } if address == DATA + 4096));
    assert_eq!(jit.state.regs[0], 16);
    // SETC still reads the ADD's flags after the store. Where every path
    // rewrites all six before reading any, as for the ADD to memory above,
    // a fault leaves them unspecified.
    assert_eq!(jit.state.eflags & 0x8d5, 0x10);
}

#[test]
fn budget_yields_in_a_busy_loop_without_a_system_call() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.inc(eax).unwrap();
        a.jmp(top).unwrap();
    });
    let mut jit = machine(&code, true);
    assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::Budget);
    assert_eq!(jit.state.regs[0], 1000);
    assert_eq!(jit.state.eip, CODE);
    assert_eq!(jit.run(STOP, 3).unwrap(), RunOutcome::Budget);
    assert_eq!(jit.state.regs[0], 1003);
}

#[test]
fn host_patch_invalidates_native_and_unicorn_code() {
    let code = assemble(|a| {
        a.mov(eax, 1).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    jit.write(CODE + 1, &42u32.to_le_bytes()).unwrap();
    reset(&mut jit, [0; 8], 2);
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(jit.state.regs[0], 42);
    support::x64_shape!(assert_eq!(jit.stats.invalidations, 1));
}

#[test]
fn guest_patch_exits_before_write_and_invalidates_the_active_region() {
    let code = assemble(|a| {
        a.mov(dword_ptr(edi), 42).unwrap();
        a.mov(eax, 1).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    jit.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    // MOV [edi],imm32 is six bytes; patch the following MOV's immediate.
    reset(&mut jit, [0, 0, 0, 0, 0, 0, 0, CODE + 7], 2);
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(jit.state.regs[0], 42);
    assert_eq!(jit.memory().read(CODE + 7, 4).unwrap(), 42u32.to_le_bytes());
    support::x64_shape!(assert!(jit.stats.invalidations >= 1));
}

#[test]
fn fs_relative_access_agrees_across_native_and_interpreter_execution() {
    let code = assemble(|a| {
        a.mov(eax, dword_ptr(0x30).fs()).unwrap();
        a.bswap(eax).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    for m in [&mut jit, &mut uc] {
        m.state.fs_base = DATA;
        m.write(DATA + 0x30, &0x12345678u32.to_le_bytes()).unwrap();
    }
    equal(&mut jit, &mut uc, 0x8d5);
    assert_eq!(jit.state.regs[0], 0x78563412);
}

#[test]
fn all_conditions_observe_lazy_flags_after_comparison() {
    for branch in 0..16 {
        let code = assemble(|a| {
            let mut yes = a.create_label();
            a.cmp(eax, ecx).unwrap();
            match branch {
                0 => a.je(yes),
                1 => a.jne(yes),
                2 => a.ja(yes),
                3 => a.jae(yes),
                4 => a.jb(yes),
                5 => a.jbe(yes),
                6 => a.jg(yes),
                7 => a.jge(yes),
                8 => a.jl(yes),
                9 => a.jle(yes),
                10 => a.jo(yes),
                11 => a.jno(yes),
                12 => a.js(yes),
                13 => a.jns(yes),
                14 => a.jp(yes),
                _ => a.jnp(yes),
            }
            .unwrap();
            a.mov(edx, 0).unwrap();
            a.ret().unwrap();
            a.set_label(&mut yes).unwrap();
            a.mov(edx, 1).unwrap();
            a.ret().unwrap();
        });
        let mut jit = machine(&code, true);
        let mut uc = machine(&code, false);
        for a in [0, 1, 0x7fffffff, 0x80000000, 0xffffffff] {
            for b in [0, 1, 0x7fffffff, 0x80000000, 0xffffffff] {
                let regs = [a, b, 0, 0, 0, 0, 0, 0];
                reset(&mut jit, regs, 2);
                reset(&mut uc, regs, 2);
                equal(&mut jit, &mut uc, 0x8d5);
            }
        }
        support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
    }
}

#[test]
fn an_indirect_target_is_reloaded_after_a_pointer_patch() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut first = a.create_label();
    let mut second = a.create_label();
    a.jmp(dword_ptr(edi)).unwrap();
    a.set_label(&mut first).unwrap();
    a.mov(eax, 17).unwrap();
    a.ret().unwrap();
    a.set_label(&mut second).unwrap();
    a.mov(eax, 29).unwrap();
    a.ret().unwrap();
    let result = a
        .assemble_options(
            u64::from(CODE),
            iced_x86::BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let targets = [
        result.label_ip(&first).unwrap() as u32,
        result.label_ip(&second).unwrap() as u32,
    ];
    let mut jit = machine(&result.inner.code_buffer, true);
    for (target, expected) in targets.into_iter().zip([17, 29]) {
        jit.write(DATA, &target.to_le_bytes()).unwrap();
        reset(&mut jit, [0, 0, 0, 0, 0, 0, 0, DATA], 2);
        assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(jit.state.regs[0], expected);
    }
    support::x64_shape!(assert_eq!(jit.stats.compilations, 1));
    support::x64_shape!(assert_eq!(jit.stats.native_entries, 2));
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

#[test]
fn interpreted_floating_state_survives_native_islands() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.fld1().unwrap();
        a.set_label(&mut top).unwrap();
        a.inc(eax).unwrap();
        a.fld1().unwrap();
        a.faddp(st1, st0).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.fstp(dword_ptr(edi)).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    let regs = [0, 9, 0, 0, 0, 0, 0, DATA];
    reset(&mut jit, regs, 2);
    reset(&mut uc, regs, 2);
    equal(&mut jit, &mut uc, 0x8d5);
    assert_eq!(jit.memory().read(DATA, 4).unwrap(), 10f32.to_le_bytes());
    // The loop's x87 instructions stay inside the native region.
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

#[test]
fn interpreted_sse_state_survives_native_islands() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.pxor(xmm0, xmm0).unwrap();
        a.set_label(&mut top).unwrap();
        a.inc(eax).unwrap();
        a.paddd(xmm0, xmmword_ptr(esi)).unwrap();
        a.pause().unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    for m in [&mut jit, &mut uc] {
        reset(m, [0, 9, 0, 0, 0, 0, DATA, DATA + 32], 2);
        m.write(
            DATA,
            &[1u32, 2, 3, 4]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    }
    equal(&mut jit, &mut uc, 0x8d5);
    assert_eq!(jit.memory().read(DATA + 32, 4).unwrap(), 9u32.to_le_bytes());
    support::x64_shape!(assert!(jit.stats.native_entries >= 9));
}

#[test]
fn tiny_budget_can_advance_a_cold_entry() {
    let code = assemble(|a| {
        a.pause().unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(jit.state.eip, CODE + 2);
    assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::ReachedStop);
}

#[test]
fn accesses_outside_the_window_exit_without_touching_host_memory() {
    let code = assemble(|a| {
        a.mov(eax, dword_ptr(edi)).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    // A read straddling the window end faults at the end.
    for (start, address) in [
        (0, 0),
        (CODE - 1, CODE - 1),
        (0x3fffd, 0x40000),
        (0xfffffffc, 0xfffffffc),
        (0xffffffff, 0xffffffff),
    ] {
        reset(&mut jit, [42, 0, 0, 0, 0, 0, 0, start], 2);
        assert_eq!(
            jit.run(STOP, 100).unwrap(),
            RunOutcome::ReadFault { pc: CODE, address }
        );
        assert_eq!(jit.state.regs[0], 42);
    }
}

#[test]
fn bytecode_loads_sign_extension_and_partial_registers_match() {
    let code = assemble(|a| {
        a.mov(al, byte_ptr(esi)).unwrap();
        a.mov(ah, byte_ptr(esi + 1)).unwrap();
        a.mov(bx, word_ptr(esi + 2)).unwrap();
        a.mov(byte_ptr(edi), ah).unwrap();
        a.mov(word_ptr(edi + 1), bx).unwrap();
        a.movzx(edx, ah).unwrap();
        a.movsx(ecx, bx).unwrap();
        a.movsx(ebp, byte_ptr(esi)).unwrap();
        a.movzx(ebx, word_ptr(esi + 2)).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    for data in [
        [0, 0, 0, 0],
        [0xff, 0x80, 0xfe, 0xff],
        [0x80, 0x7f, 0, 0x80],
    ] {
        for m in [&mut jit, &mut uc] {
            reset(
                m,
                [0x12345678, 0, 0, 0x87654321, 0, 0, DATA, DATA + 32],
                0x8d7,
            );
            m.write(DATA, &data).unwrap();
        }
        equal(&mut jit, &mut uc, 0x8d5);
    }
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

#[test]
fn writable_code_makes_progress_with_one_unit_scheduler_slices() {
    let code = assemble(|a| {
        a.mov(dword_ptr(edi), 42).unwrap();
        a.mov(eax, 1).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    jit.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    reset(&mut jit, [0, 0, 0, 0, 0, 0, 0, CODE + 7], 2);
    let mut outcome = RunOutcome::Budget;
    for _ in 0..12 {
        outcome = jit.run(STOP, 1).unwrap();
        if outcome == RunOutcome::ReachedStop {
            break;
        }
    }
    assert_eq!(outcome, RunOutcome::ReachedStop);
    assert_eq!(jit.state.regs[0], 42);
}

#[test]
fn a_modified_return_address_misses_the_native_return_hint() {
    let code = assemble(|a| {
        let mut sub = a.create_label();
        a.call(sub).unwrap();
        a.mov(eax, 99).unwrap();
        a.ret().unwrap();
        a.set_label(&mut sub).unwrap();
        a.mov(dword_ptr(esp), STOP).unwrap();
        a.mov(eax, 17).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    let mut uc = machine(&code, false);
    equal(&mut jit, &mut uc, 0x8d5);
    assert_eq!(jit.state.regs[0], 17);
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

#[test]
fn integer_sse2_lanes_match_unicorn_including_saturation_and_packing() {
    for op in 0..25 {
        let code = assemble(|a| {
            a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
            a.movdqu(xmm1, xmmword_ptr(esi + 16)).unwrap();
            match op {
                0 => a.pavgb(xmm0, xmm1),
                1 => a.pavgw(xmm0, xmm1),
                2 => a.pxor(xmm0, xmm1),
                3 => a.pand(xmm0, xmm1),
                4 => a.por(xmm0, xmm1),
                5 => a.pandn(xmm0, xmm1),
                6 => a.paddb(xmm0, xmm1),
                7 => a.paddw(xmm0, xmm1),
                8 => a.paddd(xmm0, xmm1),
                9 => a.psubb(xmm0, xmm1),
                10 => a.psubw(xmm0, xmm1),
                11 => a.psubd(xmm0, xmm1),
                12 => a.paddusb(xmm0, xmm1),
                13 => a.paddusw(xmm0, xmm1),
                14 => a.psubusb(xmm0, xmm1),
                15 => a.psubusw(xmm0, xmm1),
                16 => a.pmullw(xmm0, xmm1),
                17 => a.pmulhuw(xmm0, xmm1),
                18 => a.punpcklbw(xmm0, xmm1),
                19 => a.punpckhbw(xmm0, xmm1),
                20 => a.packuswb(xmm0, xmm1),
                21 => a.packsswb(xmm0, xmm1),
                22 => a.psrlw(xmm0, 8),
                23 => a.psllw(xmm0, 7),
                _ => a.psrld(xmm0, 15),
            }
            .unwrap();
            a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
            a.ret().unwrap();
        });
        let mut jit = machine(&code, true);
        let mut uc = machine(&code, false);
        let mut seed = 0x13579bdfu32;
        for _ in 0..40 {
            let data: Vec<u8> = (0..32)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    seed as u8
                })
                .collect();
            for m in [&mut jit, &mut uc] {
                reset(m, [0, 0, 0, 0, 0, 0, DATA + 1, DATA + 65], 0x8d7);
                m.write(DATA + 1, &data).unwrap();
            }
            equal(&mut jit, &mut uc, 0x8d5);
        }
        support::x64_shape!(assert_eq!(
            jit.stats.interpreter_entries, 0,
            "SSE2 operation {op}"
        ));
    }
}

#[test]
fn sse2_shift_counts_at_and_above_lane_width_zero_the_result() {
    for count in [0, 1, 15, 16, 17, 31, 32, 255] {
        let code = assemble(|a| {
            a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
            a.movdqa(xmm1, xmm0).unwrap();
            a.psrlw(xmm0, count).unwrap();
            a.pslld(xmm1, count).unwrap();
            a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
            a.movdqu(xmmword_ptr(edi + 16), xmm1).unwrap();
            a.ret().unwrap();
        });
        let mut jit = machine(&code, true);
        let mut uc = machine(&code, false);
        for m in [&mut jit, &mut uc] {
            reset(m, [0, 0, 0, 0, 0, 0, DATA, DATA + 64], 2);
            m.write(DATA, &[0xfd; 16]).unwrap();
        }
        equal(&mut jit, &mut uc, 0x8d5);
        support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
    }
}

#[test]
fn vector_store_fault_checks_both_pages_before_writing() {
    let code = assemble(|a| {
        a.movdqu(xmm0, xmmword_ptr(esi)).unwrap();
        a.movdqu(xmmword_ptr(edi), xmm0).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    jit.protect(DATA + 4096, 4096, READ).unwrap();
    jit.write(DATA, &[0xa5; 16]).unwrap();
    reset(&mut jit, [0, 0, 0, 0, 0, 0, DATA, DATA + 4088], 2);
    assert!(
        // Hardware reports the first inaccessible byte and writes nothing.
        matches!(jit.run(STOP, 100).unwrap(), RunOutcome::WriteFault { address, .. } if address == DATA + 4096)
    );
    assert_eq!(jit.memory().read(DATA + 4088, 16).unwrap(), [0; 16]);
    assert_eq!(jit.state.xmm[0], [0xa5; 16]);
}

#[test]
fn cliffs_count_native_fallbacks_by_instruction() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.inc(eax).unwrap();
        a.pause().unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine(&code, true);
    reset(&mut jit, [0, 9, 0, 0, 0, 0, 0, 0], 2);
    assert_eq!(jit.run(STOP, 1_000_000).unwrap(), RunOutcome::ReachedStop);
    let cliffs = &jit.stats.cliffs;
    assert!(cliffs.fallbacks[&iced_x86::Mnemonic::Pause] >= 9);
    let fallback = cliffs.interpreter[&adv_cpu::InterpreterReason::Fallback];
    assert!(fallback.entries >= 9);
    assert!(fallback.instructions >= fallback.entries);
}

#[test]
fn cliffs_attribute_exact_x87_work_to_the_control_word() {
    for (control, inexact, reason) in [
        (0x027f_u16, false, None),
        (0x037f, false, None),
        (0x037f, true, Some(adv_cpu::X87Cliff::Control)),
        (0x077f, false, Some(adv_cpu::X87Cliff::Control)),
        (0x0b7f, false, Some(adv_cpu::X87Cliff::Control)),
        (0x0f7f, false, Some(adv_cpu::X87Cliff::Control)),
        (0x037e, false, Some(adv_cpu::X87Cliff::Control)),
    ] {
        let code = assemble(|a| {
            a.fldcw(word_ptr(DATA)).unwrap();
            a.fld(qword_ptr(DATA + 8)).unwrap();
            a.fdiv(qword_ptr(DATA + 24)).unwrap();
            a.fld(qword_ptr(DATA + 8)).unwrap();
            a.fmul_2(st0, st1).unwrap();
            a.fstp(qword_ptr(DATA + 16)).unwrap();
            a.fstp(st0).unwrap();
            a.ret().unwrap();
        });
        let mut jit = machine(&code, true);
        jit.write(DATA, &control.to_le_bytes()).unwrap();
        jit.write(DATA + 8, &3.0f64.to_le_bytes()).unwrap();
        jit.write(
            DATA + 24,
            &(if inexact { 5.0f64 } else { 3.0f64 }).to_le_bytes(),
        )
        .unwrap();
        assert_eq!(jit.run(STOP, 1_000_000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(
            jit.memory().read(DATA + 16, 8).unwrap(),
            (if inexact { 1.8f64 } else { 3.0f64 }).to_le_bytes()
        );
        let cliffs = &jit.stats.cliffs;
        let exact = cliffs
            .x87_exact
            .iter()
            .filter(|((_, r), _)| Some(*r) == reason);
        assert_eq!(
            exact.count() > 0,
            reason.is_some(),
            "control={control:#x} {cliffs:?}"
        );
        assert!(
            cliffs.x87_exact.keys().all(|(_, r)| Some(*r) == reason),
            "{cliffs:?}"
        );
        let candidates = cliffs.x87_pc64_rows();
        if control == 0x037f && inexact {
            assert!(
                candidates
                    .iter()
                    .any(|(label, n)| label == "Fdiv/Other" && *n > 0)
            );
            assert!(!candidates.iter().any(|(label, _)| label == "Fmul/Exact"));
            assert_eq!(
                candidates.iter().map(|(_, n)| n).sum::<u64>(),
                cliffs.x87_control[&0x033f]
            );
        } else {
            assert!(candidates.is_empty(), "{control:#x}: {candidates:?}");
        }
    }
}

#[test]
#[cfg(not(feature = "test-a64"))] // x64 tier verification
fn tier_verification_checks_new_regions_without_changing_results() {
    let code = assemble(|a| {
        let mut top = a.create_label();
        let mut leaf = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(eax, ecx).unwrap();
        a.mov(dword_ptr(edi), eax).unwrap();
        a.add(edi, 4).unwrap();
        a.call(leaf).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
        a.set_label(&mut leaf).unwrap();
        a.push(ebx).unwrap();
        a.lea(ebx, dword_ptr(eax + eax * 2 + 1)).unwrap();
        a.pop(edx).unwrap();
        a.ret().unwrap();
    });
    let mut results = Vec::new();
    for verify in [false, true] {
        let mut jit = machine(&code, true);
        jit.set_tier_verification(verify.then_some(true));
        reset(&mut jit, [0, 50, 0, 7, 0, 0, 0, DATA], 2);
        assert_eq!(jit.run(STOP, 1_000_000).unwrap(), RunOutcome::ReachedStop);
        let checks = &jit.stats.verification;
        assert_eq!(checks.mismatches, 0, "{checks:?}");
        assert_eq!(checks.verified > 0, verify, "{checks:?}");
        results.push((
            jit.state.regs,
            jit.memory().read(DATA, 256).unwrap().to_vec(),
        ));
    }
    assert_eq!(results[0], results[1]);
}
