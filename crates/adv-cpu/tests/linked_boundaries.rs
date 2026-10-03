use adv_cpu::{CpuState, EXEC, ExecutionTrap, Executor, Memory, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;

const MAIN: u32 = 0x10000;
const TARGET: u32 = 0x12000;
const API: u32 = 0x14000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x2f800;

fn code(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}
fn blank() -> Executor {
    let mut m = Executor::new(MAIN, 0x20000).unwrap();
    m.protect(MAIN, 0x20000, READ | WRITE).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m
}
fn region(m: &mut Executor, at: u32, bytes: &[u8], writable: bool) {
    m.write(at, bytes).unwrap();
    m.protect(at, 4096, READ | EXEC | if writable { WRITE } else { 0 })
        .unwrap();
    m.add_region(Region::new(at, bytes.len())).unwrap();
}
fn start(m: &mut Executor) {
    m.state.eip = MAIN;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}
fn api(m: &mut Executor) {
    m.write(API, &[0xcd, 0x2e, 0xc3]).unwrap();
    m.protect(API, 4096, READ | EXEC).unwrap();
    m.add_region(Region::new(API, 16)).unwrap();
}

#[test]
fn host_patch_of_a_linked_target_retires_its_slot_before_the_next_call() {
    struct Patch;
    impl ExecutionTrap for Patch {
        fn call(
            &mut self,
            state: &mut CpuState,
            memory: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            memory.write(TARGET + 1, &99u32.to_le_bytes())?;
            state.regs[0] = 7;
            Ok(false)
        }
    }
    let main = code(MAIN, |a| {
        a.call(u64::from(TARGET)).unwrap();
        a.mov(ebx, eax).unwrap();
        a.call(u64::from(API)).unwrap();
        a.call(u64::from(TARGET)).unwrap();
        a.add(eax, ebx).unwrap();
        a.ret().unwrap();
    });
    let target = code(TARGET, |a| {
        a.mov(eax, 1).unwrap();
        a.ret().unwrap();
    });
    for links in [false, true] {
        let mut m = blank();
        region(&mut m, MAIN, &main, false);
        region(&mut m, TARGET, &target, true);
        api(&mut m);
        m.set_region_linking(links);
        start(&mut m);
        assert_eq!(
            m.run_with_traps(STOP, 1000, &mut Patch, false).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(m.state.regs[0], 100);
        assert_eq!(m.stats.trap_calls, 1);
        assert!(m.stats.invalidations >= 1);
        assert_eq!(m.stats.interpreter_entries, 0);
        if links {
            assert!(m.stats.linked_entries >= 4);
        }
    }
}

#[test]
fn guest_patch_of_a_linked_target_rebuilds_slots_before_reentry() {
    let main = code(MAIN, |a| {
        a.call(u64::from(TARGET)).unwrap();
        a.mov(ebx, eax).unwrap();
        a.mov(dword_ptr(TARGET as i32 + 1), 42).unwrap();
        a.call(u64::from(TARGET)).unwrap();
        a.add(eax, ebx).unwrap();
        a.ret().unwrap();
    });
    let target = code(TARGET, |a| {
        a.mov(eax, 1).unwrap();
        a.ret().unwrap();
    });
    for links in [false, true] {
        let mut m = blank();
        region(&mut m, MAIN, &main, false);
        region(&mut m, TARGET, &target, true);
        m.set_region_linking(links);
        start(&mut m);
        assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 43);
        assert!(m.stats.invalidations >= 1);
        assert_eq!(m.stats.linked_entries >= 2, links);
        assert_eq!(m.memory().read(TARGET + 1, 4).unwrap(), 42u32.to_le_bytes());
    }
}

#[test]
fn long_pure_native_tail_chain_checks_wall_deadline_without_losing_resume_state() {
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Backend, Exit, Guest, Prot, Reg, TrapAction, TrapHandler};
    use std::time::{Duration, Instant};
    struct UnexpectedTrap;
    impl TrapHandler for UnexpectedTrap {
        fn on_trap(&mut self, _: &mut dyn Guest, _: u8) -> TrapAction {
            panic!("unexpected trap")
        }
    }
    let main = code(MAIN, |a| {
        a.inc(eax).unwrap();
        a.jmp(u64::from(TARGET)).unwrap();
    });
    let target = code(TARGET, |a| {
        a.inc(ebx).unwrap();
        a.jmp(u64::from(MAIN)).unwrap();
    });
    let mut cpu = JitCpu::new(STOP, true).unwrap();
    cpu.map(STOP, 4096, Prot::RX).unwrap();
    for (at, bytes) in [(MAIN, main), (TARGET, target)] {
        cpu.map(at, 4096, Prot::RX).unwrap();
        cpu.write(at, &bytes).unwrap();
        cpu.executor_mut()
            .add_region(Region::new(at, bytes.len()))
            .unwrap();
    }
    cpu.set_reg(Reg::Eip, MAIN);
    cpu.set_execution_budget(Some(Duration::from_millis(3)));
    for _ in 0..2 {
        let elapsed = Instant::now();
        assert!(matches!(
            cpu.run(&mut UnexpectedTrap),
            Exit::BudgetExceeded { .. }
        ));
        assert!(elapsed.elapsed() < Duration::from_secs(1));
        assert!(
            cpu.reg(Reg::Eax) == cpu.reg(Reg::Ebx) || cpu.reg(Reg::Eax) == cpu.reg(Reg::Ebx) + 1
        );
        assert!(cpu.reg(Reg::Eip) == MAIN || cpu.reg(Reg::Eip) == TARGET);
    }
    assert!(cpu.stats().linked_entries > 1024);
    assert_eq!(cpu.stats().interpreter_entries, 0);
}
