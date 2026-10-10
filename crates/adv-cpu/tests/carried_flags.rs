//! Shifts, rotates and multiplies keep the flags they leave undefined. The JIT
//! carries those values lazily; full EFLAGS must still equal the interpreter's
//! after every producer kind, including a loop whose flags state is merged.
mod support;
use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x2f800;

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut r = Region::new(CODE, bytes.len());
        r.compile_strategy = strategy;
        m.add_region(r).unwrap();
    }
    m
}

fn producer(a: &mut CodeAssembler, form: usize) {
    match form {
        0 => Ok(()), // Raw entry flags.
        1 => a.add(eax, ebx),
        2 => a.sub(eax, ebx),
        3 => a.and(eax, ebx),
        4 => a.add(al, bl),
        5 => a.inc(eax),
        _ => a.sub(bx, ax),
    }
    .unwrap();
}

fn consumer(a: &mut CodeAssembler, form: usize) {
    match form {
        0 => a.rol(eax, 1),
        1 => a.rol(eax, 5),
        2 => a.ror(esi, cl),
        3 => a.mul(ebx),
        4 => a.imul_2(esi, ebx),
        5 => a.sar(eax, 3),
        6 => a.shl(esi, 1),
        7 => a.shr(si, cl),
        8 => a.shl(al, 9),
        _ => a.sar(esi, cl),
    }
    .unwrap();
}

#[test]
fn carry_from_mixed_predecessors_survives_partial_writers_and_a_fault() {
    const BAD: u32 = 0x2e000;
    for form in 0..7 {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut raw = a.create_label();
        let mut join = a.create_label();
        // JECXZ selects a predecessor without replacing its incoming flags.
        a.jecxz(raw).unwrap();
        producer(&mut a, form);
        a.jmp(join).unwrap();
        a.set_label(&mut raw).unwrap();
        a.cmc().unwrap();
        a.set_label(&mut join).unwrap();
        a.inc(edi).unwrap();
        a.shl(esi, cl).unwrap(); // Both tested counts mask to zero.
        a.setb(dl).unwrap();
        a.mov(eax, dword_ptr(BAD)).unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            for m in [&mut jit, &mut reference] {
                m.protect(BAD, 4096, 0).unwrap();
            }
            for (left, right) in [
                (0, 1),
                (u32::MAX, 1),
                (0x7fff_ffff, 1),
                (0x8000_0000, u32::MAX),
                (0xff, 1),
            ] {
                for flags in [0x202, 0xad7] {
                    for count in [0, 32] {
                        for m in [&mut jit, &mut reference] {
                            m.state = CpuState {
                                regs: [left, count, 0, right, STACK, 0, left ^ right, 7],
                                eflags: flags,
                                eip: CODE,
                                ..CpuState::default()
                            };
                        }
                        let expected = reference.run(STOP, 100).unwrap();
                        assert!(matches!(
                            expected,
                            RunOutcome::ReadFault { address: BAD, .. }
                        ));
                        assert_eq!(jit.run(STOP, 100).unwrap(), expected);
                        assert_eq!(jit.state.regs, reference.state.regs);
                        assert_eq!(jit.state.eflags, reference.state.eflags);
                        assert_eq!(jit.state.eip, reference.state.eip);
                    }
                }
            }
            support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
        }
    }
}

#[test]
fn carry_controls_preserve_carried_overflow_after_rotate_and_multiply() {
    for operation in [0, 3] {
        let mut a = CodeAssembler::new(32).unwrap();
        a.cmp(eax, ebx).unwrap();
        consumer(&mut a, operation);
        a.cmc().unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            for left in [0, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
                for right in [0, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
                    for flags in [0x202, 0xad7] {
                        for m in [&mut jit, &mut reference] {
                            m.state = CpuState {
                                regs: [left, 0, 0, right, STACK, 0, left ^ right, 0],
                                eflags: flags,
                                eip: CODE,
                                ..CpuState::default()
                            };
                            m.write(STACK, &STOP.to_le_bytes()).unwrap();
                            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                        }
                        // MUL's undefined flags follow the interpreter's
                        // convention, while CMC must preserve all of them.
                        assert_eq!(jit.state.regs, reference.state.regs);
                        assert_eq!(jit.state.eflags, reference.state.eflags);
                    }
                }
            }
            support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
        }
    }
}

#[test]
fn undefined_flags_after_shift_rotate_and_multiply_match_the_interpreter() {
    let values = [0, 1, 0x0f, 0x10, 0x7fff_ffff, 0x8000_0000, 0xffff_ffff];
    for producer_form in 0..7 {
        for first in 0..10 {
            for second in [None, Some(3), Some(1), Some(7)] {
                for looped in [false, true] {
                    let mut a = CodeAssembler::new(32).unwrap();
                    let mut top = a.create_label();
                    a.mov(edi, 3).unwrap();
                    a.set_label(&mut top).unwrap();
                    producer(&mut a, producer_form);
                    consumer(&mut a, first);
                    if let Some(second) = second {
                        consumer(&mut a, second);
                    }
                    if looped {
                        // DEC writes all but CF, then the merged state reenters.
                        a.dec(edi).unwrap();
                        a.jnz(top).unwrap();
                        consumer(&mut a, first);
                    }
                    a.ret().unwrap();
                    let bytes = a.assemble(u64::from(CODE)).unwrap();
                    let mut jit = machine(&bytes, Some(CompileStrategy::Speed));
                    let mut reference = machine(&bytes, None);
                    for &left in &values {
                        for &right in &values {
                            for flags in [0x202, 0xad7] {
                                for count in [0, 1, 7] {
                                    for m in [&mut jit, &mut reference] {
                                        m.state = CpuState {
                                            regs: [
                                                left,
                                                count,
                                                0,
                                                right,
                                                STACK,
                                                0,
                                                left ^ right,
                                                0,
                                            ],
                                            eflags: flags,
                                            eip: CODE,
                                            ..CpuState::default()
                                        };
                                        m.write(STACK, &STOP.to_le_bytes()).unwrap();
                                        assert_eq!(
                                            m.run(STOP, 10_000).unwrap(),
                                            RunOutcome::ReachedStop
                                        );
                                    }
                                    // AF after a logical producer is a separate,
                                    // pre-existing convention difference.
                                    let mask = if producer_form == 3 { !0x10 } else { !0 };
                                    assert_eq!(
                                        (jit.state.regs, jit.state.eflags & mask),
                                        (reference.state.regs, reference.state.eflags & mask),
                                        "producer {producer_form} first {first} second \
                                         {second:?} loop {looped} {left:#x} {right:#x} \
                                         {flags:#x} cl {count}"
                                    );
                                }
                            }
                        }
                    }
                    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
                }
            }
        }
    }
}
