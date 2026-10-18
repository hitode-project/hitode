mod support;

use adv_cpu::{CompileStrategy, CpuState, EXEC, READ, Region, RunOutcome, WRITE, X87Register};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut assembler = CodeAssembler::new(32).unwrap();
    build(&mut assembler);
    assembler.assemble(u64::from(CODE)).unwrap()
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut machine = Executor::new(CODE, 0x20000).unwrap();
    machine.protect(CODE, 0x20000, READ | WRITE).unwrap();
    machine.write(CODE, bytes).unwrap();
    machine.protect(CODE, 4096, READ | EXEC).unwrap();
    machine.protect(STOP, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        machine.add_region(region).unwrap();
    }
    machine
}

fn initial() -> CpuState {
    let mut state = CpuState::default();
    state.eip = CODE;
    state.regs = [
        0x1234_5678,
        17,
        0x9876_5432,
        0x2468_ace0,
        STACK,
        DATA,
        DATA,
        DATA + 64,
    ];
    state.eflags = 0xcd7;
    state.mxcsr = 0x5fc3;
    state.x87.status = 0x4500;
    state.x87.set_top(5);
    state.x87.tag = 0;
    for (slot, register) in state.x87.regs.iter_mut().enumerate() {
        *register = X87Register::from_parts(0x8000_0000_0000_0000 | slot as u64, 0x3fff);
    }
    state.refresh_mmx();
    for (slot, register) in state.xmm.iter_mut().enumerate() {
        *register = std::array::from_fn(|lane| (slot * 17 + lane + 1) as u8);
    }
    state
}

fn start(machine: &mut Executor, state: &CpuState) {
    machine.state = state.clone();
    machine.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn compare(jit: &mut Executor, interpreter: &mut Executor, unicorn: &mut Executor) {
    for machine in [&mut *jit, &mut *interpreter, &mut *unicorn] {
        machine.state.commit_mmx();
    }
    for reference in [&*interpreter, &*unicorn] {
        assert_eq!(jit.state.regs, reference.state.regs);
        assert_eq!(jit.state.eflags, reference.state.eflags);
        assert_eq!(jit.state.eip, reference.state.eip);
        assert_eq!(jit.state.xmm, reference.state.xmm);
        assert_eq!(jit.state.mmx, reference.state.mmx);
        assert_eq!(jit.state.mxcsr, reference.state.mxcsr);
        assert_eq!(
            jit.memory().read(DATA, 128).unwrap(),
            reference.memory().read(DATA, 128).unwrap()
        );
    }
    // Unicorn reconstructs numeric x87 tags after MMX writes. The transactional
    // interpreter retains exact tags and all untouched physical register bits.
    assert_eq!(jit.state.x87, interpreter.state.x87);
    assert_eq!(jit.state.mmx_mode, 0);
}

fn completed(jit: &mut Executor, interpreter: &mut Executor, unicorn: &mut Executor) {
    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        interpreter.run(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        unicorn.run_unicorn(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    compare(jit, interpreter, unicorn);
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

fn words(values: [i16; 8]) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (chunk, value) in bytes.as_chunks_mut::<2>().0.iter_mut().zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[test]
fn mmx_xmm_moves_preserve_aliases_and_clear_the_xmm_high_half_natively() {
    let values = [0, u64::MAX, 0x0123_4567_89ab_cdef, 0x8000_7fff_ffff_0001];
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for operation in 0..4 {
            let bytes = code(|assembler| {
                match operation {
                    0 => assembler.movq2dq(xmm2, mm3).unwrap(),
                    1 => assembler.movdq2q(mm5, xmm4).unwrap(),
                    _ => {
                        assembler.movq(mm3, qword_ptr(DATA as i32)).unwrap();
                        assembler.movq2dq(xmm2, mm3).unwrap();
                        assembler.movdq2q(mm5, xmm2).unwrap();
                    }
                }
                if operation == 3 {
                    assembler.emms().unwrap();
                }
                assembler.ret().unwrap();
            });
            let mut jit = machine(&bytes, Some(strategy));
            let mut interpreter = machine(&bytes, None);
            let mut unicorn = machine(&bytes, None);
            for value in values {
                let mut state = initial();
                state.x87.regs[3] = X87Register::from_parts(value, 0x3fff);
                state.refresh_mmx();
                state.xmm[4][..8].copy_from_slice(&value.to_le_bytes());
                for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                    start(machine, &state);
                    machine.write(DATA, &value.to_le_bytes()).unwrap();
                }
                completed(&mut jit, &mut interpreter, &mut unicorn);
                if operation != 1 {
                    assert_eq!(&jit.state.xmm[2][..8], &value.to_le_bytes());
                    assert_eq!(&jit.state.xmm[2][8..], &[0; 8]);
                }
                if operation != 0 {
                    assert_eq!(jit.state.mmx[5], value);
                    assert_eq!(
                        jit.state.x87.regs[5],
                        X87Register::from_parts(value, 0xffff)
                    );
                }
                if operation == 0 {
                    assert_eq!(jit.state.x87.regs, state.x87.regs);
                }
                assert_eq!(jit.state.x87.top(), 0);
                assert_eq!(jit.state.x87.tag, if operation == 3 { 0xffff } else { 0 });
                assert_eq!(jit.state.xmm[4], state.xmm[4]);
            }
        }
    }
}

#[test]
fn mmx_xmm_moves_survive_native_dispatch_and_emms() {
    const NEXT: u32 = 0x13000;
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        let bytes = code(|assembler| {
            assembler.movdq2q(mm3, xmm4).unwrap();
            assembler.jmp(u64::from(NEXT)).unwrap();
        });
        let next = {
            let mut assembler = CodeAssembler::new(32).unwrap();
            assembler.movq2dq(xmm2, mm3).unwrap();
            assembler.emms().unwrap();
            assembler.ret().unwrap();
            assembler.assemble(u64::from(NEXT)).unwrap()
        };
        let mut jit = machine(&bytes, Some(strategy));
        let mut interpreter = machine(&bytes, None);
        let mut unicorn = machine(&bytes, None);
        // Rust entries count region transfers only while linking is off.
        jit.set_region_linking(false);
        let state = initial();
        for machine in [&mut jit, &mut interpreter, &mut unicorn] {
            machine.write(NEXT, &next).unwrap();
            machine.protect(NEXT, 4096, READ | EXEC).unwrap();
            start(machine, &state);
        }
        let mut region = Region::new(NEXT, next.len());
        region.compile_strategy = strategy;
        jit.add_region(region).unwrap();
        completed(&mut jit, &mut interpreter, &mut unicorn);
        support::x64_shape!(assert!(jit.stats.native_entries >= 2));
        assert_eq!(&jit.state.xmm[2][..8], &state.xmm[4][..8]);
        assert_eq!(&jit.state.xmm[2][8..], &[0; 8]);
        assert_eq!(jit.state.x87.tag, 0xffff);
    }
}

#[test]
fn psraw_xmm_immediate_register_and_memory_counts_match_both_oracles() {
    let values = [
        [i16::MIN, -32767, -1, 0, 1, 123, 32766, i16::MAX],
        [1, -1, 0x1234, -0x1234, 0x4321, -0x4321, 0, i16::MIN],
    ];
    let counts = [
        0,
        1,
        7,
        14,
        15,
        16,
        17,
        31,
        32,
        63,
        64,
        255,
        256,
        1 << 32,
        u64::MAX,
    ];
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for form in 0..3 {
            for immediate in if form == 0 {
                &counts[..12]
            } else {
                &counts[..1]
            } {
                let bytes = code(|assembler| {
                    match form {
                        0 => assembler.psraw(xmm2, *immediate as i32),
                        1 => assembler.psraw(xmm2, xmm4),
                        _ => assembler.psraw(xmm2, xmmword_ptr(DATA as i32)),
                    }
                    .unwrap();
                    assembler.ret().unwrap();
                });
                let mut jit = machine(&bytes, Some(strategy));
                let mut interpreter = machine(&bytes, None);
                let mut unicorn = machine(&bytes, None);
                for count in if form == 0 {
                    std::slice::from_ref(immediate)
                } else {
                    &counts
                } {
                    for values in values {
                        let mut state = initial();
                        state.xmm[2] = words(values);
                        state.xmm[4][..8].copy_from_slice(&count.to_le_bytes());
                        // Only the low 64 bits supply the variable count.
                        state.xmm[4][8..].fill(0xff);
                        for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                            start(machine, &state);
                            machine.write(DATA, &state.xmm[4]).unwrap();
                        }
                        completed(&mut jit, &mut interpreter, &mut unicorn);
                        assert_eq!(jit.state.x87, state.x87);
                        assert_eq!(jit.state.xmm[4], state.xmm[4]);
                    }
                }
            }
        }
    }
}

#[test]
fn punpcklwd_xmm_interleaves_only_the_low_half_for_register_and_memory() {
    let left = words([1, -2, 0x1234, i16::MIN, 101, 102, 103, 104]);
    let right = words([5, -6, 0x4321, i16::MAX, 201, 202, 203, 204]);
    let expected = words([1, 5, -2, -6, 0x1234, 0x4321, i16::MIN, i16::MAX]);
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for form in 0..3 {
            let bytes = code(|assembler| {
                match form {
                    0 => assembler.punpcklwd(xmm2, xmm4),
                    1 => assembler.punpcklwd(xmm2, xmmword_ptr(DATA as i32)),
                    _ => assembler.punpcklwd(xmm2, xmm2),
                }
                .unwrap();
                assembler.ret().unwrap();
            });
            let mut jit = machine(&bytes, Some(strategy));
            let mut interpreter = machine(&bytes, None);
            let mut unicorn = machine(&bytes, None);
            let mut state = initial();
            state.xmm[2] = left;
            state.xmm[4] = right;
            for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                start(machine, &state);
                machine.write(DATA, &right).unwrap();
            }
            completed(&mut jit, &mut interpreter, &mut unicorn);
            assert_eq!(
                jit.state.xmm[2],
                if form == 2 {
                    words([1, 1, -2, -2, 0x1234, 0x1234, i16::MIN, i16::MIN])
                } else {
                    expected
                }
            );
            assert_eq!(jit.state.x87, state.x87);
        }
    }
}

#[test]
fn packed_memory_faults_preserve_the_committed_prefix_and_resume() {
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for shift in [false, true] {
            let bytes = code(|assembler| {
                assembler.mov(eax, 0x1357_2468).unwrap();
                assembler.movdq2q(mm5, xmm4).unwrap();
                if shift {
                    assembler.psraw(xmm2, xmmword_ptr(esi)).unwrap();
                } else {
                    assembler.punpcklwd(xmm2, xmmword_ptr(esi)).unwrap();
                }
                assembler.movdq2q(mm5, xmm2).unwrap();
                assembler.ret().unwrap();
            });
            let mnemonic = if shift {
                Mnemonic::Psraw
            } else {
                Mnemonic::Punpcklwd
            };
            let fault_pc = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
                .into_iter()
                .find(|instruction| instruction.mnemonic() == mnemonic)
                .unwrap()
                .ip32();
            for misaligned in [true, false] {
                let mut jit = machine(&bytes, None);
                let mut region = Region::new(CODE, bytes.len());
                region.compile_strategy = strategy;
                region.entries = vec![CODE, fault_pc];
                jit.add_region(region).unwrap();
                let mut interpreter = machine(&bytes, None);
                let mut unicorn = machine(&bytes, None);
                let mut state = initial();
                state.regs[6] = DATA + if misaligned { 1 } else { 4096 };
                for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                    start(machine, &state);
                    machine.write(DATA + 4096, &[1; 16]).unwrap();
                    if !misaligned {
                        machine.protect(DATA + 4096, 4096, WRITE).unwrap();
                    }
                }
                let expected = if misaligned {
                    RunOutcome::Trap {
                        pc: fault_pc,
                        vector: 13,
                    }
                } else {
                    RunOutcome::ReadFault {
                        pc: fault_pc,
                        address: DATA + 4096,
                    }
                };
                assert_eq!(jit.run(STOP, 10000).unwrap(), expected);
                assert_eq!(interpreter.run(STOP, 10000).unwrap(), expected);
                // Unicorn does not enforce legacy SSE alignment and treats
                // WRITE-only x86 pages as readable. Independently execute the
                // exact prefix there, and use the interpreter for the fault.
                assert_eq!(
                    unicorn.run_unicorn(fault_pc, 10000).unwrap(),
                    RunOutcome::ReachedStop
                );
                compare(&mut jit, &mut interpreter, &mut unicorn);
                assert_eq!(jit.state.xmm[2], state.xmm[2]);
                assert_eq!(
                    jit.state.x87.regs[5].significand(),
                    u64::from_le_bytes(state.xmm[4][..8].try_into().unwrap())
                );
                // Fault guards intentionally hand the offending instruction to
                // the interpreter. The repaired continuation must be all native.
                let fallbacks = jit.stats.interpreter_entries;
                for machine in [&mut jit, &mut interpreter, &mut unicorn] {
                    machine.state.regs[6] = DATA;
                    machine.write(DATA, &[0; 16]).unwrap();
                }
                assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(
                    interpreter.run(STOP, 10000).unwrap(),
                    RunOutcome::ReachedStop
                );
                assert_eq!(
                    unicorn.run_unicorn(STOP, 10000).unwrap(),
                    RunOutcome::ReachedStop
                );
                compare(&mut jit, &mut interpreter, &mut unicorn);
                support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, fallbacks));
            }
        }
    }
}
