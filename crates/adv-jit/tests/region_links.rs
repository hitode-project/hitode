use adv_jit::{CompileStrategy, EXEC, Machine, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;

const MAIN: u32 = 0x10000;
const CALLEE: u32 = 0x12000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x1f800;

fn assemble(at: u32, f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(at as u64).unwrap()
}

fn setup(strategy: CompileStrategy, linked: bool) -> Machine {
    let caller = assemble(MAIN, |a| {
        let mut again = a.create_label();
        a.set_label(&mut again).unwrap();
        a.call(CALLEE as u64).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(again).unwrap();
        a.ret().unwrap();
    });
    let callee = assemble(CALLEE, |a| {
        a.add(eax, 1).unwrap();
        a.paddb(xmm0, xmm1).unwrap();
        a.ret().unwrap();
    });
    let mut m = Machine::new(MAIN, 0x10000).unwrap();
    m.protect(MAIN, 0x10000, READ | WRITE).unwrap();
    for (pc, bytes) in [(MAIN, caller), (CALLEE, callee)] {
        m.write(pc, &bytes).unwrap();
        m.protect(pc, 4096, READ | EXEC).unwrap();
        let mut region = Region::new(pc, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.set_region_linking(linked);
    seed(&mut m, 100_000);
    m
}

fn seed(m: &mut Machine, iterations: u32) {
    m.state.eip = MAIN;
    m.state.regs = [0, iterations, 0x7654_3210, 0, STACK, 0, 0, 0];
    m.state.eflags = 0x203;
    m.state.xmm[0] = [0; 16];
    m.state.xmm[1] = [3; 16];
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

#[test]
fn long_tail_chains_preserve_integer_flags_vectors_and_guest_stack() {
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut linked = setup(strategy, true);
        let mut dispatched = setup(strategy, false);
        assert_eq!(
            linked.run(STOP, 1_000_000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(
            dispatched.run(STOP, 1_000_000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(linked.state, dispatched.state);
        assert_eq!(linked.state.regs[0], 100_000);
        assert_eq!(linked.state.regs[4], STACK + 4);
        assert_eq!(linked.state.xmm[0], [(300_000u32 % 256) as u8; 16]);
        assert!(linked.stats.linked_entries > 190_000);
        assert_eq!(dispatched.stats.linked_entries, 0);
        assert_eq!(linked.stats.native_entries, dispatched.stats.native_entries);
        assert_eq!(linked.stats.native_blocks, dispatched.stats.native_blocks);
        assert_eq!(linked.stats.interpreter_entries, 0);
    }
}

#[test]
fn tiny_budgets_return_and_resume_at_the_same_guest_instruction() {
    let mut linked = setup(CompileStrategy::Speed, true);
    let mut dispatched = setup(CompileStrategy::Speed, false);
    seed(&mut linked, 31);
    seed(&mut dispatched, 31);
    for round in 0..200 {
        let budget = (round % 5) + 1;
        let a = linked.run(STOP, budget).unwrap();
        let b = dispatched.run(STOP, budget).unwrap();
        assert_eq!(a, b);
        assert_eq!(linked.state, dispatched.state);
        assert_eq!(
            linked.memory().read(STACK - 4, 8).unwrap(),
            dispatched.memory().read(STACK - 4, 8).unwrap()
        );
        if a == RunOutcome::ReachedStop {
            assert_eq!(linked.state.regs[0], 31);
            return;
        }
        assert_eq!(a, RunOutcome::Budget);
    }
    panic!("linked loop did not make progress");
}

#[test]
fn rewritten_and_reprotected_targets_do_not_use_retired_entry_slots() {
    let mut m = setup(CompileStrategy::Speed, true);
    seed(&mut m, 3);
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    let replacement = assemble(CALLEE, |a| {
        a.add(eax, 7).unwrap();
        a.paddb(xmm0, xmm1).unwrap();
        a.ret().unwrap();
    });
    m.write(CALLEE, &replacement).unwrap();
    seed(&mut m, 3);
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 21);
    m.protect(CALLEE, 4096, READ).unwrap();
    seed(&mut m, 3);
    assert!(matches!(
        m.run(STOP, 1000).unwrap(),
        RunOutcome::FetchFault { pc: CALLEE, .. }
    ));
    m.protect(CALLEE, 4096, READ | EXEC).unwrap();
    seed(&mut m, 3);
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 21);
}

#[test]
fn profiling_keeps_per_region_attribution_and_can_be_disabled_again() {
    let mut m = setup(CompileStrategy::Speed, true);
    seed(&mut m, 7);
    m.set_profiling(true);
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.stats.linked_entries, 0);
    assert_eq!(m.profile[&CALLEE].native_entries, 7);
    m.set_profiling(false);
    seed(&mut m, 7);
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    assert!(m.stats.linked_entries >= 13);
}
