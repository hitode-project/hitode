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

// spec: EXEC-CORE-6
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
    assert!(cached_block(&interpreter, CODE).successor.is_some());
    let generation = m.code_generation;
    storage.store_bytes(1, &11u32.to_le_bytes());
    assert_eq!(m.code_generation, generation);
    assert_eq!(
        interpreter
            .run_block(&mut s, &mut m, 256, STOP, None)
            .0
            .units,
        2
    );
    assert_eq!(s.regs[0], 11, "a successor hit still compares SHARED bytes");
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
    // Rewrite the CALL's own bytes unchanged: still a store to fetched code.
    let word = u32::from_le_bytes(code[..4].try_into().unwrap());
    s.regs[6] = CODE;
    s.regs[0] = word;
    let mut interpreter = Interpreter::new();
    let result = interpreter.run_block(&mut s, &mut m, 256, STOP, None).0;
    assert_eq!(result.units, 2);
    assert_eq!(s.regs[3], 0);
    assert_eq!(s.regs[4], STACK - 4);
    assert_eq!(m.read_data(CODE, 4).unwrap().as_ref(), &word.to_le_bytes());
}

// spec: EXEC-CORE-6
#[test]
fn unchanged_generation_skips_stamps_and_changed_generation_restamps() {
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(eax, 7).unwrap();
    a.ret().unwrap();
    let mut m = memory(&a.assemble(CODE as u64).unwrap());
    let mut decodes = 0;
    let mut block = DecodedBlock::fetch(&mut m, CODE, None, &mut decodes);
    assert_eq!(block.generation, Some(m.code_generation));
    // A stale local stamp would trigger the old per-page revalidation. The
    // unchanged owner generation proves that no page stamp needs consulting.
    block.pages[0].version ^= 1;
    let stale = block.pages[0].version;
    assert!(block.valid(&mut m));
    assert_eq!(block.pages[0].version, stale);
    m.touch(TABLE, 1);
    assert_ne!(block.generation, Some(m.code_generation));
    assert!(block.valid(&mut m));
    assert_eq!(block.pages[0].version, Page::at(&m, CODE).version);
    assert_eq!(block.generation, Some(m.code_generation));
}

// spec: EXEC-CORE-6
#[test]
fn data_write_on_code_page_keeps_generation_and_decoding() {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.mov(eax, 7).unwrap();
    a.jmp(top).unwrap();
    let code = a.assemble(CODE as u64).unwrap();
    let mut m = memory(&code);
    m.protect(CODE, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
    let mut s = state();
    let mut interpreter = Interpreter::new();
    interpreter.run_block(&mut s, &mut m, 2, STOP, None);
    let decodes = interpreter.decode_count();
    let generation = m.code_generation;
    m.write(CODE + 0x800, &[9]).unwrap();
    assert_eq!(m.code_generation, generation);
    interpreter.run_block(&mut s, &mut m, 2, STOP, None);
    assert_eq!(interpreter.decode_count(), decodes);
    // A code write revokes the fast path even if the bytes are identical.
    m.write(CODE, &code[..1]).unwrap();
    assert_ne!(m.code_generation, generation);
    interpreter.run_block(&mut s, &mut m, 2, STOP, None);
    assert_eq!(interpreter.decode_count(), decodes);
    m.write(CODE + 1, &9u32.to_le_bytes()).unwrap();
    interpreter.run_block(&mut s, &mut m, 2, STOP, None);
    assert_eq!(s.regs[0], 9);
    assert!(interpreter.decode_count() > decodes);
}

// spec: EXEC-CORE-6
#[test]
fn generation_covers_mapping_permissions_and_distinct_owners() {
    let mut m = Memory::reserve(CODE, 3 * PAGE_SIZE).unwrap();
    let initial = m.code_generation;
    m.map(CODE, PAGE_SIZE, READ | WRITE).unwrap();
    assert_ne!(m.code_generation, initial);
    let mapped = m.code_generation;
    m.protect(CODE, PAGE_SIZE, READ).unwrap();
    assert_ne!(m.code_generation, mapped);
    let protected = m.code_generation;
    m.unmap(CODE, PAGE_SIZE).unwrap();
    assert_ne!(m.code_generation, protected);
    let unmapped = m.code_generation;
    m.map_shared_audio(CODE, PAGE_SIZE).unwrap();
    assert_ne!(m.code_generation, unmapped);
    let shared = m.code_generation;
    m.map_private_copy_pages(CODE + PAGE_SIZE as u32, &[0; PAGE_SIZE], READ | WRITE)
        .unwrap();
    assert_ne!(m.code_generation, shared);
    let snapshot = m.code_generation;
    m.privatize_copy_pages(CODE + PAGE_SIZE as u32, PAGE_SIZE)
        .unwrap();
    assert_ne!(m.code_generation, snapshot);
    let other = Memory::reserve(CODE, 3 * PAGE_SIZE).unwrap();
    assert_ne!(m.code_generation, other.code_generation);
}

fn successor_fixture(capacity: usize) -> (Interpreter, Memory, CpuState) {
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(eax, 7).unwrap();
    a.jmp(TABLE as u64).unwrap();
    a.ud2().unwrap();
    let mut m = memory(&a.assemble(CODE as u64).unwrap());
    let mut b = CodeAssembler::new(32).unwrap();
    b.mov(ebx, 11).unwrap();
    b.jmp(CODE as u64).unwrap();
    m.write(TABLE, &b.assemble(TABLE as u64).unwrap()).unwrap();
    m.protect(TABLE, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
    let mut interpreter = Interpreter::with_capacity(capacity);
    let mut s = state();
    for _ in 0..6 {
        assert_eq!(
            interpreter
                .run_block(&mut s, &mut m, 64, STOP, None)
                .0
                .units,
            2
        );
    }
    (interpreter, m, s)
}

// spec: EXEC-CORE-6
#[test]
fn successors_follow_boxes_and_reject_changed_entry_and_native_revision() {
    let (mut interpreter, mut m, mut s) = successor_fixture(8);
    let a = interpreter.blocks[&CODE].pointer().0;
    let b = interpreter.blocks[&TABLE].pointer().0;
    let forward = cached_block(&interpreter, CODE).successor.unwrap();
    let backward = cached_block(&interpreter, TABLE).successor.unwrap();
    assert_eq!((forward.pc, forward.block.0), (TABLE, b));
    assert_eq!((backward.pc, backward.block.0), (CODE, a));
    assert_eq!(forward.generation, interpreter.cache_generation);
    let decodes = interpreter.decode_count();
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!(interpreter.decode_count(), decodes);
    // Returning to a different entry than the previous exit cannot use its edge.
    s.eip = CODE;
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!((s.eip, s.regs[0]), (TABLE, 7));
    assert_eq!(interpreter.decode_count(), decodes);
    let native = BTreeMap::new();
    for _ in 0..6 {
        interpreter.run_block(&mut s, &mut m, 64, STOP, Some((&native, 1)));
    }
    let decodes = interpreter.decode_count();
    interpreter.run_block(&mut s, &mut m, 64, STOP, Some((&native, 2)));
    assert!(interpreter.decode_count() > decodes);
}

// spec: EXEC-CORE-6
#[test]
fn successor_smc_and_permissions_revalidate_before_execution() {
    let (mut interpreter, mut m, mut s) = successor_fixture(8);
    // A -> B is already linked. Patch B before entering it through that link.
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    let decodes = interpreter.decode_count();
    m.write(TABLE + 1, &19u32.to_le_bytes()).unwrap();
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!((s.eip, s.regs[3]), (CODE, 19));
    assert!(interpreter.decode_count() > decodes);
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    m.protect(TABLE, PAGE_SIZE, READ | WRITE).unwrap();
    let result = interpreter.run_block(&mut s, &mut m, 64, STOP, None).0;
    assert_eq!(
        result.step,
        Step::FetchFault {
            pc: TABLE,
            address: TABLE
        }
    );
    assert_eq!(result.units, 0);
    m.protect(TABLE, PAGE_SIZE, READ | WRITE | EXEC).unwrap();
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!((s.eip, s.regs[3]), (CODE, 19));
}

// spec: EXEC-CORE-6
#[test]
fn clearing_and_saturation_revoke_successors_before_storage_reuse() {
    let (mut interpreter, mut m, mut s) = successor_fixture(2);
    let generation = interpreter.cache_generation;
    let decodes = interpreter.decode_count();
    interpreter.clear_cache();
    assert_ne!(interpreter.cache_generation, generation);
    assert!(interpreter.previous.is_none());
    assert!(interpreter.blocks.is_empty());
    // Repopulate in the opposite order after the old allocations are freed.
    s.eip = TABLE;
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!(interpreter.blocks.len(), 1);
    assert!(interpreter.decode_count() > decodes);
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    let generation = interpreter.cache_generation;
    // A third entry flushes the bounded two-block set, invalidating all edges.
    s.eip = CODE + 5;
    interpreter.run_block(&mut s, &mut m, 1, STOP, None);
    assert_ne!(interpreter.cache_generation, generation);
    assert_eq!(interpreter.blocks.len(), 1);
    s.eip = TABLE;
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!((s.eip, s.regs[3]), (CODE, 11));
    let mut uncached = Interpreter::with_capacity(0);
    uncached.run_block(&mut s, &mut m, 64, STOP, None);
    assert!(uncached.blocks.is_empty());
    assert!(uncached.previous.is_none());
}

fn cached_block(interpreter: &Interpreter, pc: u32) -> &DecodedBlock {
    // SAFETY: tests only inspect this live entry through the shared owner borrow.
    unsafe { interpreter.blocks[&pc].pointer().0.as_ref() }
}

// spec: EXEC-CORE-6
#[test]
fn successor_references_survive_hash_table_growth() {
    let (mut interpreter, mut m, mut s) = successor_fixture(256);
    let a = interpreter.blocks[&CODE].pointer().0;
    let b = interpreter.blocks[&TABLE].pointer().0;
    let capacity = interpreter.blocks.capacity();
    for n in 0..128 {
        let pc = CODE + 0x100 + n * 16;
        let mut code = CodeAssembler::new(32).unwrap();
        code.mov(eax, n).unwrap();
        code.jmp(TABLE as u64).unwrap();
        code.ud2().unwrap();
        m.write(pc, &code.assemble(pc as u64).unwrap()).unwrap();
        s.eip = pc;
        interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    }
    assert!(interpreter.blocks.capacity() > capacity);
    assert_eq!(interpreter.blocks[&CODE].pointer().0, a);
    assert_eq!(interpreter.blocks[&TABLE].pointer().0, b);
    let decodes = interpreter.decode_count();
    s.eip = CODE;
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!((s.eip, s.regs[3]), (CODE, 11));
    assert_eq!(interpreter.decode_count(), decodes);
}

// spec: EXEC-CORE-6
#[test]
fn instruction_cache_saturation_also_revokes_successor_generation() {
    let (mut interpreter, mut m, mut s) = successor_fixture(2);
    let generation = interpreter.cache_generation;
    let mut code = CodeAssembler::new(32).unwrap();
    for _ in 0..3 {
        code.nop().unwrap();
    }
    m.write(CODE + 0x100, &code.assemble((CODE + 0x100) as u64).unwrap())
        .unwrap();
    for pc in CODE + 0x100..CODE + 0x103 {
        s.eip = pc;
        assert_eq!(interpreter.step(&mut s, &mut m), Step::Executed);
    }
    assert_ne!(interpreter.cache_generation, generation);
    assert!(interpreter.previous.is_none());
    assert_eq!(interpreter.blocks.len(), 2);
    assert_ne!(
        cached_block(&interpreter, CODE)
            .successor
            .unwrap()
            .generation,
        interpreter.cache_generation
    );
    let decodes = interpreter.decode_count();
    s.eip = CODE;
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    interpreter.run_block(&mut s, &mut m, 64, STOP, None);
    assert_eq!(interpreter.decode_count(), decodes);
}
