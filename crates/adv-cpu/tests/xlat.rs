mod support;

use adv_cpu::{
    CpuState, EXEC, Executor, Memory, READ, Region, RunOutcome, WRITE,
    interpreter::{Interpreter, Step},
};
use iced_x86::Code;
use support::Oracle;

const CODE: u32 = 0x40000;
const DATA: u32 = 0x20000;
const LENGTH: usize = 0x50000;

fn state() -> CpuState {
    CpuState {
        regs: [
            0x1234_5678,
            0x89ab_cdef,
            0xabcd_3210,
            DATA,
            0x48000,
            5,
            6,
            7,
        ],
        eip: CODE,
        eflags: 0xed7,
        xmm: std::array::from_fn(|r| std::array::from_fn(|b| (r * 31 + b * 7) as u8)),
        mxcsr: 0x3f80,
        fuel: 59,
        fault_addr: 0x1234_5678,
        ..CpuState::default()
    }
}

fn memory(code: &[u8]) -> Memory {
    let mut memory = Memory::new(0, LENGTH).unwrap();
    memory.protect(0, LENGTH, READ | WRITE).unwrap();
    memory.write(CODE, code).unwrap();
    memory.protect(CODE, 4096, READ | EXEC).unwrap();
    memory
}

#[test]
fn unsigned_al_address_size_and_flat_segment_overrides_match_unicorn() {
    // Every AL value, both address sizes, all supported segment overrides and
    // an ignored operand-size prefix. The 16-bit table straddles offset 0xffff.
    for segment in [
        None,
        Some(0x2e),
        Some(0x3e),
        Some(0x26),
        Some(0x36),
        Some(0x64),
    ] {
        for word in [false, true] {
            let mut code = vec![0x66];
            code.extend(segment);
            if word {
                code.push(0x67);
            }
            code.push(0xd7);
            let mut memory = memory(&code);
            let mut oracle = Oracle::new(0, LENGTH).unwrap();
            oracle.protect(0, LENGTH, READ | WRITE).unwrap();
            oracle.write(CODE, &code).unwrap();
            oracle.protect(CODE, 4096, READ | EXEC).unwrap();
            let mut initial = state();
            initial.fs_base = 0x10000;
            initial.regs[3] = if word { 0xa5a5_ff80 } else { DATA + 4096 - 128 };
            let segment_base = if segment == Some(0x64) {
                initial.fs_base
            } else {
                0
            };
            for index in 0..=255u32 {
                let offset = initial.regs[3].wrapping_add(index);
                let offset = if word { offset & 0xffff } else { offset };
                let address = offset.wrapping_add(segment_base);
                let value = (index as u8).reverse_bits() ^ 0x5a;
                memory.write(address, &[value]).unwrap();
                oracle.write(address, &[value]).unwrap();
            }
            let mut interpreter = Interpreter::new();
            for index in 0..=255u32 {
                initial.regs[0] = 0x1234_5600 | index;
                let mut actual = initial.clone();
                let execution = interpreter.run_chunk(&mut actual, &mut memory, 1);
                assert_eq!(execution.step, Step::Executed, "{code:x?}, AL={index}");
                assert_eq!(execution.units, 1);
                let mut expected = initial.clone();
                expected.regs[0] = 0x1234_5600 | u32::from((index as u8).reverse_bits() ^ 0x5a);
                expected.eip += code.len() as u32;
                assert_eq!(actual, expected, "{code:x?}, AL={index}");
                oracle.state = initial.clone();
                assert_eq!(
                    oracle.run_unicorn(expected.eip, 1).unwrap(),
                    RunOutcome::ReachedStop,
                    "{code:x?}, AL={index}"
                );
                assert_eq!(actual.regs, oracle.state.regs, "{code:x?}, AL={index}");
                assert_eq!(actual.eflags, oracle.state.eflags);
                assert_eq!(actual.eip, oracle.state.eip);
            }
        }
    }
}

#[test]
fn address32_and_fs_base_addition_wrap_at_32_bits() {
    for (code, base, fs_base) in [
        (vec![0xd7], 0xffff_ff80, 0),
        (vec![0x64, 0xd7], 0x100, 0xffff_fe80),
    ] {
        let mut memory = memory(&code);
        memory.write(0x7f, &[0xb2]).unwrap();
        let mut initial = state();
        initial.regs[0] = 0x1234_56ff;
        initial.regs[3] = base;
        initial.fs_base = fs_base;
        let mut actual = initial.clone();
        assert_eq!(
            Interpreter::new().step(&mut actual, &mut memory),
            Step::Executed
        );
        initial.regs[0] = 0x1234_56b2;
        initial.eip += code.len() as u32;
        assert_eq!(actual, initial);
    }
}

#[test]
fn read_fault_preserves_full_state_and_can_retry_after_mapping_or_protection() {
    for unmapped in [false, true] {
        for word in [false, true] {
            for fs in [false, true] {
                let mut code = Vec::new();
                if fs {
                    code.push(0x64);
                }
                if word {
                    code.push(0x67);
                }
                code.push(0xd7);
                let mut memory = Memory::reserve(0, LENGTH).unwrap();
                memory.map(CODE, 4096, READ | EXEC).unwrap();
                memory.write(CODE, &code).unwrap();
                let address = if fs || !word { DATA + 4096 } else { 4096 };
                if !unmapped {
                    memory.map(address, 4096, WRITE).unwrap();
                    memory.write(address, &[0x6b]).unwrap();
                }
                let mut before = state();
                before.regs[0] = 0x1234_56ff;
                before.fs_base = if fs { DATA } else { 0 };
                before.regs[3] = address - before.fs_base - 255;
                if word {
                    before.regs[3] |= 0xabcd_0000;
                }
                let mut actual = before.clone();
                let mut interpreter = Interpreter::new();
                let execution = interpreter.run_chunk(&mut actual, &mut memory, 1);
                assert_eq!(execution.step, Step::ReadFault { pc: CODE, address });
                assert_eq!(execution.units, 0);
                assert_eq!(actual, before);
                if unmapped {
                    memory.map(address, 4096, READ | WRITE).unwrap();
                    memory.write(address, &[0x6b]).unwrap();
                } else {
                    memory.protect(address, 4096, READ).unwrap();
                }
                assert_eq!(interpreter.step(&mut actual, &mut memory), Step::Executed);
                before.regs[0] = 0x1234_566b;
                before.eip += code.len() as u32;
                assert_eq!(actual, before);
            }
        }
    }
}

#[test]
fn page_end_reads_exactly_one_byte_without_write_permission() {
    let mut memory = Memory::reserve(0, LENGTH).unwrap();
    memory.map(CODE, 4096, READ | EXEC).unwrap();
    memory.write(CODE, &[0xd7]).unwrap();
    memory.map(DATA, 4096, READ).unwrap();
    memory.write(DATA + 4095, &[0xa7]).unwrap();
    let mut actual = state();
    actual.regs[0] = 0x1234_56ff;
    actual.regs[3] = DATA + 4095 - 255;
    assert_eq!(
        Interpreter::new().step(&mut actual, &mut memory),
        Step::Executed
    );
    assert_eq!(actual.regs[0], 0x1234_56a7);
    assert_eq!(actual.eip, CODE + 1);
    assert_eq!(actual.eflags, 0xed7);
}

#[test]
fn single_step_trap_occurs_after_the_byte_and_next_pc_commit() {
    let mut memory = memory(&[0xd7]);
    memory.write(DATA + 255, &[0x6d]).unwrap();
    let mut expected = state();
    expected.regs[0] = 0x1234_56ff;
    expected.eflags |= 0x100;
    let mut actual = expected.clone();
    let execution = Interpreter::new().run_chunk(&mut actual, &mut memory, 1);
    assert_eq!(
        execution.step,
        Step::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(execution.units, 1);
    expected.regs[0] = 0x1234_566d;
    expected.eip += 1;
    assert_eq!(actual, expected);
}

#[test]
fn gs_stays_outside_the_flat_profile_and_lock_is_invalid() {
    for (code, expected) in [
        (
            vec![0x65, 0xd7],
            Step::Unsupported {
                pc: CODE,
                code: Code::Xlat_m8,
            },
        ),
        (vec![0xf0, 0xd7], Step::InvalidInstruction { pc: CODE }),
    ] {
        let mut memory = memory(&code);
        let before = state();
        let mut actual = before.clone();
        assert_eq!(Interpreter::new().step(&mut actual, &mut memory), expected);
        assert_eq!(actual, before);
    }
}

#[test]
fn native_xlat_reads_the_table_between_native_instructions() {
    // MOV EAX, imm; XLAT; MOV ECX, EAX, all native.
    let code = [0xb8, 0xff, 0x56, 0x34, 0x12, 0xd7, 0x89, 0xc1];
    let mut executor = Executor::new(0, LENGTH).unwrap();
    executor.protect(0, LENGTH, READ | WRITE).unwrap();
    executor.protect(CODE, 4096, READ | EXEC).unwrap();
    executor.write(CODE, &code).unwrap();
    executor.write(DATA + 255, &[0xa9]).unwrap();
    let mut region = Region::new(CODE, code.len());
    region.entries.push(CODE + 6);
    executor.add_region(region).unwrap();
    executor.state = state();
    assert_eq!(
        executor.run(CODE + code.len() as u32, 32).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(executor.state.regs[0], 0x1234_56a9);
    assert_eq!(executor.state.regs[1], 0x1234_56a9);
    assert_eq!(executor.state.eflags, 0xed7);
    assert!(executor.stats.native_entries >= 1);
    assert_eq!(executor.stats.interpreter_instructions, 0);
}
