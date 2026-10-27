use adv_cpu::{CpuState, EXEC, ExecutionTrap, Executor, Memory, READ, Region, RunOutcome, WRITE};

mod support;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x1ff00;

fn executor(bytes: &[u8], native: bool) -> Executor {
    let mut cpu = Executor::new(CODE, 0x10000).unwrap();
    cpu.protect(CODE, 0x10000, READ | WRITE).unwrap();
    cpu.write(CODE, bytes).unwrap();
    cpu.protect(CODE, 4096, READ | EXEC).unwrap();
    cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
    cpu.state.eip = CODE;
    cpu.state.regs[4] = STACK;
    if native {
        cpu.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    cpu
}

#[test]
fn restored_resume_flag_survives_a_budget_stop_and_is_consumed_without_a_debug_trap() {
    for native in [false, true] {
        let mut cpu = executor(&[0x90, 0x40, 0xc3], native);
        let mut state = cpu.state.capture_architecture();
        state.eflags |= 1 << 16;
        cpu.state.restore_architecture(&state).unwrap();
        assert_eq!(cpu.run(STOP, 0).unwrap(), RunOutcome::Budget);
        assert_ne!(cpu.state.eflags & (1 << 16), 0);
        assert_eq!(cpu.run(STOP, 1).unwrap(), RunOutcome::Budget);
        assert_eq!(cpu.state.eip, CODE + 1);
        assert_eq!(cpu.state.eflags & (1 << 16), 0);
        assert_eq!(cpu.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(cpu.state.regs[0], 1);
    }
}

#[test]
fn unhandled_native_and_interpreted_interrupt_return_committed_cpu_state() {
    for native in [false, true] {
        let mut cpu = executor(&[0x40, 0xcd, 0x2e, 0x43, 0xc3], native);
        assert_eq!(
            cpu.run(STOP, 100).unwrap(),
            RunOutcome::Trap {
                pc: CODE + 1,
                vector: 0x2e
            }
        );
        assert_eq!(cpu.state.eip, CODE + 3);
        assert_eq!(cpu.state.regs[0], 1);
        assert_eq!(cpu.state.regs[3], 0);
        assert_eq!(cpu.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(cpu.state.regs[3], 1);
    }
}

#[test]
fn raw_handler_changes_to_pc_and_code_stop_stale_native_execution() {
    struct Patch;
    impl ExecutionTrap for Patch {
        fn call(
            &mut self,
            state: &mut CpuState,
            memory: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            assert_eq!(state.eip, CODE + 2);
            memory.write(CODE + 3, &99u32.to_le_bytes())?;
            state.regs[0] = 7;
            Ok(false)
        }
    }
    for native in [false, true] {
        let mut cpu = executor(&[0xcd, 0x2e, 0xb8, 1, 0, 0, 0, 0xc3], native);
        assert_eq!(
            cpu.run_with_traps(STOP, 100, &mut Patch, false).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(cpu.state.regs[0], 99);
        assert_eq!(cpu.stats.trap_calls, 1);
    }
}

#[test]
fn raw_yield_preserves_handler_state_and_has_no_cpu_owned_continuation() {
    struct Yield;
    impl ExecutionTrap for Yield {
        fn call(
            &mut self,
            state: &mut CpuState,
            _: &mut Memory,
            vector: u32,
        ) -> Result<bool, String> {
            assert_eq!(vector, 0x2e);
            state.regs[0] = 41;
            Ok(true)
        }
    }
    for native in [false, true] {
        let mut cpu = executor(&[0xcd, 0x2e, 0x40, 0xc3], native);
        assert_eq!(
            cpu.run_with_traps(STOP, 100, &mut Yield, false).unwrap(),
            RunOutcome::Parked
        );
        assert_eq!(cpu.state.eip, CODE + 2);
        assert_eq!(cpu.state.regs[0], 41);
        let context = cpu.save_context().unwrap();
        cpu.state.regs[0] = 0;
        cpu.restore_context(&context).unwrap();
        assert_eq!(cpu.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(cpu.state.regs[0], 42);
        assert_eq!(cpu.stats.trap_calls, 1);
    }
}

#[test]
fn raw_handler_errors_and_panics_are_contained_at_the_cpu_boundary() {
    struct Error(bool);
    impl ExecutionTrap for Error {
        fn call(&mut self, _: &mut CpuState, _: &mut Memory, _: u32) -> Result<bool, String> {
            if self.0 {
                panic!("test panic");
            }
            Err("test failure".into())
        }
    }
    for native in [false, true] {
        for panic in [false, true] {
            let mut cpu = executor(&[0xcd, 0x2e, 0xc3], native);
            assert_eq!(
                cpu.run_with_traps(STOP, 100, &mut Error(panic), false)
                    .unwrap_err(),
                if panic {
                    "CPU trap handler panicked"
                } else {
                    "test failure"
                }
            );
            assert_eq!(cpu.state.eip, CODE + 2);
        }
    }
}
