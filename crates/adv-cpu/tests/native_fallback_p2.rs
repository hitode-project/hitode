mod support;

use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const SRC: u32 = 0x4000;
const DST: u32 = 0x8000;

fn assemble(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    a.inc(ebp).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(0, 0x30000).unwrap();
    m.enable_auto_jit(false);
    m.protect(0, 0x30000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m
}

fn string_code(kind: usize, width: usize, prefix: usize, word: bool) -> Vec<u8> {
    use iced_x86::{Instruction as I, Register as R, RepPrefixKind};
    let address_size = if word { 16 } else { 32 };
    let prefix = [
        RepPrefixKind::None,
        RepPrefixKind::Repe,
        RepPrefixKind::Repne,
    ][prefix];
    let instruction = match (kind, width) {
        (0, 1) => I::with_movsb(address_size, R::None, prefix),
        (0, 2) => I::with_movsw(address_size, R::None, prefix),
        (0, 4) => I::with_movsd(address_size, R::None, prefix),
        (1, 1) => I::with_stosb(address_size, prefix),
        (1, 2) => I::with_stosw(address_size, prefix),
        (1, 4) => I::with_stosd(address_size, prefix),
        (2, 1) => I::with_lodsb(address_size, R::None, prefix),
        (2, 2) => I::with_lodsw(address_size, R::None, prefix),
        (2, 4) => I::with_lodsd(address_size, R::None, prefix),
        _ => unreachable!(),
    }
    .unwrap();
    assemble(|a| a.add_instruction(instruction).unwrap())
}

fn double_shift_code(left: bool, word: bool, dynamic: bool, memory: bool, count: u8) -> Vec<u8> {
    assemble(|a| {
        macro_rules! shift {
            ($dst:expr, $src:expr) => {
                match (left, dynamic) {
                    (false, false) => a.shrd($dst, $src, i32::from(count)),
                    (false, true) => a.shrd($dst, $src, cl),
                    (true, false) => a.shld($dst, $src, i32::from(count)),
                    (true, true) => a.shld($dst, $src, cl),
                }
                .unwrap()
            };
        }
        match (word, memory) {
            (false, false) => shift!(eax, edx),
            (true, false) => shift!(ax, dx),
            (false, true) => shift!(dword_ptr(ebx), edx),
            (true, true) => shift!(word_ptr(ebx), dx),
        }
    })
}

fn branch_code(word: bool) -> Vec<u8> {
    assemble(|a| {
        let mut end = a.create_label();
        if word {
            a.add_instruction(
                iced_x86::Instruction::with_branch(
                    iced_x86::Code::Jcxz_rel8_32,
                    u64::from(CODE + 9),
                )
                .unwrap(),
            )
        } else {
            a.jecxz(end)
        }
        .unwrap();
        a.mov(eax, 7).unwrap();
        a.set_label(&mut end).unwrap();
        a.nop().unwrap();
    })
}

fn compare(
    native: &mut Executor,
    interpreted: &mut Executor,
    oracle: &mut Executor,
    stop: u32,
    mask: u32,
) {
    assert_eq!(native.run(stop, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        interpreted.run(stop, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        oracle.run_unicorn(stop, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    interpreted.state.fuel = native.state.fuel;
    assert_eq!(native.state, interpreted.state);
    assert_eq!(native.state.regs, oracle.state.regs);
    assert_eq!(native.state.eflags & mask, oracle.state.eflags & mask);
    for address in [SRC, DST] {
        assert_eq!(
            native.memory().read(address, 4096).unwrap(),
            interpreted.memory().read(address, 4096).unwrap()
        );
        assert_eq!(
            native.memory().read(address, 4096).unwrap(),
            oracle.memory().read(address, 4096).unwrap()
        );
    }
    assert!(native.stats.native_entries > 0);
    assert_eq!(native.stats.interpreter_entries, 0);
    assert!(
        native.stats.cliffs.fallbacks.is_empty(),
        "{:?}",
        native.stats.cliffs.fallback_rows()
    );
    assert_eq!(native.stats.verification.mismatches, 0);
}

fn initial() -> CpuState {
    CpuState {
        regs: [
            0x89ab_cdef,
            3,
            0x7654_3210,
            DST,
            0x22000,
            0x7fff_ffff,
            SRC + 128,
            DST + 128,
        ],
        eip: CODE,
        eflags: 0xed7,
        ..CpuState::default()
    }
}

#[test]
fn strings_match_both_references_across_widths_prefixes_directions_and_address_sizes() {
    let pattern: Vec<_> = (0..4096).map(|n| (n * 37 + n / 13) as u8).collect();
    for kind in 0..3 {
        for width in [1, 2, 4] {
            for prefix in 0..3 {
                for word in [false, true] {
                    let code = string_code(kind, width, prefix, word);
                    let stop = CODE + code.len() as u32;
                    let mut native = machine(&code, Some(CompileStrategy::Speed));
                    let mut interpreted = machine(&code, None);
                    let mut oracle = machine(&code, None);
                    for backwards in [false, true] {
                        for count in [0, 1, 2, 31, 32] {
                            let mut state = initial();
                            state.eflags = 0xad7 | if backwards { 0x400 } else { 0 };
                            state.regs[1] = count;
                            if prefix != 0 && count == 0 {
                                state.regs[6] = u32::MAX;
                                state.regs[7] = u32::MAX;
                            }
                            if word {
                                state.regs[1] |= 0x1234_0000;
                                state.regs[6] |= 0x5678_0000;
                                state.regs[7] |= 0x9abc_0000;
                            }
                            for m in [&mut native, &mut interpreted, &mut oracle] {
                                m.state = state.clone();
                                m.write(SRC, &pattern).unwrap();
                                m.write(DST, &[0x6d; 4096]).unwrap();
                            }
                            compare(&mut native, &mut interpreted, &mut oracle, stop, u32::MAX);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn double_shifts_match_both_references_for_register_memory_immediate_and_cl_counts() {
    let counts = [0, 1, 2, 15, 16, 17, 31, 32, 33, 255];
    for left in [false, true] {
        for word in [false, true] {
            for memory in [false, true] {
                for encoded in std::iter::once(None).chain(counts.map(Some)) {
                    let code = double_shift_code(
                        left,
                        word,
                        encoded.is_none(),
                        memory,
                        encoded.unwrap_or(0),
                    );
                    let stop = CODE + code.len() as u32;
                    let mut native = machine(&code, Some(CompileStrategy::Speed));
                    let mut interpreted = machine(&code, None);
                    let mut oracle = machine(&code, None);
                    let dynamic = encoded.map_or(counts.to_vec(), |c| vec![c]);
                    for count in dynamic {
                        for value in [0, 1, 0x8000_8000, 0x7fff_7fff, 0xffff_ffff, 0x5555_aaaa] {
                            for flags in [0x202, 0xad7] {
                                let mut state = initial();
                                state.regs[0] = value;
                                state.regs[1] = 0x1234_5600 | u32::from(count);
                                state.regs[2] = value.rotate_left(7) ^ 0xa53c_918e;
                                state.regs[5] = if flags == 0x202 { 0 } else { 0x7fff_ffff };
                                state.eflags = flags;
                                for m in [&mut native, &mut interpreted, &mut oracle] {
                                    m.state = state.clone();
                                    m.write(DST, &value.to_le_bytes()).unwrap();
                                }
                                let masked = count & 31;
                                // Undefined AF and multi-bit OF retain the interpreter's
                                // values. Counts beyond a word have no architectural flags.
                                let mask = if masked == 0 {
                                    u32::MAX
                                } else if word && masked > 16 {
                                    !0x8d5
                                } else if masked == 1 {
                                    !0x10
                                } else {
                                    !0x810
                                };
                                compare(&mut native, &mut interpreted, &mut oracle, stop, mask);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn count_branches_read_cx_or_ecx_and_preserve_lazy_flags() {
    for word in [false, true] {
        let code = branch_code(word);
        let mut native = machine(&code, Some(CompileStrategy::Speed));
        let mut interpreted = machine(&code, None);
        let mut oracle = machine(&code, None);
        for count in [0, 1, 0x10000, 0xabcd_0000, 0xffff_ffff] {
            for flags in [0x202, 0xed7] {
                let mut state = initial();
                state.regs[1] = count;
                state.eflags = flags;
                for m in [&mut native, &mut interpreted, &mut oracle] {
                    m.state = state.clone();
                }
                compare(
                    &mut native,
                    &mut interpreted,
                    &mut oracle,
                    CODE + code.len() as u32,
                    u32::MAX,
                );
                assert_eq!(native.state.regs[1], count);
                assert_eq!(
                    native.state.regs[0],
                    if (if word { count & 0xffff } else { count }) == 0 {
                        state.regs[0]
                    } else {
                        7
                    }
                );
            }
        }
    }
}

#[test]
fn strings_fault_after_exact_committed_prefix_and_resume() {
    for kind in 0..3 {
        for width in [1usize, 2, 4] {
            for prefix in [0, 1, 2] {
                for word in [false, true] {
                    for backwards in [false, true] {
                        for write_fault in [false, true] {
                            if kind == 1 && !write_fault || kind == 2 && write_fault {
                                continue;
                            }
                            let code = string_code(kind, width, prefix, word);
                            let stop = CODE + code.len() as u32;
                            let mut native = machine(&code, Some(CompileStrategy::Speed));
                            let mut interpreted = machine(&code, None);
                            let mut state = initial();
                            let committed = if prefix == 0 { 0 } else { 2 };
                            let offset = if backwards {
                                4096 + committed * width - width
                            } else {
                                4096 - (committed + 1) * width + 1
                            };
                            state.regs[6] = SRC + offset as u32;
                            state.regs[7] = DST + offset as u32;
                            state.regs[1] = 4;
                            state.eflags = 0xad7 | if backwards { 0x400 } else { 0 };
                            if word {
                                for slot in [1, 6, 7] {
                                    state.regs[slot] |= 0x1234_0000;
                                }
                            }
                            let protected = (if write_fault { DST } else { SRC })
                                + if backwards { 0 } else { 4096 };
                            for m in [&mut native, &mut interpreted] {
                                m.state = state.clone();
                                m.write(SRC, &[0x91; 8192]).unwrap();
                                m.write(DST, &[0x6d; 8192]).unwrap();
                                m.protect(protected, 4096, if write_fault { READ } else { 0 })
                                    .unwrap();
                            }
                            let actual = native.run(stop, 1000).unwrap();
                            let expected = interpreted.run(stop, 1000).unwrap();
                            assert_eq!(actual, expected);
                            assert!(matches!(
                                actual,
                                RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                            ));
                            interpreted.state.fuel = native.state.fuel;
                            assert_eq!(native.state, interpreted.state);
                            assert_eq!(native.state.eip, CODE + 1);
                            if prefix != 0 {
                                assert_eq!(native.state.regs[1] & 0xffff, 4 - committed as u32);
                            }
                            assert_eq!(
                                native.memory().read(DST, 8192).unwrap(),
                                interpreted.memory().read(DST, 8192).unwrap()
                            );
                            assert_eq!(native.stats.interpreter_entries, 0);
                            for m in [&mut native, &mut interpreted] {
                                m.protect(protected, 4096, READ | WRITE).unwrap();
                            }
                            assert_eq!(native.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
                            assert_eq!(
                                interpreted.run(stop, 1000).unwrap(),
                                RunOutcome::ReachedStop
                            );
                            interpreted.state.fuel = native.state.fuel;
                            assert_eq!(native.state, interpreted.state);
                            assert_eq!(
                                native.memory().read(DST, 8192).unwrap(),
                                interpreted.memory().read(DST, 8192).unwrap()
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn double_shift_page_faults_preserve_memory_registers_and_flags_including_zero_count() {
    for left in [false, true] {
        for word in [false, true] {
            for count in [0, 1, 31, 32] {
                for permission in [0, READ] {
                    let code = double_shift_code(left, word, true, true, 0);
                    let stop = CODE + code.len() as u32;
                    let mut native = machine(&code, Some(CompileStrategy::Speed));
                    let mut interpreted = machine(&code, None);
                    for m in [&mut native, &mut interpreted] {
                        m.state = initial();
                        m.state.regs[1] = count;
                        m.state.regs[3] = DST + 4095;
                        m.write(DST, &[0x91; 8192]).unwrap();
                        m.protect(DST + 4096, 4096, permission).unwrap();
                    }
                    let actual = native.run(stop, 1000).unwrap();
                    assert_eq!(actual, interpreted.run(stop, 1000).unwrap());
                    assert!(matches!(
                        actual,
                        RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                    ));
                    interpreted.state.fuel = native.state.fuel;
                    assert_eq!(native.state, interpreted.state);
                    assert_eq!(native.memory().read(DST, 8192).unwrap(), &[0x91; 8192]);
                    assert_eq!(native.stats.interpreter_entries, 0);
                    for m in [&mut native, &mut interpreted] {
                        m.protect(DST + 4096, 4096, READ | WRITE).unwrap();
                    }
                    assert_eq!(native.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(
                        interpreted.run(stop, 1000).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    interpreted.state.fuel = native.state.fuel;
                    assert_eq!(native.state, interpreted.state);
                    assert_eq!(
                        native.memory().read(DST, 8192).unwrap(),
                        interpreted.memory().read(DST, 8192).unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn word_indices_wrap_and_fs_applies_only_to_the_source() {
    use iced_x86::{Decoder, DecoderOptions, Register};
    for kind in 0..3 {
        for width in [1, 2, 4] {
            for backwards in [false, true] {
                let base = string_code(kind, width, 1, true);
                let mut instruction = Decoder::new(32, &base, DecoderOptions::NONE)
                    .into_iter()
                    .nth(1)
                    .unwrap();
                instruction.set_segment_prefix(Register::FS);
                let code = assemble(|a| a.add_instruction(instruction).unwrap());
                let mut native = machine(&code, Some(CompileStrategy::Fast));
                let mut interpreted = machine(&code, None);
                let mut oracle = machine(&code, None);
                let mut state = initial();
                state.fs_base = SRC;
                state.regs[1] = 0x1234_0003;
                state.regs[6] = 0xabcd_0000 | if backwards { 0 } else { 0xffff };
                // Avoid wrapping into the code page at the upper 16-bit boundary.
                state.regs[7] = 0xcdef_0000 | if backwards { width as u32 } else { 0xfff0 };
                state.eflags = 0xad7 | if backwards { 0x400 } else { 0 };
                for m in [&mut native, &mut interpreted, &mut oracle] {
                    m.state = state.clone();
                    m.write(SRC, &[0x83; 16]).unwrap();
                    m.write(SRC + 0xfff0, &[0x57; 32]).unwrap();
                }
                compare(
                    &mut native,
                    &mut interpreted,
                    &mut oracle,
                    CODE + code.len() as u32,
                    u32::MAX,
                );
                for address in [0, 0xfff0] {
                    assert_eq!(
                        native.memory().read(address, 16).unwrap(),
                        interpreted.memory().read(address, 16).unwrap()
                    );
                    assert_eq!(
                        native.memory().read(address, 16).unwrap(),
                        oracle.memory().read(address, 16).unwrap()
                    );
                }
            }
        }
    }
}

#[test]
fn rep_element_slices_preserve_progress_across_context_switches_and_interrupts() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    for kind in 0..3 {
        // Address-size 16 selects the element path even for MOVS/STOS.
        let code = string_code(kind, 2, 1, true);
        let stop = CODE + code.len() as u32;
        let mut native = machine(&code, Some(CompileStrategy::Speed));
        // Verification executes its own bounded reference prefix, so test the
        // scheduler's exact one-element shape separately from strict semantics.
        native.set_tier_verification(None);
        let mut interpreted = machine(&code, None);
        let flag = Arc::new(AtomicBool::new(false));
        native.set_host_interrupt_flag(flag.clone());
        native.set_host_interrupts(true);
        for m in [&mut native, &mut interpreted] {
            m.state = initial();
            m.state.regs[1] = 0x1234_001f;
            m.write(SRC, &[0x5d; 4096]).unwrap();
        }
        // One prefix block, then exactly one REP element per unit.
        assert_eq!(native.run(stop, 2).unwrap(), RunOutcome::Budget);
        assert_eq!(native.state.regs[1], 0x1234_001e);
        assert_eq!(native.state.eip, CODE + 1);
        let context = native.save_context().unwrap();
        let prefix = native.state.clone();
        flag.store(true, Ordering::Release);
        assert_eq!(native.run(stop, 10).unwrap(), RunOutcome::Interrupted);
        assert_eq!(native.state.regs, prefix.regs);
        assert_eq!(native.state.eip, prefix.eip);
        flag.store(false, Ordering::Release);
        native.state = CpuState::default();
        native.restore_context(&context).unwrap();
        assert_eq!(native.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(interpreted.run(stop, 100).unwrap(), RunOutcome::ReachedStop);
        interpreted.state.fuel = native.state.fuel;
        assert_eq!(native.state, interpreted.state);
        assert_eq!(
            native.memory().read(DST, 4096).unwrap(),
            interpreted.memory().read(DST, 4096).unwrap()
        );
        assert!(native.stats.cliffs.fallbacks.is_empty());
    }
}

#[test]
fn double_shifts_preserve_aliased_sources_and_word_memory_addressing() {
    for left in [false, true] {
        for word in [false, true] {
            for form in 0..4 {
                let code = assemble(|a| {
                    macro_rules! shift {
                        ($dst:expr, $src:expr) => {
                            if left {
                                a.shld($dst, $src, cl)
                            } else {
                                a.shrd($dst, $src, cl)
                            }
                            .unwrap()
                        };
                    }
                    match (word, form) {
                        (false, 0) => shift!(ecx, edx),
                        (false, 1) => shift!(eax, ecx),
                        (false, 2) => shift!(ecx, ecx),
                        (false, _) => shift!(dword_ptr(bx + si + 7), ecx),
                        (true, 0) => shift!(cx, dx),
                        (true, 1) => shift!(ax, cx),
                        (true, 2) => shift!(cx, cx),
                        (true, _) => shift!(word_ptr(bx + si + 7), cx),
                    }
                    a.seto(al).unwrap();
                    a.setb(dl).unwrap();
                    a.sete(bl).unwrap();
                    a.setp(ah).unwrap();
                });
                let mut native = machine(&code, Some(CompileStrategy::Fast));
                let mut interpreted = machine(&code, None);
                for count in [0, 1, 15, 16, 17, 31, 32] {
                    for m in [&mut native, &mut interpreted] {
                        m.state = initial();
                        m.state.regs[1] = 0xabcd_8700 | count;
                        m.state.regs[3] = 0x1111_8000;
                        m.state.regs[6] = 0x2222_0003;
                        m.write(DST, &[0xa5; 4096]).unwrap();
                    }
                    let stop = CODE + code.len() as u32;
                    assert_eq!(native.run(stop, 1000).unwrap(), RunOutcome::ReachedStop);
                    assert_eq!(
                        interpreted.run(stop, 1000).unwrap(),
                        RunOutcome::ReachedStop
                    );
                    interpreted.state.fuel = native.state.fuel;
                    assert_eq!(native.state, interpreted.state);
                    assert_eq!(
                        native.memory().read(DST, 4096).unwrap(),
                        interpreted.memory().read(DST, 4096).unwrap()
                    );
                    assert_eq!(native.stats.interpreter_entries, 0);
                }
            }
        }
    }
}

#[test]
fn strict_verifier_covers_all_three_native_fallback_groups() {
    let mut cases = vec![branch_code(false), branch_code(true)];
    for kind in 0..3 {
        for prefix in 0..3 {
            for word in [false, true] {
                cases.push(string_code(kind, 2, prefix, word));
            }
        }
    }
    for left in [false, true] {
        for word in [false, true] {
            cases.push(double_shift_code(left, word, true, true, 0));
        }
    }
    let mut verified = 0;
    for code in cases {
        let mut native = machine(&code, Some(CompileStrategy::Speed));
        native.set_tier_verification(Some(true));
        native.state = initial();
        native.write(SRC, &[0x5d; 4096]).unwrap();
        assert_eq!(
            native.run(CODE + code.len() as u32, 1000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(native.stats.verification.mismatches, 0);
        assert!(native.stats.verification.verified > 0);
        assert!(native.stats.cliffs.fallbacks.is_empty());
        verified += native.stats.verification.verified;
    }
    assert!(verified >= 24);
}

#[test]
fn count_branches_with_word_targets_preserve_count_and_flags() {
    use iced_x86::{Code, Instruction};
    const TARGET: u32 = 0x20;
    for word in [false, true] {
        let mut code = assemble(|_| {});
        let mut encoder = iced_x86::Encoder::new(32);
        let instruction = Instruction::with_branch(
            if word {
                Code::Jcxz_rel8_16
            } else {
                Code::Jecxz_rel8_16
            },
            u64::from(TARGET),
        )
        .unwrap();
        encoder
            .encode(&instruction, u64::from(CODE) + code.len() as u64)
            .unwrap();
        code.extend(encoder.take_buffer());
        for count in [0, 1, 0x10000] {
            let mut native = machine(&code, Some(CompileStrategy::Speed));
            let mut interpreted = machine(&code, None);
            let mut oracle = machine(&code, None);
            for m in [&mut native, &mut interpreted, &mut oracle] {
                m.state = initial();
                m.state.regs[1] = count;
            }
            let taken = (if word { count & 0xffff } else { count }) == 0;
            let stop = if taken {
                TARGET
            } else {
                CODE + code.len() as u32
            };
            compare(&mut native, &mut interpreted, &mut oracle, stop, u32::MAX);
        }
    }
}
