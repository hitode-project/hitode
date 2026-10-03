mod support;
use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Machine;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x20800;
const ITERATIONS: u32 = 96;

fn machine(sparse: bool) -> Machine {
    let mut m = if sparse {
        let mut m = Machine::address_space().unwrap();
        m.map(CODE, 0x3000, READ | EXEC).unwrap();
        m.map(STOP, 4096, READ | EXEC).unwrap();
        m.map(STACK & !4095, 4096, READ | WRITE).unwrap();
        m
    } else {
        let mut m = Machine::new(CODE, 0x20000).unwrap();
        m.protect(CODE, 0x20000, READ | WRITE).unwrap();
        m.protect(CODE, 0x3000, READ | EXEC).unwrap();
        m.protect(STOP, 4096, READ | EXEC).unwrap();
        m
    };
    m.set_profiling(true);
    m
}

fn program(root: u32, instruction: &[u8]) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    a.sete(bl).unwrap();
    a.inc(eax).unwrap();
    a.dec(ecx).unwrap();
    a.jnz(u64::from(root)).unwrap();
    a.ret().unwrap();
    let mut bytes = instruction.to_vec();
    bytes.extend(
        a.assemble(u64::from(root) + instruction.len() as u64)
            .unwrap(),
    );
    bytes
}

fn start(m: &mut Machine, root: u32) {
    m.state.eip = root;
    m.state.regs = [
        0,
        ITERATIONS,
        0x5566_7788,
        0x1122_3300,
        STACK,
        STACK + 0x40,
        0,
        0,
    ];
    m.state.eflags = 2;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.write(STACK + 0x20, &255u32.to_le_bytes()).unwrap();
}

fn compare(jit: &mut Machine, uc: &mut Machine, root: u32) {
    for m in [&mut *jit, &mut *uc] {
        start(m, root);
    }
    assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 10000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
}

fn root_interpreter_entries(m: &Machine, root: u32) -> u64 {
    m.profile.get(&root).map_or(0, |s| s.interpreter_entries)
}

#[test]
fn automatic_tail_completes_seven_and_fifteen_byte_instructions_without_moving_the_window() {
    // An authored CMP of a stack local crosses the 512-byte boundary by one byte.
    let mut cases = vec![(6, vec![0x81, 0x7d, 0xe0, 0xff, 0, 0, 0])];
    // Redundant DS prefixes make a legal maximum-length register MOV. Exercise
    // one, six and fourteen tail bytes rather than extending the region's root range.
    for distance in [1, 9, 14] {
        let mut instruction = vec![0x3e; 10];
        instruction.extend([0xb8, 0x78, 0x56, 0x34, 0x12]);
        cases.push((distance, instruction));
    }
    for (distance, instruction) in cases {
        let root = CODE + 512 - distance;
        let bytes = program(root, &instruction);
        let mut jit = machine(false);
        let mut uc = machine(false);
        for m in [&mut jit, &mut uc] {
            m.write(root, &bytes).unwrap();
        }
        jit.enable_auto_jit(true);
        compare(&mut jit, &mut uc, root);
        let interpreted = root_interpreter_entries(&jit, root);
        assert!(jit.stats.native_entries > 0);
        assert!(
            interpreted <= 32,
            "tail root remains in the interpreter: {interpreted}"
        );
        compare(&mut jit, &mut uc, root);
        assert_eq!(
            root_interpreter_entries(&jit, root),
            interpreted,
            "warm tail must stay native"
        );
        assert_eq!(
            jit.stats.compilations, 2,
            "each nominal 512-byte window owns its instruction starts"
        );
    }
}

#[test]
fn changing_only_a_tail_byte_on_the_next_page_invalidates_its_decoder_owner() {
    let root = CODE + 4096 - 6;
    let bytes = program(root, &[0x81, 0x7d, 0xe0, 0xff, 0, 0, 0]);
    let mut jit = machine(false);
    let mut uc = machine(false);
    for m in [&mut jit, &mut uc] {
        m.write(root, &bytes).unwrap();
    }
    jit.enable_auto_jit(true);
    compare(&mut jit, &mut uc, root);
    assert_eq!(jit.state.regs[3] & 255, 1);
    let interpreted = root_interpreter_entries(&jit, root);

    // A separate, warmed explicit region must not be invalidated by the tail edit.
    let unrelated = CODE + 0x2040;
    jit.write(unrelated, &[0xb8, 77, 0, 0, 0, 0xc3]).unwrap();
    jit.add_region(Region::new(unrelated, 6)).unwrap();
    start(&mut jit, unrelated);
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    let compiles = jit.stats.compilations;
    for m in [&mut jit, &mut uc] {
        m.write(CODE + 4096, &[1]).unwrap();
    }
    compare(&mut jit, &mut uc, root);
    assert_eq!(jit.state.regs[3] & 255, 0);
    assert_eq!(root_interpreter_entries(&jit, root), interpreted);
    assert_eq!(jit.stats.compilations, compiles + 2);
    assert_eq!(
        jit.stats.invalidations, 2,
        "tail owner and following region share the edited page"
    );
    start(&mut jit, unrelated);
    assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(jit.state.regs[0], 77);
    assert_eq!(jit.stats.compilations, compiles + 2);
}

#[test]
fn a_guest_store_to_the_tail_page_recompiles_the_cross_page_instruction() {
    let root = CODE + 4096 - 6;
    let bytes = program(root, &[0x81, 0x7d, 0xe0, 0xff, 0, 0, 0]);
    let writer = CODE + 0x2040;
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(byte_ptr((CODE + 4096) as i32), 1).unwrap();
    a.jmp(u64::from(root)).unwrap();
    let modification = a.assemble(u64::from(writer)).unwrap();
    let mut jit = machine(false);
    let mut uc = machine(false);
    for m in [&mut jit, &mut uc] {
        m.write(root, &bytes).unwrap();
        m.write(writer, &modification).unwrap();
        m.protect(CODE + 4096, 4096, READ | WRITE | EXEC).unwrap();
    }
    jit.enable_auto_jit(true);
    compare(&mut jit, &mut uc, root);
    assert_eq!(jit.state.regs[3] & 255, 1);
    jit.add_region(Region::new(writer, modification.len()))
        .unwrap();
    compare(&mut jit, &mut uc, writer);
    assert_eq!(jit.state.regs[3] & 255, 0);
    assert_eq!(jit.memory().read(CODE + 4096, 1).unwrap(), [1]);
    assert_eq!(jit.stats.invalidations, 2);
    assert_eq!(jit.stats.compilations, 5);
}

#[test]
fn a_missing_tail_execute_page_faults_before_the_instruction_and_can_be_restored() {
    for unmap in [false, true] {
        let root = CODE + 4096 - 6;
        let bytes = program(root, &[0x81, 0x7d, 0xe0, 0xff, 0, 0, 0]);
        let mut jit = machine(unmap);
        let mut uc = machine(unmap);
        for m in [&mut jit, &mut uc] {
            m.write(root, &bytes).unwrap();
        }
        jit.enable_auto_jit(true);
        compare(&mut jit, &mut uc, root);
        for m in [&mut jit, &mut uc] {
            if unmap {
                m.unmap(CODE + 4096, 4096).unwrap();
            } else {
                m.protect(CODE + 4096, 4096, READ | WRITE).unwrap();
            }
            start(m, root);
        }
        let outcome = jit.run(STOP, 10000).unwrap();
        assert_eq!(outcome, uc.run_unicorn(STOP, 10000).unwrap());
        assert!(matches!(outcome, RunOutcome::FetchFault { .. }));
        assert_eq!(jit.state.eip, root);
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(jit.state.eflags, uc.state.eflags);
        let interpreted = root_interpreter_entries(&jit, root);
        for m in [&mut jit, &mut uc] {
            if unmap {
                m.map(CODE + 4096, 4096, READ | EXEC).unwrap();
                m.write(CODE + 4096, &bytes[6..]).unwrap();
            } else {
                m.protect(CODE + 4096, 4096, READ | EXEC).unwrap();
            }
        }
        compare(&mut jit, &mut uc, root);
        assert_eq!(
            root_interpreter_entries(&jit, root),
            interpreted,
            "mapping/EXEC restoration must invalidate the truncated decode"
        );
    }
}

#[test]
fn explicit_linear_and_reachable_regions_keep_their_original_byte_limit() {
    let root = CODE + 512 - 6;
    let bytes = program(root, &[0x81, 0x7d, 0xe0, 0xff, 0, 0, 0]);
    for reachable in [false, true] {
        let mut jit = machine(false);
        let mut uc = machine(false);
        for m in [&mut jit, &mut uc] {
            m.write(root, &bytes).unwrap();
        }
        let mut region = Region::new(root, 6);
        region.reachable = reachable;
        jit.add_region(region).unwrap();
        compare(&mut jit, &mut uc, root);
        assert_eq!(jit.stats.native_entries, 0);
        assert!(root_interpreter_entries(&jit, root) > 0);
    }
}
