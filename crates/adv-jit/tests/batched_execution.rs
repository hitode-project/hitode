//! Regression contracts for the Machine-only predecoded basic-block executor.
use adv_jit::interpreter::{Interpreter, Step};
use adv_jit::{EXEC, Machine, Memory, READ, RunOutcome, WRITE};

const CODE: u32 = 0x10000;
const STOP: u32 = 0x13000;
const STACK: u32 = 0x14f00;
fn machine(code: &[u8]) -> Machine {
    let mut m = Machine::new(CODE, 0x5000).unwrap();
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
    assert_eq!(m.stats.interpreter_instructions, 3);
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
    m.add_region(adv_jit::Region::new(CODE, 2)).unwrap();
    for value in 0..32u32 {
        m.write(CODE + 0x800, &value.to_le_bytes()).unwrap();
        m.state.eip = CODE;
        m.state.regs[4] = STACK;
        assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    }
    assert_eq!(m.state.regs[0], 32);
    assert_eq!(m.stats.compilations, 1);
    assert_eq!(m.stats.invalidations, 0);
    assert_eq!(m.stats.code_revalidations, 32);
    m.write(CODE, &[0x43]).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 32);
    assert_eq!(m.state.regs[3], 1);
    assert_eq!(m.stats.compilations, 2);
    assert_eq!(m.stats.invalidations, 1);
}

#[test]
fn unchanged_bytes_do_not_preserve_native_code_after_exec_is_revoked() {
    let mut m = machine(&[0x40, 0xc3]);
    m.add_region(adv_jit::Region::new(CODE, 2)).unwrap();
    m.protect(CODE, 4096, READ).unwrap();
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::FetchFault {
            pc: CODE,
            address: CODE
        }
    );
    assert_eq!(m.stats.code_revalidations, 0);
    assert_eq!(m.stats.invalidations, 1);
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 1);
}

#[test]
fn data_writes_on_the_code_page_do_not_force_single_step_redecoding() {
    let mut memory = Memory::new(CODE, 4096).unwrap();
    memory.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    memory.write(CODE, &[0x40]).unwrap();
    let mut state = adv_jit::CpuState::default();
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
