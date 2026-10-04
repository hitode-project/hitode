use adv_cpu::{
    CpuState, EXEC, Executor, Memory, READ, Region, RunOutcome, WRITE,
    interpreter::{Interpreter, Step},
};

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;

#[test]
fn operand_prefixed_word_extensions_preserve_upper_half_and_flags() {
    // Same register and memory encodings are independently checked by the
    // authored WoW64 fixture tools/cpu/mov16-native.c (256 combinations).
    for opcode in [0xb7, 0xbf] {
        for memory_operand in [false, true] {
            let mut code = vec![0x66, 0x0f, opcode, if memory_operand { 0x05 } else { 0xc1 }];
            if memory_operand {
                code.extend(DATA.to_le_bytes());
            }
            for native_body in [false, true] {
                let mut executor = Executor::new(CODE, 0x11000).unwrap();
                executor.protect(CODE, 4096, READ | EXEC).unwrap();
                executor.protect(DATA, 4096, READ | WRITE).unwrap();
                executor.write(CODE, &code).unwrap();
                if native_body {
                    executor.add_region(Region::new(CODE, code.len())).unwrap();
                }
                for destination in [
                    0,
                    1,
                    0x7fff,
                    0x8000,
                    0xffff,
                    0x12348000,
                    0xfeedcafe,
                    u32::MAX,
                ] {
                    for source in [
                        0,
                        1,
                        0x7fff,
                        0x8000,
                        0xffff,
                        0x12348000,
                        0xfeedcafe,
                        u32::MAX,
                    ] {
                        let expected = (destination & 0xffff0000) | (source & 0xffff);
                        executor.state = CpuState {
                            regs: [destination, source, 3, 4, 5, 6, 7, 8],
                            eflags: 0xed7,
                            eip: CODE,
                            ..CpuState::default()
                        };
                        executor.write(DATA, &source.to_le_bytes()).unwrap();
                        assert_eq!(
                            executor.run(CODE + code.len() as u32, 8).unwrap(),
                            RunOutcome::ReachedStop
                        );
                        assert_eq!(executor.state.regs, [expected, source, 3, 4, 5, 6, 7, 8]);
                        assert_eq!(executor.state.eflags, 0xed7);
                    }
                }
            }
        }
    }
}

#[test]
fn word_extensions_check_the_entire_source_before_committing() {
    for opcode in [0xb7, 0xbf] {
        let address = DATA + 4095;
        let mut bytes = vec![0x66, 0x0f, opcode, 0x05];
        bytes.extend(address.to_le_bytes());
        let mut memory = Memory::new(CODE, 0x12000).unwrap();
        memory.protect(CODE, 4096, READ | EXEC).unwrap();
        memory.protect(DATA, 4096, READ | WRITE).unwrap();
        memory.protect(DATA + 4096, 4096, 0).unwrap();
        memory.write(CODE, &bytes).unwrap();
        memory.write(address, &[0xaa]).unwrap();
        let before = CpuState {
            regs: [0xbeef_1234; 8],
            eflags: 0x8d7,
            eip: CODE,
            ..CpuState::default()
        };
        let mut state = before.clone();
        let mut interpreter = Interpreter::new();
        assert!(matches!(
            interpreter.step(&mut state, &mut memory),
            Step::ReadFault { pc: CODE, .. }
        ));
        assert_eq!(state, before);
        memory.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
        memory.write(DATA + 4096, &[0x80]).unwrap();
        assert_eq!(interpreter.step(&mut state, &mut memory), Step::Executed);
        assert_eq!(state.regs[0], 0xbeef80aa);
        assert_eq!(state.eflags, before.eflags);
    }
}

#[test]
fn published_386_divide_quirk_still_traps_on_the_modern_flat_cpu() {
    // Upstream issue #3 documents this 386EX result; the current WoW64 probe
    // confirms #DE. Do not turn arithmetic overflow into a successful quotient.
    let mut memory = Memory::new(CODE, 4096).unwrap();
    memory.protect(CODE, 4096, READ | EXEC).unwrap();
    memory.write(CODE, &[0xf6, 0xf9]).unwrap();
    let mut state = CpuState {
        regs: [0xa1719c71, 0x2ba20547, 3, 4, 5, 6, 7, 8],
        eip: CODE,
        eflags: 0x8d7,
        ..CpuState::default()
    };
    let before = state.clone();
    assert_eq!(
        Interpreter::new().step(&mut state, &mut memory),
        Step::Trap {
            pc: CODE,
            vector: 0
        }
    );
    assert_eq!(state, before);
}
