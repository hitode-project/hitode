use adv_cpu::{
    ApiCall, ApiMemory, ApiOutcome, ApiSpec, Convention, EXEC, HostServices, Machine, READ, Region,
    Resume, RunOutcome, WRITE,
};
use iced_x86::code_asm::*;

const MAIN: u32 = 0x10000;
const TARGET: u32 = 0x12000;
const CALLBACK: u32 = 0x13000;
const API: u32 = 0x14000;
const HELPER: u32 = 0x15000;
const STOP: u32 = 0x18000;
const PAD: u32 = STOP + 16;
const STACK: u32 = 0x2f800;

fn code(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}
fn blank() -> Machine {
    let mut m = Machine::new(MAIN, 0x20000).unwrap();
    m.protect(MAIN, 0x20000, READ | WRITE).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m
}
fn region(m: &mut Machine, at: u32, bytes: &[u8], writable: bool) {
    m.write(at, bytes).unwrap();
    m.protect(at, 4096, READ | EXEC | if writable { WRITE } else { 0 })
        .unwrap();
    m.add_region(Region::new(at, bytes.len())).unwrap();
}
fn start(m: &mut Machine) {
    m.state.eip = MAIN;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}
fn api(m: &mut Machine) {
    m.register_api(ApiSpec::new(
        1,
        "fixture.dll",
        "boundary",
        API,
        Convention::Stdcall(0),
    ))
    .unwrap();
    m.protect(API, 4096, READ | EXEC).unwrap();
    m.add_region(Region::new(API, 16)).unwrap();
}

#[test]
fn host_patch_of_a_linked_target_retires_its_slot_before_the_next_call() {
    struct Patch;
    impl HostServices for Patch {
        fn call(
            &mut self,
            _: ApiCall<'_>,
            memory: &mut ApiMemory<'_>,
        ) -> Result<ApiOutcome, String> {
            memory.put_u32(TARGET + 1, 99)?;
            Ok(ApiOutcome::Return(7))
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
            m.run_with_services(STOP, 1000, &mut Patch).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(m.state.regs[0], 100);
        assert_eq!(m.stats.host_calls, 1);
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
fn callback_return_pad_remains_a_machine_boundary_after_cross_region_tail_calls() {
    struct CallbackServices {
        returned: Vec<u32>,
    }
    impl HostServices for CallbackServices {
        fn call(&mut self, _: ApiCall<'_>, _: &mut ApiMemory<'_>) -> Result<ApiOutcome, String> {
            Ok(ApiOutcome::Callback {
                target: CALLBACK,
                args: vec![41],
                convention: Convention::Stdcall(1),
                continuation: 7,
            })
        }
        fn resume(
            &mut self,
            token: u64,
            reason: Resume,
            _: &mut ApiMemory<'_>,
        ) -> Result<ApiOutcome, String> {
            assert_eq!(token, 7);
            let Resume::Returned(value) = reason else {
                panic!("unexpected continuation")
            };
            self.returned.push(value);
            Ok(ApiOutcome::Return(value + 100))
        }
    }
    let main = code(MAIN, |a| {
        a.call(u64::from(API)).unwrap();
        a.add(eax, 7).unwrap();
        a.ret().unwrap();
    });
    let callback = code(CALLBACK, |a| {
        a.mov(eax, dword_ptr(esp + 4)).unwrap();
        a.call(u64::from(HELPER)).unwrap();
        a.ret_1(4).unwrap();
    });
    let helper = code(HELPER, |a| {
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    for budget in [1, 5, 1000] {
        let mut m = blank();
        region(&mut m, MAIN, &main, false);
        region(&mut m, CALLBACK, &callback, false);
        region(&mut m, HELPER, &helper, false);
        api(&mut m);
        m.set_callback_return_pad(PAD).unwrap();
        start(&mut m);
        let mut services = CallbackServices { returned: vec![] };
        for _ in 0..100 {
            let outcome = m.run_with_services(STOP, budget, &mut services).unwrap();
            if outcome == RunOutcome::ReachedStop {
                break;
            }
            assert_eq!(outcome, RunOutcome::Budget);
            // A sliced callback thread can be moved away and restored exactly.
            let context = m.save_context().unwrap();
            m.restore_context(&context).unwrap();
        }
        assert_eq!(m.state.eip, STOP);
        assert_eq!(m.state.regs[0], 149);
        assert_eq!(m.state.regs[4], STACK + 4);
        assert_eq!(services.returned, [42]);
        assert_eq!(m.stats.callbacks, 1);
        assert_eq!(m.stats.interpreter_entries, 0);
        if budget == 1000 {
            assert!(m.stats.linked_entries >= 3);
        }
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
        cpu.machine_mut()
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
