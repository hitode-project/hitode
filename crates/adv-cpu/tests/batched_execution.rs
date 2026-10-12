//! Regression contracts for the Executor-only predecoded basic-block executor.
use adv_cpu::interpreter::{Interpreter, Step};
use adv_cpu::{EXEC, Executor, Memory, READ, RunOutcome, WRITE};

mod support;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x13000;
const STACK: u32 = 0x14f00;
fn machine(code: &[u8]) -> Executor {
    let mut m = Executor::new(CODE, 0x5000).unwrap();
    m.protect(CODE, 0x5000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    m
}

#[test]
fn arbitrary_fuel_slices_match_single_instruction_reference() {
    let mut code = vec![0x40; 97]; // inc eax
    code.push(0xc3);
    for slice in [1, 2, 7, 31, 32, 33, 97, 98, 256] {
        let mut m = machine(&code);
        let mut memory = Memory::new(CODE, 0x5000).unwrap();
        memory.protect(CODE, 0x5000, READ | WRITE).unwrap();
        memory.write(CODE, &code).unwrap();
        memory.write(STACK, &STOP.to_le_bytes()).unwrap();
        memory.protect(CODE, 4096, READ | EXEC).unwrap();
        let mut state = m.state.clone();
        let mut reference = Interpreter::new();
        loop {
            let outcome = m.run(STOP, slice).unwrap();
            state.fuel = slice;
            while state.fuel != 0 && state.eip != STOP {
                let execution = reference.run_chunk(&mut state, &mut memory, 1);
                assert_eq!(execution.step, Step::Executed);
                state.fuel -= u64::from(execution.units);
            }
            // a64 records indirect branches only when its table misses.
            if cfg!(target_arch = "aarch64") {
                state.last_indirect_branch = m.state.last_indirect_branch;
            }
            assert_eq!(m.state, state, "slice={slice}");
            if outcome == RunOutcome::ReachedStop {
                break;
            }
            assert_eq!(outcome, RunOutcome::Budget);
        }
        assert_eq!(m.state.regs[0], 97);
    }
}

#[test]
fn mixed_x87_mmx_and_sse_aliases_match_public_steps_at_every_fuel_boundary() {
    use iced_x86::code_asm::*;
    let data = CODE + 0x2000;
    let mut a = CodeAssembler::new(32).unwrap();
    a.fld1().unwrap();
    a.fldz().unwrap();
    a.fstp(tbyte_ptr(data as i32)).unwrap();
    a.fld1().unwrap();
    a.fstp(qword_ptr(data as i32 + 16)).unwrap();
    // MM6 aliases a physical x87 register written by the completed prefix.
    a.movq(mm0, mm6).unwrap();
    a.emms().unwrap();
    a.fld1().unwrap();
    a.pxor(xmm0, xmm0).unwrap();
    a.fnstsw(ax).unwrap();
    a.fstp(qword_ptr(data as i32 + 24)).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    for slice in 1..=16 {
        let mut m = machine(&code);
        m.state.xmm = [[0xa5; 16]; 8];
        let mut memory = Memory::new(CODE, 0x5000).unwrap();
        memory.protect(CODE, 0x5000, READ | WRITE).unwrap();
        memory.write(CODE, &code).unwrap();
        memory.write(STACK, &STOP.to_le_bytes()).unwrap();
        memory.protect(CODE, 4096, READ | EXEC).unwrap();
        let mut state = m.state.clone();
        let mut reference = Interpreter::new();
        loop {
            let outcome = m.run(STOP, slice).unwrap();
            state.fuel = slice;
            while state.fuel != 0 && state.eip != STOP {
                let execution = reference.run_chunk(&mut state, &mut memory, 1);
                assert_eq!(execution.step, Step::Executed);
                state.fuel -= u64::from(execution.units);
            }
            // a64 records indirect branches only when its table misses.
            if cfg!(target_arch = "aarch64") {
                state.last_indirect_branch = m.state.last_indirect_branch;
            }
            assert_eq!(m.state, state, "slice={slice}");
            assert_eq!(
                m.memory().read(data, 32).unwrap(),
                memory.read(data, 32).unwrap()
            );
            if outcome == RunOutcome::ReachedStop {
                break;
            }
            assert_eq!(outcome, RunOutcome::Budget);
        }
    }
}

#[test]
fn stop_inside_a_cached_block_is_not_skipped() {
    let mut m = machine(&[0x40, 0x40, 0x40, 0x40, 0xc3]);
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    m.state.eip = CODE;
    m.state.regs[0] = 0;
    assert_eq!(m.run(CODE + 2, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 2);
    assert_eq!(m.state.fuel, 98);
}

#[test]
fn self_modification_of_the_next_instruction_ends_the_batch() {
    // mov byte [CODE+7], 0x43; inc eax (patched to inc ebx); ret
    let mut code = vec![0xc6, 0x05];
    code.extend_from_slice(&(CODE + 7).to_le_bytes());
    code.extend_from_slice(&[0x43, 0x40, 0xc3]);
    let mut m = machine(&code);
    m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 0);
    assert_eq!(m.state.regs[3], 1);
    support::x64_shape!(assert_eq!(m.stats.interpreter_instructions, 3));
}

#[test]
fn faulting_instruction_preserves_the_committed_prefix() {
    // inc eax; mov [unmapped/out-of-window], eax; inc eax
    let address = 0x70000u32;
    let mut code = vec![0x40, 0xa3];
    code.extend_from_slice(&address.to_le_bytes());
    code.push(0x40);
    let mut m = machine(&code);
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::WriteFault {
            pc: CODE + 1,
            address
        }
    );
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.state.eip, CODE + 1);
    assert_eq!(m.state.fuel, 99);
}

#[test]
fn a_negative_tail_dependency_is_redecoded_when_exec_is_granted() {
    let mut m = machine(&[]);
    let pc = CODE + 4094;
    m.write(pc, &[0x40, 0xb8, 7, 0, 0, 0, 0xc3]).unwrap();
    m.state.eip = pc;
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::FetchFault {
            pc: pc + 1,
            address: CODE + 4096
        }
    );
    assert_eq!(m.state.regs[0], 1);
    m.protect(CODE + 4096, 4096, READ | EXEC).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 7);
}

#[test]
fn debug_steps_keep_one_instruction_precision() {
    let mut m = machine(&[0x40, 0x40, 0xc3]);
    m.state.eflags |= 1 << 8;
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.state.eip, CODE + 1);
    assert_eq!(m.state.fuel, 99);
}

#[test]
fn popf_enabling_debug_cannot_hide_the_next_trap() {
    let flags = 0x102u32;
    let mut code = vec![0x68];
    code.extend_from_slice(&flags.to_le_bytes());
    code.extend_from_slice(&[0x9d, 0x40, 0x40, 0xc3]);
    let mut m = machine(&code);
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::Trap {
            pc: CODE + 6,
            vector: 1
        }
    );
    assert_eq!(m.state.regs[0], 1);
}

#[test]
fn same_block_address_in_a_different_machine_has_independent_state() {
    let mut one = machine(&[0xb8, 1, 0, 0, 0, 0xc3]);
    let mut two = machine(&[0xb8, 2, 0, 0, 0, 0xc3]);
    for m in [&mut one, &mut two] {
        assert_eq!(m.run(STOP, 10).unwrap(), RunOutcome::ReachedStop);
    }
    assert_eq!(one.state.regs[0], 1);
    assert_eq!(two.state.regs[0], 2);
}

#[test]
fn unchanged_code_on_a_dirtied_executable_page_keeps_its_native_entry() {
    let mut m = machine(&[0x40, 0xc3]);
    m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    m.add_region(adv_cpu::Region::new(CODE, 2)).unwrap();
    for value in 0..32u32 {
        m.write(CODE + 0x800, &value.to_le_bytes()).unwrap();
        m.state.eip = CODE;
        m.state.regs[4] = STACK;
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    assert_eq!(m.state.regs[0], 32);
    assert_eq!(m.stats.compilations, 1);
    assert_eq!(m.stats.invalidations, 0);
    // Data beside code is not code: no region needs even a byte compare.
    assert_eq!(m.stats.code_revalidations, 0);
    m.write(CODE, &[0x43]).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 32);
    assert_eq!(m.state.regs[3], 1);
    assert_eq!(m.stats.compilations, 2);
    support::x64_shape!(assert_eq!(m.stats.invalidations, 1));
}

#[test]
fn unchanged_bytes_do_not_preserve_native_code_after_exec_is_revoked() {
    let mut m = machine(&[0x40, 0xc3]);
    m.add_region(adv_cpu::Region::new(CODE, 2)).unwrap();
    m.protect(CODE, 4096, READ).unwrap();
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::FetchFault {
            pc: CODE,
            address: CODE
        }
    );
    assert_eq!(m.stats.code_revalidations, 0);
    support::x64_shape!(assert_eq!(m.stats.invalidations, 1));
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 1);
}

#[test]
fn data_writes_on_the_code_page_do_not_force_single_step_redecoding() {
    let mut memory = Memory::new(CODE, 4096).unwrap();
    memory.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    memory.write(CODE, &[0x40]).unwrap();
    let mut state = adv_cpu::CpuState::default();
    let mut interpreter = Interpreter::new();
    for value in 0..100u32 {
        memory.write(CODE + 0x800, &value.to_le_bytes()).unwrap();
        state.eip = CODE;
        assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
    }
    assert_eq!(state.regs[0], 100);
    assert_eq!(interpreter.decode_count(), 1);
    memory.write(CODE, &[0x43]).unwrap();
    state.eip = CODE;
    assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
    assert_eq!(state.regs[0], 100);
    assert_eq!(state.regs[3], 1);
    assert_eq!(interpreter.decode_count(), 2);
}

#[test]
fn native_region_executes_x87_without_leaving_for_the_interpreter() {
    // INC EBX; FLD1; FADD ST0,ST0; FSTP dword [DATA]; MOV EAX,91; RET.
    let data = CODE + 0x2000;
    let mut code = vec![0x43, 0xd9, 0xe8, 0xd8, 0xc0, 0xd9, 0x1d];
    code.extend_from_slice(&data.to_le_bytes());
    code.extend_from_slice(&[0xb8, 91, 0, 0, 0, 0xc3]);
    let mut m = machine(&code);
    let mut region = adv_cpu::Region::new(CODE, code.len());
    region.entries.push(CODE + 11);
    m.add_region(region).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 91);
    assert_eq!(m.state.regs[3], 1);
    assert_eq!(m.memory().read(data, 4).unwrap(), 2.0f32.to_le_bytes());
    support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 0));
    support::x64_shape!(assert_eq!(m.stats.native_entries, 1));
}

#[test]
fn cached_floating_fallthrough_yields_to_new_integer_and_simd_native_entries() {
    for simd in [false, true] {
        // Cache FLD1; FADD ST0,ST0 before publishing its native fallthrough.
        let data = CODE + 0x2000;
        let mut code = vec![0xd9, 0xe8, 0xd8, 0xc0];
        if simd {
            // PXOR XMM0,XMM0; MOVDQU [DATA],XMM0; RET.
            code.extend_from_slice(&[0x66, 0x0f, 0xef, 0xc0, 0xf3, 0x0f, 0x7f, 0x05]);
            code.extend_from_slice(&data.to_le_bytes());
        } else {
            code.extend_from_slice(&[0xb8, 91, 0, 0, 0]);
        }
        code.push(0xc3);
        let mut m = machine(&code);
        assert_eq!(m.run(CODE + 4, 100).unwrap(), RunOutcome::ReachedStop);
        support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 1));
        assert_eq!(m.stats.interpreter_instructions, 2);
        m.add_region(adv_cpu::Region::new(CODE + 4, code.len() - 4))
            .unwrap();
        m.state.eip = CODE;
        m.state.xmm[0] = [0x5a; 16];
        m.write(data, &[0xa5; 16]).unwrap();
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 2, "simd={simd}"));
        support::x64_shape!(assert_eq!(
            m.stats.interpreter_instructions, 4,
            "simd={simd}"
        ));
        support::x64_shape!(assert_eq!(m.stats.native_entries, 1, "simd={simd}"));
        if simd {
            assert_eq!(m.memory().read(data, 16).unwrap(), [0; 16]);
        } else {
            assert_eq!(m.state.regs[0], 91);
        }
    }
}

#[test]
fn cached_floating_prefix_keeps_unmasked_sse_fault_precise() {
    // INC EBX; FLD1; DIVSD XMM0,XMM1; INC EBX; RET.
    let mut m = machine(&[0x43, 0xd9, 0xe8, 0xf2, 0x0f, 0x5e, 0xc1, 0x43, 0xc3]);
    m.state.mxcsr &= !(1 << 9);
    m.state.xmm[0][..8].copy_from_slice(&1.0f64.to_le_bytes());
    let before = m.state.xmm;
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::Trap {
            pc: CODE + 3,
            vector: 19
        }
    );
    assert_eq!(m.state.eip, CODE + 3);
    assert_eq!(m.state.regs[3], 1);
    assert_eq!(m.state.xmm, before);
    assert_eq!(m.state.mxcsr & 4, 4);
    assert_eq!(m.state.fuel, 98);
    support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 1));
    support::x64_shape!(assert_eq!(m.stats.interpreter_instructions, 2));
    m.state.xmm[1][..8].copy_from_slice(&2.0f64.to_le_bytes());
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[3], 2);
    assert_eq!(&m.state.xmm[0][..8], &0.5f64.to_le_bytes());
}

#[test]
fn authored_pcm_mixing_matches_integer_expectations_across_fuel_slices() {
    use iced_x86::code_asm::*;
    let source = CODE + 0x2000;
    let output = source + 0x100;
    let gain = source + 0x200;
    let samples = [-32768i32, -321, -3, -2, -1, 0, 1, 2, 3, 321, 32767];
    let expected: Vec<_> = samples
        .iter()
        .flat_map(|&s| ((s / 2) as i16).to_le_bytes())
        .collect();
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.mov(esi, source).unwrap();
    a.mov(edi, output).unwrap();
    a.mov(ecx, samples.len() as u32).unwrap();
    a.set_label(&mut top).unwrap();
    a.fild(dword_ptr(esi)).unwrap();
    a.fmul(qword_ptr(gain as i32)).unwrap();
    a.fistp(word_ptr(edi)).unwrap();
    a.add(esi, 4).unwrap();
    a.add(edi, 2).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    for native in [false, true] {
        for fuel in [1, 2, 3, 7, 64, 256] {
            let mut m = machine(&code);
            m.write(
                source,
                &samples
                    .iter()
                    .flat_map(|s| s.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
            m.write(gain, &0.5f64.to_le_bytes()).unwrap();
            // Truncate toward zero; the independent integer expectation is s / 2.
            m.state.x87.control = (m.state.x87.control & !0x0c00) | 0x0c00;
            if native {
                let mut region = adv_cpu::Region::new(CODE, code.len());
                // Resume the integer loop tail after the three x87 instructions.
                region.entries.push(CODE + 25);
                m.add_region(region).unwrap();
            }
            let mut complete = false;
            for _ in 0..256 {
                match m.run(STOP, fuel).unwrap() {
                    RunOutcome::ReachedStop => {
                        complete = true;
                        break;
                    }
                    RunOutcome::Budget => {}
                    other => panic!("native={native} fuel={fuel}: {other:?}"),
                }
            }
            assert!(complete, "native={native} fuel={fuel}");
            assert_eq!(
                m.memory().read(output, expected.len()).unwrap(),
                expected,
                "native={native} fuel={fuel}"
            );
            assert_eq!(m.state.regs[1], 0);
            assert_eq!(m.state.regs[6], source + 4 * samples.len() as u32);
            assert_eq!(m.state.regs[7], output + 2 * samples.len() as u32);
            assert_eq!(m.state.x87.tag, 0xffff);
        }
    }
}

#[test]
fn an_exception_raised_inside_cached_x87_cannot_enter_native_mmx() {
    // FLDZ; FLD1; FDIV ST0,ST1; PXOR MM0,MM0; RET.
    let code = [0xd9, 0xee, 0xd9, 0xe8, 0xd8, 0xf1, 0x0f, 0xef, 0xc0, 0xc3];
    let mut m = machine(&code);
    assert_eq!(m.run(CODE + 6, 100).unwrap(), RunOutcome::ReachedStop);
    m.add_region(adv_cpu::Region::new(CODE + 6, code.len() - 6))
        .unwrap();
    m.state.eip = CODE;
    m.state.x87 = adv_cpu::X87State::default();
    m.state.x87.control &= !(1 << 2);
    m.state.refresh_mmx();
    let mut reference = machine(&code);
    reference.state.x87.control &= !(1 << 2);
    assert_eq!(
        reference.run(CODE + 6, 100).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::Trap {
            pc: CODE + 6,
            vector: 16
        }
    );
    assert_eq!(m.state, reference.state);
    assert_eq!(m.state.x87.status & 0x84, 0x84);
    support::x64_shape!(assert_eq!(m.stats.native_entries, 0));
}
