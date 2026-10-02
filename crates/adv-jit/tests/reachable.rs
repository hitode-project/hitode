mod support;
use adv_jit::{EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Machine;

const CODE: u32 = 0x10000;
const ROOT: u32 = CODE + 0x31;
const HELPER: u32 = CODE + 0x101;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}
fn machine() -> Machine {
    let mut m = Machine::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m
}
fn start(m: &mut Machine, pc: u32) {
    m.state.eip = pc;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

#[test]
fn reachable_roots_follow_calls_branches_and_skip_embedded_dead_code() {
    let bytes = assemble(ROOT, |a| {
        let mut zero = a.create_label();
        let mut done = a.create_label();
        a.test(ecx, ecx).unwrap();
        a.jz(zero).unwrap();
        a.call(u64::from(HELPER)).unwrap();
        a.jmp(done).unwrap();
        a.set_label(&mut zero).unwrap();
        a.mov(eax, 55).unwrap();
        a.set_label(&mut done).unwrap();
        a.ret().unwrap();
    });
    let helper = assemble(HELPER, |a| {
        a.add(eax, 7).unwrap();
        a.ret().unwrap();
    });
    let mut jit = machine();
    let mut uc = machine();
    for m in [&mut jit, &mut uc] {
        m.write(CODE, &[0xcc; 512]).unwrap();
        m.write(ROOT, &bytes).unwrap();
        m.write(HELPER, &helper).unwrap();
    }
    let mut region = Region::new(CODE, 512);
    region.reachable = true;
    region.entries.push(ROOT);
    jit.add_region(region).unwrap();
    for (condition, expected) in [(0, 55), (1, 12)] {
        for m in [&mut jit, &mut uc] {
            start(m, ROOT);
            m.state.regs[0] = 5;
            m.state.regs[1] = condition;
        }
        assert_eq!(jit.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(
            uc.run_unicorn(STOP, 10000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(jit.state.regs, uc.state.regs);
        assert_eq!(jit.state.regs[0], expected);
        assert_eq!(jit.state.eflags & 0x8d5, uc.state.eflags & 0x8d5);
    }
    assert_eq!(jit.stats.interpreter_entries, 0);
}

#[test]
fn reachable_compilation_does_not_generate_native_code_after_return() {
    let bytes = assemble(CODE, |a| {
        a.mov(eax, 7).unwrap();
        a.ret().unwrap();
        for _ in 0..100 {
            a.add(dword_ptr(edi), 1).unwrap();
        }
    });
    let mut linear = machine();
    let mut reachable = machine();
    for m in [&mut linear, &mut reachable] {
        m.write(CODE, &bytes).unwrap();
    }
    linear.add_region(Region::new(CODE, bytes.len())).unwrap();
    let mut region = Region::new(CODE, bytes.len());
    region.reachable = true;
    reachable.add_region(region).unwrap();
    for m in [&mut linear, &mut reachable] {
        start(m, CODE);
        m.state.regs[7] = DATA;
        assert_eq!(m.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 7);
        assert_eq!(m.memory().read(DATA, 4).unwrap(), [0; 4]);
    }
    assert!(
        reachable.stats.native_code_bytes * 4 < linear.stats.native_code_bytes,
        "reachable {} bytes, linear {} bytes",
        reachable.stats.native_code_bytes,
        linear.stats.native_code_bytes
    );
}

#[test]
fn automatic_region_expands_for_new_hot_indirect_targets_without_replaying_work() {
    let main = assemble(ROOT, |a| {
        let mut top = a.create_label();
        a.mov(edi, dword_ptr(DATA as i32)).unwrap();
        a.mov(ecx, 80).unwrap();
        a.xor(eax, eax).unwrap();
        a.xor(ebx, ebx).unwrap();
        a.set_label(&mut top).unwrap();
        a.call(edi).unwrap();
        a.inc(eax).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let first = assemble(HELPER, |a| {
        a.add(ebx, 2).unwrap();
        a.ret().unwrap();
    });
    let second = assemble(HELPER + 0x60, |a| {
        a.add(ebx, 5).unwrap();
        a.ret().unwrap();
    });
    let mut m = machine();
    m.write(ROOT, &main).unwrap();
    m.write(HELPER, &first).unwrap();
    m.write(HELPER + 0x60, &second).unwrap();
    m.enable_auto_jit(true);
    for (target, expected) in [(HELPER, 160), (HELPER + 0x60, 400)] {
        m.write(DATA, &target.to_le_bytes()).unwrap();
        start(&mut m, ROOT);
        assert_eq!(m.run(STOP, 10000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 80);
        assert_eq!(m.state.regs[3], expected);
    }
    assert_eq!(
        m.stats.compilations, 3,
        "main loop plus two dynamically discovered targets"
    );
    assert!(m.stats.native_entries > 0);
    assert_eq!(m.stats.invalidations, 0);
}
