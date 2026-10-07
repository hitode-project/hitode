mod support;

use adv_cpu::backend::{FilePage, Guest, Prot};
use adv_cpu::{CompileStrategy, CpuState, EXEC, JitCpu, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;
const STRATEGIES: [CompileStrategy; 2] = [CompileStrategy::Fast, CompileStrategy::Speed];

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn initial() -> CpuState {
    CpuState {
        eip: CODE,
        regs: [
            0xabcd_8001,
            4,
            0x2244_6688,
            0x3355_7799,
            STACK,
            DATA,
            DATA + 3,
            DATA + 3,
        ],
        eflags: 0xcd7,
        fs_base: DATA,
        mxcsr: 0x5fc3,
        xmm: std::array::from_fn(|reg| {
            std::array::from_fn(|byte| (reg as u8 * 23).wrapping_add(byte as u8 + 1))
        }),
        ..CpuState::default()
    }
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        // Publish exact fault/restart entries, including MASKMOVDQU's implicit
        // MemorySegEDI operand which is not an ordinary OpKind::Memory.
        region.entries.extend(
            Decoder::with_ip(32, bytes, u64::from(CODE), DecoderOptions::NONE)
                .into_iter()
                .filter(|i| {
                    i.op_kinds()
                        .any(|k| matches!(k, OpKind::Memory | OpKind::MemorySegEDI))
                })
                .map(|i| i.ip32()),
        );
        m.add_region(region).unwrap();
    }
    m.state = initial();
    m
}

fn equal_cpu(actual: &CpuState, expected: &CpuState) {
    assert_eq!(actual.regs, expected.regs);
    assert_eq!(actual.eip, expected.eip);
    assert_eq!(actual.eflags, expected.eflags);
    assert_eq!(actual.xmm, expected.xmm);
    assert_eq!(actual.fs_base, expected.fs_base);
    assert_eq!(actual.mxcsr, expected.mxcsr);
    assert_eq!(actual.mmx, expected.mmx);
    assert_eq!(actual.mmx_mode, expected.mmx_mode);
}

fn native_only(m: &Executor) {
    assert_eq!(m.stats.interpreter_entries, 0);
    assert_eq!(m.stats.interpreter_instructions, 0);
}

fn finish(jit: &mut Executor, interpreter: &mut Executor, uc: &mut Executor) {
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(interpreter.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(uc.run_unicorn(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    equal_cpu(&jit.state, &interpreter.state);
    equal_cpu(&jit.state, &uc.state);
    native_only(jit);
}

fn dwords(values: [i32; 4]) -> [u8; 16] {
    let mut out = [0; 16];
    for (lane, value) in out.chunks_exact_mut(4).zip(values) {
        lane.copy_from_slice(&value.to_le_bytes());
    }
    out
}

fn words(values: [i16; 8]) -> [u8; 16] {
    let mut out = [0; 16];
    for (lane, value) in out.chunks_exact_mut(2).zip(values) {
        lane.copy_from_slice(&value.to_le_bytes());
    }
    out
}

#[test]
fn psrad_counts_use_the_full_low_quadword_and_saturate_at_31() {
    let values = [i32::MIN, -0x1234_567, 0x7654_3210, i32::MAX];
    let counts = [0u64, 1, 31, 32, 255, 256, 1 << 32, u64::MAX];
    for strategy in STRATEGIES {
        for form in 0..3 {
            for immediate in if form == 0 {
                &counts[..5]
            } else {
                &counts[..1]
            } {
                let bytes = code(|a| {
                    match form {
                        0 => a.psrad(xmm2, *immediate as i32),
                        1 => a.psrad(xmm2, xmm4),
                        _ => a.psrad(xmm2, xmmword_ptr(DATA as i32)),
                    }
                    .unwrap();
                    a.ret().unwrap();
                });
                let mut jit = machine(&bytes, Some(strategy));
                let mut interpreter = machine(&bytes, None);
                let mut uc = machine(&bytes, None);
                for count in if form == 0 {
                    std::slice::from_ref(immediate)
                } else {
                    &counts
                } {
                    let mut state = initial();
                    state.xmm[2] = dwords(values);
                    state.xmm[4][..8].copy_from_slice(&count.to_le_bytes());
                    state.xmm[4][8..].fill(0xff); // Upper quadword never supplies the count.
                    for m in [&mut jit, &mut interpreter, &mut uc] {
                        m.state = state.clone();
                        m.write(DATA, &state.xmm[4]).unwrap();
                    }
                    finish(&mut jit, &mut interpreter, &mut uc);
                    assert_eq!(
                        jit.state.xmm[2],
                        dwords(values.map(|v| v >> (*count).min(31)))
                    );
                    assert_eq!(jit.state.eflags, state.eflags);
                }
            }
        }
    }
}

#[test]
fn packssdw_and_punpckhwd_keep_both_source_halves_and_saturate_signed_lanes() {
    for strategy in STRATEGIES {
        for operation in 0..2 {
            for form in 0..3 {
                let bytes = code(|a| {
                    match (operation, form) {
                        (0, 0) => a.packssdw(xmm2, xmm4),
                        (0, 1) => a.packssdw(xmm2, xmmword_ptr(DATA as i32)),
                        (0, _) => a.packssdw(xmm2, xmm2),
                        (_, 0) => a.punpckhwd(xmm2, xmm4),
                        (_, 1) => a.punpckhwd(xmm2, xmmword_ptr(DATA as i32)),
                        _ => a.punpckhwd(xmm2, xmm2),
                    }
                    .unwrap();
                    a.ret().unwrap();
                });
                let mut state = initial();
                let expected = if operation == 0 {
                    let left = [i32::MIN, -32769, -32768, 32767];
                    let right = [32768, i32::MAX, -1, 0];
                    state.xmm[2] = dwords(left);
                    state.xmm[4] = dwords(right);
                    let source = if form == 2 { left } else { right };
                    words(std::array::from_fn(|lane| {
                        (if lane < 4 {
                            left[lane]
                        } else {
                            source[lane - 4]
                        })
                        .clamp(i16::MIN as i32, i16::MAX as i32) as i16
                    }))
                } else {
                    let left = [1, 2, 3, 4, -5, i16::MIN, 0x1234, i16::MAX];
                    let right = [11, 12, 13, 14, -15, 0x4321, -17, 18];
                    state.xmm[2] = words(left);
                    state.xmm[4] = words(right);
                    let source = if form == 2 { left } else { right };
                    words(std::array::from_fn(|lane| {
                        if lane % 2 == 0 {
                            left[4 + lane / 2]
                        } else {
                            source[4 + lane / 2]
                        }
                    }))
                };
                let mut jit = machine(&bytes, Some(strategy));
                let mut interpreter = machine(&bytes, None);
                let mut uc = machine(&bytes, None);
                for m in [&mut jit, &mut interpreter, &mut uc] {
                    m.state = state.clone();
                    m.write(DATA, &state.xmm[4]).unwrap();
                }
                finish(&mut jit, &mut interpreter, &mut uc);
                assert_eq!(
                    jit.state.xmm[2], expected,
                    "operation={operation}, form={form}"
                );
            }
        }
    }
}

#[test]
fn pinsrw_masks_the_lane_and_reads_exactly_two_unaligned_or_page_end_bytes() {
    for strategy in STRATEGIES {
        for form in 0..3 {
            for lane in [0i32, 7, 8, 255] {
                let bytes = code(|a| {
                    match form {
                        0 => a.pinsrw(xmm2, eax, lane),
                        1 => a.pinsrw(xmm2, word_ptr(esi).fs(), lane),
                        _ => a.pinsrw(xmm2, word_ptr(esi), lane),
                    }
                    .unwrap();
                    a.cwde().unwrap();
                    a.ret().unwrap();
                });
                let mut state = initial();
                state.regs[0] = match lane {
                    0 => 0xabcd_0000,
                    7 => 0xabcd_7fff,
                    8 => 0xabcd_8000,
                    _ => 0xabcd_ffff,
                };
                state.regs[6] = if form == 1 { 3 } else { DATA + 4094 };
                let at = if form == 1 { DATA + 3 } else { DATA + 4094 };
                let mut jit = machine(&bytes, Some(strategy));
                let mut interpreter = machine(&bytes, None);
                let mut uc = machine(&bytes, None);
                for m in [&mut jit, &mut interpreter, &mut uc] {
                    m.state = state.clone();
                    m.write(at, &0x9876u16.to_le_bytes()).unwrap();
                    // A 16-byte check at the memory operand would wrongly fault.
                    m.protect(DATA + 4096, 4096, 0).unwrap();
                }
                finish(&mut jit, &mut interpreter, &mut uc);
                let mut expected = state.xmm[2];
                let offset = (lane as usize & 7) * 2;
                let inserted = if form == 0 {
                    state.regs[0] as u16
                } else {
                    0x9876
                };
                expected[offset..offset + 2].copy_from_slice(&inserted.to_le_bytes());
                assert_eq!(jit.state.xmm[2], expected);
                assert_eq!(jit.state.regs[0], state.regs[0] as u16 as i16 as i32 as u32);
                assert_eq!(jit.state.eflags, state.eflags);
            }
        }
    }
}

#[test]
fn image_arithmetic_loop_preserves_complete_state_through_short_fuel_restarts() {
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.pinsrw(xmm0, eax, 255).unwrap();
        a.psrad(xmm1, xmm2).unwrap();
        a.packssdw(xmm3, xmm4).unwrap();
        a.punpckhwd(xmm5, xmm6).unwrap();
        a.cwde().unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    for strategy in STRATEGIES {
        for budget in [1, 2, 3] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut uc = machine(&bytes, None);
            let mut state = initial();
            state.xmm[2][..8].copy_from_slice(&u64::MAX.to_le_bytes());
            jit.state = state.clone();
            uc.state = state;
            assert_eq!(jit.run(STOP, 0).unwrap(), RunOutcome::Budget);
            equal_cpu(&jit.state, &uc.state);
            let mut finished = false;
            for _ in 0..32 {
                let outcome = jit.run(STOP, budget).unwrap();
                assert!(jit.state.fuel <= budget);
                for step in 0..128 {
                    if uc.state.eip == jit.state.eip && uc.state.regs[1] == jit.state.regs[1] {
                        break;
                    }
                    assert!(step != 127, "oracle missed the actual native prefix");
                    let expected = uc.run_unicorn(STOP, 1).unwrap();
                    assert!(matches!(
                        expected,
                        RunOutcome::Budget | RunOutcome::ReachedStop
                    ));
                }
                equal_cpu(&jit.state, &uc.state);
                native_only(&jit);
                if outcome == RunOutcome::ReachedStop {
                    finished = true;
                    break;
                }
                assert_eq!(outcome, RunOutcome::Budget);
            }
            assert!(finished);
            assert_eq!(jit.state.regs[0], 0xffff_8001);
            assert_eq!(jit.state.xmm[7], initial().xmm[7]);
        }
    }
}

fn mask_code(use_fs: bool, prefix: bool) -> Vec<u8> {
    let bytes = code(|a| {
        if prefix {
            a.cwde().unwrap();
        }
        a.add_instruction(
            Instruction::with_maskmovdqu(
                32,
                Register::XMM2,
                Register::XMM4,
                if use_fs { Register::FS } else { Register::None },
            )
            .unwrap(),
        )
        .unwrap();
        a.ret().unwrap();
    });
    let masked = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .find(|i| i.mnemonic() == Mnemonic::Maskmovdqu)
        .unwrap();
    assert_eq!(masked.op_count(), 3);
    assert_eq!(masked.op0_kind(), OpKind::MemorySegEDI);
    bytes
}

fn mask_state(at: u32, mask: u16, use_fs: bool) -> CpuState {
    let mut state = initial();
    state.regs[7] = if use_fs { at.wrapping_sub(DATA) } else { at };
    state.xmm[2] = std::array::from_fn(|byte| 0x30 + byte as u8);
    state.xmm[4] = std::array::from_fn(|byte| {
        // Low seven bits must not select a byte, even when they are all set.
        if mask & (1 << byte) != 0 { 0x80 } else { 0x7f }
    });
    state
}

fn masked_expected(initial: &[u8], offset: usize, state: &CpuState) -> Vec<u8> {
    let mut expected = initial.to_vec();
    for byte in 0..16 {
        if state.xmm[4][byte] & 0x80 != 0 {
            expected[offset + byte] = state.xmm[2][byte];
        }
    }
    expected
}

#[test]
fn maskmovdqu_native_zero_full_and_mixed_masks_touch_only_selected_bytes_and_honor_fs() {
    for strategy in STRATEGIES {
        for use_fs in [false, true] {
            let bytes = mask_code(use_fs, false);
            let mut jit = machine(&bytes, Some(strategy));
            let mut interpreter = machine(&bytes, None);
            let mut uc = machine(&bytes, None);
            for at in [DATA + 3, DATA + 4090] {
                for mask in [0u16, u16::MAX, 0x5555, 0x8101] {
                    let state = mask_state(at, mask, use_fs);
                    let before: [u8; 32] = std::array::from_fn(|byte| 0xa0 + byte as u8);
                    for m in [&mut jit, &mut interpreter, &mut uc] {
                        m.state = state.clone();
                        m.write(at - 3, &before).unwrap();
                    }
                    finish(&mut jit, &mut interpreter, &mut uc);
                    let expected = masked_expected(&before, 3, &state);
                    for m in [&jit, &interpreter, &uc] {
                        assert_eq!(m.memory().read(at - 3, 32).unwrap(), expected);
                    }
                    assert_eq!(jit.state.eflags, state.eflags);
                }
            }
        }
    }
}

#[test]
fn masked_protection_fallback_is_transactional_and_zero_mask_never_accesses_memory() {
    let bytes = mask_code(true, true);
    let at = DATA + 4088;
    for strategy in STRATEGIES {
        for mask in [0x00ffu16, 0x0101, u16::MAX] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut interpreter = machine(&bytes, None);
            let state = mask_state(at, mask, true);
            let before = [0xa5; 16];
            for m in [&mut jit, &mut interpreter] {
                m.state = state.clone();
                m.write(at, &before).unwrap();
                m.protect(DATA + 4096, 4096, READ).unwrap();
            }
            let actual = jit.run(STOP, 100).unwrap();
            let expected = interpreter.run(STOP, 100).unwrap();
            assert_eq!(actual, expected, "mask={mask:#x}");
            equal_cpu(&jit.state, &interpreter.state);
            assert!(jit.stats.interpreter_entries > 0);
            if mask == 0x00ff {
                // The protected page is wholly unselected. Full-span native
                // admission falls back; the checked masked write succeeds.
                assert_eq!(actual, RunOutcome::ReachedStop);
                assert_eq!(
                    jit.memory().read(at, 16).unwrap(),
                    masked_expected(&before, 0, &state)
                );
            } else {
                let fault_pc = CODE + 1; // CWDE completed, MASKMOVDQU did not.
                assert_eq!(
                    actual,
                    RunOutcome::WriteFault {
                        pc: fault_pc,
                        address: at + 8
                    }
                );
                assert_eq!(jit.state.fault_addr, at + 8);
                assert_eq!(jit.state.regs[0], 0xffff_8001);
                assert_eq!(jit.memory().read(at, 16).unwrap(), before);
                assert_eq!(interpreter.memory().read(at, 16).unwrap(), before);
                for m in [&mut jit, &mut interpreter] {
                    m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
                }
                let fallbacks = jit.stats.interpreter_entries;
                assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(interpreter.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                equal_cpu(&jit.state, &interpreter.state);
                assert_eq!(jit.stats.interpreter_entries, fallbacks);
                assert_eq!(
                    jit.memory().read(at, 16).unwrap(),
                    masked_expected(&before, 0, &state)
                );
            }
        }
        let mut jit = machine(&bytes, Some(strategy));
        let mut interpreter = machine(&bytes, None);
        // The computed FS:EDI address is outside the allocation, with its
        // sixteen-byte end also wrapping. No address check is legal for mask=0.
        let state = mask_state(0xffff_fff8, 0, true);
        jit.state = state.clone();
        interpreter.state = state;
        assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(interpreter.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        equal_cpu(&jit.state, &interpreter.state);
        native_only(&jit);

        let mut jit = machine(&bytes, Some(strategy));
        let mut interpreter = machine(&bytes, None);
        let state = mask_state(DATA + 3, 0x8101, true);
        let before = [0xa5; 32];
        for m in [&mut jit, &mut interpreter] {
            m.state = state.clone();
            m.write(DATA, &before).unwrap();
            m.protect(DATA, 4096, READ | WRITE | EXEC).unwrap();
        }
        assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(interpreter.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        equal_cpu(&jit.state, &interpreter.state);
        assert!(jit.stats.interpreter_entries > 0);
        assert_eq!(
            jit.memory().read(DATA, 32).unwrap(),
            masked_expected(&before, 3, &state)
        );
    }
}

#[test]
fn maskmovdqu_shared_file_page_keeps_checked_masked_write_semantics() {
    let bytes = mask_code(false, false);
    let before = [0xa5; 32];
    let state = mask_state(DATA + 3, 0x8101, false);
    for strategy in STRATEGIES {
        let mut machines = Vec::new();
        for native in [true, false] {
            let mut cpu = JitCpu::new(STOP, false).unwrap();
            cpu.map(CODE, 4096, Prot::RX).unwrap();
            cpu.map(STOP, 4096, Prot::RX).unwrap();
            cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
            let mut page = [0; 4096];
            page[..32].copy_from_slice(&before);
            cpu.map_file_pages(DATA, &[FilePage::new(page)], Prot::RW, false)
                .unwrap();
            cpu.write(CODE, &bytes).unwrap();
            cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
            cpu.executor_mut().state = state.clone();
            if native {
                let mut region = Region::new(CODE, bytes.len());
                region.compile_strategy = strategy;
                cpu.executor_mut().add_region(region).unwrap();
            }
            assert_eq!(
                cpu.executor_mut().run(STOP, 100).unwrap(),
                RunOutcome::ReachedStop
            );
            assert!(cpu.stats().interpreter_entries > 0);
            let mut actual = [0; 32];
            cpu.read(DATA, &mut actual).unwrap();
            assert_eq!(actual.as_slice(), masked_expected(&before, 3, &state));
            machines.push(cpu);
        }
        let expected = machines[1].executor_mut().state.clone();
        equal_cpu(&machines[0].executor_mut().state, &expected);
    }
}
