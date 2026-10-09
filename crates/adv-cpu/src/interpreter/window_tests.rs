use super::*;
use iced_x86::{BlockEncoderOptions, code_asm::*};

const CODE: u32 = 0x10000;
const TABLE: u32 = 0x20000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x2f000;

fn memory(code: &[u8]) -> Memory {
    let mut m = Memory::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, code).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.protect(CODE, PAGE_SIZE, READ | EXEC).unwrap();
    m
}

fn state() -> CpuState {
    let mut state = CpuState {
        eip: CODE,
        eflags: 0xad7,
        ..Default::default()
    };
    state.regs[1] = 31;
    state.regs[4] = STACK;
    state
}

#[test]
fn local_indirect_and_conditional_branches_match_single_steps_at_every_budget() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut join = a.create_label();
    let mut handlers: Vec<_> = (0..4).map(|_| a.create_label()).collect();
    a.set_label(&mut top).unwrap();
    a.mov(ebx, ecx).unwrap();
    a.and(ebx, 3).unwrap();
    a.jmp(dword_ptr(ebx * 4 + TABLE as i32)).unwrap();
    for (n, label) in handlers.iter_mut().enumerate() {
        a.set_label(label).unwrap();
        a.add(eax, n as i32 + 1).unwrap();
        a.jmp(join).unwrap();
    }
    a.set_label(&mut join).unwrap();
    a.mov(dword_ptr(TABLE + 32), eax).unwrap();
    a.dec(ecx).unwrap();
    a.jne(top).unwrap();
    a.ret().unwrap();
    let code = a
        .assemble_options(
            CODE as u64,
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let table: Vec<_> = handlers
        .iter()
        .flat_map(|l| (code.label_ip(l).unwrap() as u32).to_le_bytes())
        .collect();
    let stops: Vec<_> = Decoder::with_ip(
        32,
        &code.inner.code_buffer,
        CODE as u64,
        DecoderOptions::NONE,
    )
    .into_iter()
    .map(|i| i.ip32())
    .chain([STOP])
    .collect();
    let mut cached = Interpreter::new();
    cached.frame_retired = Some(0);
    for budget in 1..=256 {
        for stop in [STOP, stops[budget as usize % stops.len()]] {
            let mut m = memory(&code.inner.code_buffer);
            let mut reference = memory(&code.inner.code_buffer);
            m.write(TABLE, &table).unwrap();
            reference.write(TABLE, &table).unwrap();
            let mut actual = state();
            let mut expected = state();
            let before = cached.frame_retired.unwrap();
            let (result, last) = cached.run_block(&mut actual, &mut m, budget, stop, None);
            assert_eq!(result.step, Step::Executed);
            assert!(result.units <= budget);
            assert_eq!(last, u32::from(result.units != 0));
            assert_eq!(
                cached.frame_retired.unwrap() - before,
                u64::from(result.units)
            );
            let mut scalar = Interpreter::new();
            for _ in 0..result.units {
                assert_ne!(expected.eip, stop);
                assert_eq!(scalar.step(&mut expected, &mut reference), Step::Executed);
            }
            assert_eq!(actual, expected, "budget {budget}");
            assert_eq!(
                m.read_data(TABLE, 36).unwrap(),
                reference.read_data(TABLE, 36).unwrap()
            );
        }
    }
}

#[test]
fn local_backedges_yield_to_native_publication_stop_and_tf() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.inc(eax).unwrap();
    a.jmp(top).unwrap();
    let code = a.assemble(CODE as u64).unwrap();
    let mut m = memory(&code);
    let mut interpreter = Interpreter::new();
    let mut s = state();
    assert_eq!(
        interpreter
            .run_block(&mut s, &mut m, 256, STOP, None)
            .0
            .units,
        192
    );
    assert_eq!(s.regs[0], 96);
    assert_eq!(s.eip, CODE);
    let native = BTreeMap::from([(CODE, CODE)]);
    let result = interpreter
        .run_block(&mut s, &mut m, 256, STOP, Some((&native, 1)))
        .0;
    assert_eq!(result.units, 2);
    assert_eq!(s.regs[0], 97);
    let result = interpreter.run_block(&mut s, &mut m, 256, CODE + 1, None).0;
    assert_eq!(result.units, 1);
    assert_eq!(s.eip, CODE + 1);
    s.eip = CODE;
    s.eflags |= 0x100;
    let result = interpreter.run_block(&mut s, &mut m, 256, STOP, None).0;
    assert_eq!(result.units, 1);
    assert_eq!(
        result.step,
        Step::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(s.eip, CODE + 1);
}

#[test]
fn a_local_window_commits_only_the_prefix_before_fault_or_code_write() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.inc(eax).unwrap();
    a.mov(ebx, dword_ptr(esi)).unwrap();
    a.add(esi, 4).unwrap();
    a.jmp(top).unwrap();
    let bytes = a.assemble(CODE as u64).unwrap();
    let mut m = memory(&bytes);
    m.write(0x21ffc, &17u32.to_le_bytes()).unwrap();
    m.protect(0x22000, PAGE_SIZE, 0).unwrap();
    let mut s = state();
    s.regs[6] = 0x21ffc;
    let mut interpreter = Interpreter::new();
    let result = interpreter.run_block(&mut s, &mut m, 256, STOP, None).0;
    assert_eq!(result.units, 5);
    assert_eq!(
        result.step,
        Step::ReadFault {
            pc: CODE + 1,
            address: 0x22000
        }
    );
    assert_eq!(s.regs[0], 2);
    assert_eq!(s.regs[3], 17);
    m.protect(0x22000, PAGE_SIZE, READ).unwrap();
    assert_eq!(
        interpreter.run_block(&mut s, &mut m, 1, STOP, None).0.units,
        1
    );
    assert_eq!(s.regs[0], 2);

    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    // The authored byte store patches the following MOV's immediate.
    a.mov(byte_ptr(CODE + 8), 9).unwrap();
    a.mov(eax, 7).unwrap();
    a.jmp(top).unwrap();
    let mut m = memory(&a.assemble(CODE as u64).unwrap());
    m.protect(CODE, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
    let mut s = state();
    let result = interpreter.run_block(&mut s, &mut m, 256, STOP, None).0;
    assert_eq!(result.units, 1);
    assert_eq!(s.regs[0], 0);
    assert_eq!(
        interpreter.run_block(&mut s, &mut m, 1, STOP, None).0.units,
        1
    );
    assert_eq!(s.regs[0], 9);
}

#[test]
fn shared_code_keeps_transfer_revalidation_boundaries() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.mov(eax, 7).unwrap();
    a.jmp(top).unwrap();
    let mut m = Memory::reserve(CODE, PAGE_SIZE).unwrap();
    let storage = m.map_shared_audio(CODE, PAGE_SIZE).unwrap();
    storage.store_bytes(0, &a.assemble(CODE as u64).unwrap());
    m.protect(CODE, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
    let mut s = state();
    let mut interpreter = Interpreter::new();
    assert_eq!(
        interpreter
            .run_block(&mut s, &mut m, 256, STOP, None)
            .0
            .units,
        2
    );
    assert_eq!(s.regs[0], 7);
    storage.store_bytes(1, &9u32.to_le_bytes());
    assert_eq!(
        interpreter
            .run_block(&mut s, &mut m, 256, STOP, None)
            .0
            .units,
        2
    );
    assert_eq!(s.regs[0], 9);
}

#[test]
fn local_calls_returns_and_stack_faults_match_single_steps() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    let mut leaf = a.create_label();
    let mut nested = a.create_label();
    a.set_label(&mut top).unwrap();
    a.push(ecx).unwrap();
    a.call(leaf).unwrap();
    a.dec(ecx).unwrap();
    a.jne(top).unwrap();
    a.ret().unwrap();
    a.set_label(&mut leaf).unwrap();
    a.push(ebx).unwrap();
    a.call(nested).unwrap();
    a.pop(ebx).unwrap();
    a.ret_1(4).unwrap();
    a.set_label(&mut nested).unwrap();
    a.add(eax, 7).unwrap();
    a.ret().unwrap();
    let code = a
        .assemble_options(
            CODE as u64,
            BlockEncoderOptions::RETURN_NEW_INSTRUCTION_OFFSETS,
        )
        .unwrap();
    let leaf_pc = code.label_ip(&leaf).unwrap() as u32;
    let nested_pc = code.label_ip(&nested).unwrap() as u32;
    for budget in 1..=256 {
        for stack in [STACK, 0x20008, 0x20002] {
            let mut m = memory(&code.inner.code_buffer);
            let mut reference = memory(&code.inner.code_buffer);
            for m in [&mut m, &mut reference] {
                m.write(stack, &STOP.to_le_bytes()).unwrap();
                m.protect(0x1f000, PAGE_SIZE, 0).unwrap();
            }
            let mut actual = state();
            actual.regs[4] = stack;
            let mut expected = actual.clone();
            let mut window = Interpreter::new();
            let result = window.run_block(&mut actual, &mut m, budget, STOP, None).0;
            let mut scalar = Interpreter::new();
            for _ in 0..result.units {
                assert_ne!(expected.eip, STOP);
                assert_eq!(scalar.step(&mut expected, &mut reference), Step::Executed);
            }
            if result.step != Step::Executed {
                assert_eq!(scalar.step(&mut expected, &mut reference), result.step);
            }
            assert_eq!(actual, expected, "budget {budget}, stack {stack}");
            assert_eq!(
                m.read_data(0x20000, 0x10000).unwrap(),
                reference.read_data(0x20000, 0x10000).unwrap()
            );
            if budget == 256 && stack == STACK {
                assert!(result.units >= 192);
            }
        }
    }
    // Discovery/tracing and newly published native callees keep owner exits.
    for observe in [false, true] {
        let mut m = memory(&code.inner.code_buffer);
        let mut interpreter = Interpreter::new();
        let mut s = state();
        interpreter.observe_transfers = observe;
        let native = BTreeMap::from([(leaf_pc, leaf_pc)]);
        let result = interpreter
            .run_block(&mut s, &mut m, 256, STOP, Some((&native, 1)))
            .0;
        assert_eq!(result.units, 2);
        assert_eq!(s.eip, leaf_pc);
        let result = interpreter
            .run_block(&mut s, &mut m, 256, nested_pc, None)
            .0;
        assert_eq!(result.units, 2);
        assert_eq!(s.eip, nested_pc);
    }
    let mut m = memory(&code.inner.code_buffer);
    let mut interpreter = Interpreter::new();
    let mut s = state();
    s.eflags |= 0x100;
    let result = interpreter.run_block(&mut s, &mut m, 256, STOP, None).0;
    assert_eq!(result.units, 1);
    assert_eq!(
        result.step,
        Step::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(s.regs[4], STACK - 4);
}

#[test]
fn local_callee_code_write_exits_before_cached_successor() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut leaf = a.create_label();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.call(leaf).unwrap();
    a.jmp(top).unwrap();
    a.set_label(&mut leaf).unwrap();
    a.mov(dword_ptr(esi), eax).unwrap();
    a.inc(ebx).unwrap();
    a.ret().unwrap();
    let code = a.assemble(CODE as u64).unwrap();
    let mut m = memory(&code);
    m.protect(CODE, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
    let mut s = state();
    s.regs[6] = CODE + 128;
    s.regs[0] = 17;
    let mut interpreter = Interpreter::new();
    let result = interpreter.run_block(&mut s, &mut m, 256, STOP, None).0;
    assert_eq!(result.units, 2);
    assert_eq!(s.regs[3], 0);
    assert_eq!(s.regs[4], STACK - 4);
    assert_eq!(
        m.read_data(CODE + 128, 4).unwrap().as_ref(),
        &17u32.to_le_bytes()
    );
}
