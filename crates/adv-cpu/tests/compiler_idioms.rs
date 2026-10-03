mod support;
use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use support::Machine;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn code(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Machine {
    let mut m = Machine::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        region.entries.extend(
            Decoder::with_ip(32, bytes, u64::from(CODE), DecoderOptions::NONE)
                .into_iter()
                .filter(|i| i.mnemonic() == Mnemonic::Leave)
                .map(|i| i.ip32()),
        );
        m.add_region(region).unwrap();
    }
    m
}

fn seed(m: &mut Machine, left: u32, right: u32, frame: u32) {
    m.state.regs = [
        left,
        0xa1b2_c3d4,
        0x9876_5432,
        right,
        STACK,
        frame,
        0x1357_9bdf,
        0x2468_ace0,
    ];
    m.state.eip = CODE;
    m.state.eflags = 0xed7;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn equal_registers_and_flags(jit: &Machine, uc: &Machine) {
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(jit.state.eip, uc.state.eip);
    assert_eq!(jit.state.eflags, uc.state.eflags);
}

#[test]
fn leave32_restores_aligned_and_unaligned_frames_without_changing_lazy_flags() {
    let bytes = code(|a| {
        a.add(eax, ebx).unwrap();
        a.leave().unwrap();
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let mut uc = machine(&bytes, None);
        for frame in [DATA + 3, DATA + 64, DATA + 4094] {
            for (left, right) in [(0, 0), (u32::MAX, 1), (0x7fff_ffff, 1)] {
                for m in [&mut jit, &mut uc] {
                    seed(m, left, right, frame);
                    m.write(
                        frame,
                        &[0xdead_beefu32.to_le_bytes(), STOP.to_le_bytes()].concat(),
                    )
                    .unwrap();
                }
                assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                equal_registers_and_flags(&jit, &uc);
                assert_eq!(jit.state.regs[0], left.wrapping_add(right));
                assert_eq!(jit.state.regs[4], frame + 8);
                assert_eq!(jit.state.regs[5], 0xdead_beef);
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0);
    }
}

#[test]
fn leave32_read_fault_commits_neither_stack_register_and_restarts_after_repair() {
    let bytes = code(|a| {
        a.add(eax, ebx).unwrap();
        a.leave().unwrap();
        a.ret().unwrap();
    });
    let leave_pc = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .find(|i| i.mnemonic() == Mnemonic::Leave)
        .unwrap()
        .ip32();
    let mut references = Vec::new();
    for (frame, blocked) in [(DATA + 4099, DATA + 4096), (DATA + 4094, DATA + 4096)] {
        // A wholly denied frame read observes Unicorn's fault register ordering.
        // Unicorn 2.1.5 can miss the denied second page of an unaligned access;
        // that case uses its state before LEAVE and an independent range guard.
        let mut reference = machine(&bytes, None);
        seed(&mut reference, 0x7fff_ffff, 1, frame);
        reference.protect(blocked, 4096, 0).unwrap();
        if frame < blocked {
            assert_eq!(
                reference.run_unicorn(leave_pc, 1000).unwrap(),
                RunOutcome::ReachedStop
            );
        } else {
            let outcome = reference.run_unicorn(STOP, 1000).unwrap();
            assert!(
                matches!(outcome, RunOutcome::ReadFault { pc, .. } if pc == leave_pc),
                "frame {frame:#x}: {outcome:?}, registers {:?}",
                reference.state.regs
            );
        }
        assert_eq!(reference.state.regs[4], STACK, "Unicorn LEAVE fault ESP");
        assert_eq!(reference.state.regs[5], frame, "Unicorn LEAVE fault EBP");
        references.push((frame, blocked, reference));
    }
    for (frame, blocked, reference) in references {
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            seed(&mut jit, 0x7fff_ffff, 1, frame);
            jit.protect(blocked, 4096, 0).unwrap();
            assert_eq!(
                jit.run(STOP, 1000).unwrap(),
                RunOutcome::ReadFault {
                    pc: leave_pc,
                    address: frame
                }
            );
            equal_registers_and_flags(&jit, &reference);
            assert_eq!(jit.state.fault_addr, frame);
            assert_eq!(jit.stats.interpreter_entries, 0);
            let mut uc = machine(&bytes, None);
            uc.state = reference.state.clone();
            for m in [&mut jit, &mut uc] {
                m.protect(blocked, 4096, READ | WRITE).unwrap();
                m.write(
                    frame,
                    &[0x1234_5678u32.to_le_bytes(), STOP.to_le_bytes()].concat(),
                )
                .unwrap();
            }
            assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
            equal_registers_and_flags(&jit, &uc);
            assert_eq!(
                jit.state.regs[0], 0x8000_0000,
                "fault repair must not replay ADD"
            );
            assert_eq!(jit.state.regs[4], frame + 8);
            assert_eq!(jit.state.regs[5], 0x1234_5678);
            assert_eq!(jit.stats.interpreter_entries, 0);
        }
    }
}

#[test]
fn cdq_preserves_raw_and_lazy_flags_and_sign_extends_only_eax() {
    for lazy in [false, true] {
        let bytes = code(|a| {
            if lazy {
                a.add(eax, ebx).unwrap();
            }
            a.cdq().unwrap();
            if lazy {
                a.setb(cl).unwrap();
                a.sete(ch).unwrap();
            }
            a.ret().unwrap();
        });
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut uc = machine(&bytes, None);
            for (left, right) in [
                (0, 0),
                (1, 0),
                (0x7fff_ffff, 0),
                (0x8000_0000, 0),
                (u32::MAX, 0),
                (u32::MAX, 1),
                (0x7fff_ffff, 1),
            ] {
                for m in [&mut jit, &mut uc] {
                    seed(m, left, right, DATA + 64);
                }
                assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                equal_registers_and_flags(&jit, &uc);
                let result = if lazy { left.wrapping_add(right) } else { left };
                assert_eq!(jit.state.regs[0], result);
                assert_eq!(jit.state.regs[2], (result as i32 >> 31) as u32);
                if !lazy {
                    assert_eq!(jit.state.eflags, 0xed7);
                }
            }
            assert_eq!(jit.stats.interpreter_entries, 0);
        }
    }
}

#[test]
fn word_leave_and_cwd_match_oracle_operand_size_semantics() {
    for leave in [false, true] {
        let bytes = if leave {
            vec![0x66, 0xc9, 0xc3]
        } else {
            vec![0x66, 0x99, 0xc3]
        };
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut uc = machine(&bytes, None);
            for low in [0u32, 0x7fff, 0x8000, 0xffff] {
                for m in [&mut jit, &mut uc] {
                    seed(m, 0x1234_0000 | low, 0, DATA + 64);
                    if leave {
                        m.write(
                            DATA + 64,
                            &[
                                0xabcd_u16.to_le_bytes().as_slice(),
                                STOP.to_le_bytes().as_slice(),
                            ]
                            .concat(),
                        )
                        .unwrap();
                    }
                }
                assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                equal_registers_and_flags(&jit, &uc);
                if leave {
                    assert_eq!(jit.state.regs[4], DATA + 70);
                    assert_eq!(jit.state.regs[5], 0x0002_abcd);
                } else {
                    assert_eq!(
                        jit.state.regs[2],
                        0x9876_0000 | if low & 0x8000 != 0 { 0xffff } else { 0 }
                    );
                }
            }
            assert!(jit.stats.interpreter_entries > 0);
        }
    }
}
