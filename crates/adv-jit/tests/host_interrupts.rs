use adv_jit::{EXEC, Machine, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const CODE: u32 = 0x10000;
const TARGET: u32 = 0x12000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x19000;
const STACK: u32 = 0x1ff00;

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn machine(programs: &[(u32, Vec<u8>)], native: bool) -> Machine {
    let mut m = Machine::new(CODE, 0x10000).unwrap();
    m.protect(CODE, 0x10000, READ | WRITE).unwrap();
    for (at, bytes) in programs {
        m.write(*at, bytes).unwrap();
        m.protect(*at, 4096, READ | EXEC).unwrap();
        if native {
            m.add_region(Region::new(*at, bytes.len())).unwrap();
        }
    }
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = STACK;
    m
}

#[test]
fn pending_notification_preserves_cpu_and_fuel_until_owner_clears_it() {
    let bytes = assemble(CODE, |a| {
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    for native in [false, true] {
        let mut m = machine(&[(CODE, bytes.clone())], native);
        let flag = Arc::new(AtomicBool::new(true));
        m.set_host_interrupt_flag(flag.clone());
        m.state.fuel = 123;
        let before = m.state.clone();
        for _ in 0..2 {
            assert_eq!(m.run(STOP, 123).unwrap(), RunOutcome::Interrupted);
            assert_eq!(m.state, before);
        }
        assert!(flag.load(Ordering::Acquire));
        assert_eq!(m.stats.native_blocks + m.stats.interpreter_instructions, 0);
        flag.store(false, Ordering::Release);
        assert_eq!(m.run(STOP, 123).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], 1);
    }
}

#[test]
fn native_direct_indirect_and_linked_loops_interrupt_and_resume_exact_prefix() {
    for mode in 0..3 {
        let first = assemble(CODE, |a| {
            let mut again = a.create_label();
            a.set_label(&mut again).unwrap();
            a.inc(eax).unwrap();
            a.paddb(mm0, mm1).unwrap();
            a.dec(ecx).unwrap();
            if mode == 0 {
                a.jnz(again).unwrap();
            } else {
                let mut finished = a.create_label();
                a.jz(finished).unwrap();
                if mode == 1 {
                    a.jmp(edx).unwrap();
                } else {
                    a.jmp(u64::from(TARGET)).unwrap();
                }
                a.set_label(&mut finished).unwrap();
            }
            a.emms().unwrap();
            a.ret().unwrap();
        });
        let mut programs = vec![(CODE, first)];
        if mode == 2 {
            programs.push((
                TARGET,
                assemble(TARGET, |a| {
                    a.jmp(u64::from(CODE)).unwrap();
                }),
            ));
        }
        let mut m = machine(&programs, true);
        m.state.regs[1] = 100_000_000;
        m.state.regs[2] = CODE;
        m.state.mmx[0] = 0;
        m.state.mmx[1] = u64::from_le_bytes([1; 8]);
        m.state.mmx_mode = 1 | (1 << 2) | (3 << 8);
        m.state.commit_mmx();
        let flag = Arc::new(AtomicBool::new(false));
        m.set_host_interrupt_flag(flag.clone());
        let notify = flag.clone();
        let producer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(2));
            notify.store(true, Ordering::Release);
        });
        let elapsed = Instant::now();
        let budget = 100_000_000;
        let outcome = m.run(STOP, budget).unwrap();
        producer.join().unwrap();
        assert_eq!(outcome, RunOutcome::Interrupted, "mode {mode}");
        assert!(elapsed.elapsed() < Duration::from_secs(2));
        assert_eq!(m.state.fuel + m.stats.native_blocks, budget);
        assert_eq!(m.stats.interpreter_instructions, 0);
        let completed = m.state.regs[0];
        assert_eq!(m.state.regs[1], 100_000_000 - completed);
        assert_eq!(m.state.mmx[0].to_le_bytes(), [completed as u8; 8]);
        let context = m.save_context().unwrap();
        assert_eq!(context.cpu().x87.regs[0].significand(), m.state.mmx[0]);
        assert!(m.state.eip == CODE || (mode == 2 && m.state.eip == TARGET));
        flag.store(false, Ordering::Release);
        m.state.regs[1] = 10;
        assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.regs[0], completed + 10);
        assert_eq!(m.state.mmx[0].to_le_bytes(), [(completed + 10) as u8; 8]);
        assert_eq!(m.save_context().unwrap().cpu().x87.tag, 0xffff);
    }
}

#[test]
fn interpreted_x87_loop_interrupts_at_a_restartable_block_boundary() {
    let bytes = assemble(CODE, |a| {
        let mut again = a.create_label();
        a.set_label(&mut again).unwrap();
        a.fld1().unwrap();
        a.fadd_2(st0, st0).unwrap();
        a.fstp(dword_ptr(DATA as i32)).unwrap();
        a.inc(eax).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(again).unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(&[(CODE, bytes)], false);
    m.state.regs[1] = 100_000_000;
    let flag = Arc::new(AtomicBool::new(false));
    m.set_host_interrupt_flag(flag.clone());
    let notify = flag.clone();
    let producer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2));
        notify.store(true, Ordering::Release);
    });
    let budget = 100_000_000;
    assert_eq!(m.run(STOP, budget).unwrap(), RunOutcome::Interrupted);
    producer.join().unwrap();
    assert_eq!(m.state.fuel + m.stats.interpreter_instructions, budget);
    assert_eq!(m.state.x87.tag, 0xffff);
    assert_eq!(m.state.regs[1], 100_000_000 - m.state.regs[0]);
    assert_eq!(m.memory().read(DATA, 4).unwrap(), &2f32.to_le_bytes());
    flag.store(false, Ordering::Release);
    let done = m.state.regs[0];
    m.state.regs[1] = 10;
    assert_eq!(m.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], done + 10);
    assert_eq!(m.state.x87.tag, 0xffff);
}

#[cfg(feature = "pal")]
#[test]
fn pal_budget_stop_disable_and_fault_priorities_preserve_original_outcomes() {
    use adv_jit::pal::JitCpu;
    use pal_cpu::{Backend, Exit, Fault, FaultKind, Guest, Prot, Reg, TrapAction, TrapHandler};
    struct Signal {
        wake: pal_host::WakeEvent,
        park: bool,
    }
    impl TrapHandler for Signal {
        fn on_trap(&mut self, _: &mut dyn Guest, vector: u8) -> TrapAction {
            assert_eq!(vector, 0x2e);
            self.wake.signal();
            if self.park {
                TrapAction::Park
            } else {
                TrapAction::Continue
            }
        }
    }
    for native in [false, true] {
        let wake = pal_host::WakeEvent::new().unwrap();
        let mut cpu = JitCpu::new(STOP, native).unwrap();
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(STOP, 4096, Prot::RX).unwrap();
        cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
        let bytes = assemble(CODE, |a| {
            a.int(0x2e).unwrap();
            a.mov(eax, dword_ptr(0x2dead000i32)).unwrap();
        });
        cpu.write(CODE, &bytes).unwrap();
        if native {
            cpu.machine_mut()
                .add_region(Region::new(CODE, bytes.len()))
                .unwrap();
        }
        cpu.set_waker(wake.clone()).unwrap();
        let mut handler = Signal {
            wake: wake.clone(),
            park: false,
        };
        cpu.set_reg(Reg::Eip, CODE);
        wake.signal();
        cpu.set_execution_budget(Some(Duration::ZERO));
        assert_eq!(
            cpu.run(&mut handler),
            Exit::BudgetExceeded {
                pc: CODE,
                budget: Duration::ZERO
            }
        );
        cpu.set_reg(Reg::Eip, STOP);
        assert_eq!(cpu.run(&mut handler), Exit::Parked);
        cpu.set_reg(Reg::Eip, CODE);
        cpu.set_execution_budget(None);
        assert_eq!(cpu.run(&mut handler), Exit::Interrupted { pc: CODE });
        wake.take_interrupt();
        if native {
            // The fault occurs within the next native block after the handler
            // signals. An already produced fault wins over a pending interrupt.
            assert_eq!(
                cpu.run(&mut handler),
                Exit::Fault(Fault {
                    pc: CODE + 2,
                    kind: FaultKind::Read(0x2dead000)
                })
            );
        }
        wake.take_interrupt();
        cpu.set_reg(Reg::Eip, CODE);
        cpu.set_host_interrupts(false);
        wake.signal();
        assert_eq!(
            cpu.run(&mut handler),
            Exit::Fault(Fault {
                pc: CODE + 2,
                kind: FaultKind::Read(0x2dead000)
            })
        );
        assert!(wake.interrupt_flag().load(Ordering::Acquire));
        cpu.set_host_interrupts(true);
        cpu.set_reg(Reg::Eip, CODE);
        wake.take_interrupt();
        handler.park = true;
        assert_eq!(cpu.run(&mut handler), Exit::Parked);
    }
}
