//! Backend contract tests: traps, parking, per-context FS, faults.

use std::time::{Duration, Instant};

use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{
    Backend, Exit, FaultKind, Guest, GuestExt, Prot, Reg, SegmentSetup, TrapAction, TrapHandler,
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;
const THUNKS: u32 = 0x0050_0000;
const PARK: u32 = THUNKS + 0xff0;
const STACK_TOP: u32 = 0x0020_0000;
const GDT: u32 = 0x0060_0000;
const TEB1: u32 = 0x7ffd_e000;
const TEB2: u32 = 0x7ffd_d000;

// Thunk indices.
const DOUBLE: u32 = 0; // returns arg0 * 2, fast path
const DONE: u32 = 1; // parks
const BLOCK: u32 = 2; // parks mid-call; the host completes it

fn thunk(i: u32) -> u32 {
    THUNKS + 8 * i
}

fn machine(code: impl FnOnce(&mut Asm)) -> UnicornCpu {
    let mut cpu = UnicornCpu::new(PARK).unwrap();
    cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    cpu.map(THUNKS, 0x1000, Prot::RX).unwrap();
    cpu.map(STACK_TOP - 0x10000, 0x10000, Prot::RW).unwrap();
    cpu.map(GDT, 0x1000, Prot::RW).unwrap();
    for teb in [TEB1, TEB2] {
        cpu.map(teb, 0x1000, Prot::RW).unwrap();
        cpu.write_u32(teb + 0x18, teb).unwrap(); // NT_TIB.Self
    }
    let mut t = Asm::new(THUNKS);
    for _ in 0..3 {
        t.trap().ret().pad_to(8, 0xcc);
    }
    cpu.write(THUNKS, &t.bytes).unwrap();
    cpu.write(PARK, &[0xf4]).unwrap();
    let mut a = Asm::new(CODE);
    code(&mut a);
    cpu.write(CODE, &a.bytes).unwrap();
    cpu.setup_segments(SegmentSetup { gdt_addr: GDT }).unwrap();
    cpu.set_reg(Reg::Esp, STACK_TOP - 0x100);
    cpu.set_reg(Reg::Eip, CODE);
    cpu
}

#[derive(Default)]
struct Log {
    traps: Vec<(u32, u32)>, // (thunk index, eax at trap)
}

impl TrapHandler for Log {
    fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
        assert_eq!(vector, 0x2e);
        let idx = (cpu.reg(Reg::Eip) - 2 - THUNKS) / 8;
        self.traps.push((idx, cpu.reg(Reg::Eax)));
        match idx {
            DOUBLE => {
                let a = cpu.arg(0).unwrap();
                cpu.set_reg(Reg::Eax, a * 2);
                TrapAction::Continue
            }
            _ => TrapAction::Park,
        }
    }
}

#[test]
fn fast_path_call_returns_value() {
    let mut cpu = machine(|a| {
        a.push_imm(21)
            .call(thunk(DOUBLE))
            .add_esp(4)
            .mov_ebx_eax()
            .call(thunk(DONE));
    });
    let mut log = Log::default();
    assert_eq!(cpu.run(&mut log), Exit::Parked);
    assert_eq!(cpu.reg(Reg::Ebx), 42);
    assert_eq!(cpu.reg(Reg::Eip), PARK);
    assert_eq!(
        log.traps.iter().map(|t| t.0).collect::<Vec<_>>(),
        [DOUBLE, DONE]
    );
}

#[test]
fn access_checks_include_permissions_gaps_and_overflow() {
    let mut cpu = machine(|_| {});
    let base = 0x2000_0000;
    cpu.map(base, 0x1000, Prot::RW).unwrap();
    cpu.map(base + 0x1000, 0x1000, Prot::READ).unwrap();
    assert!(cpu.accessible(base + 0xfff, 2, Prot::READ));
    assert!(!cpu.accessible(base + 0xfff, 2, Prot::WRITE));
    assert!(cpu.accessible(base, 0x1000, Prot::RW));
    assert!(!cpu.accessible(base, 0x2001, Prot::READ));
    assert!(!cpu.accessible(base + 0x2000, 1, Prot::READ));
    assert!(!cpu.accessible(u32::MAX, 2, Prot::READ));
    cpu.protect(base + 0x1000, 0x1000, Prot::RW).unwrap();
    assert!(cpu.accessible(base, 0x2000, Prot::RW));
    cpu.unmap(base, 0x1000).unwrap();
    assert!(!cpu.accessible(base, 0x2000, Prot::READ));
}

#[test]
fn parked_call_is_completed_by_host() {
    let mut cpu = machine(|a| {
        a.push_imm(5)
            .call(thunk(BLOCK))
            .add_esp(4)
            .mov_ebx_eax()
            .call(thunk(DONE));
    });
    let mut log = Log::default();
    assert_eq!(cpu.run(&mut log), Exit::Parked);
    // Parked inside BLOCK: the return address is still on the stack, args above it.
    assert_eq!(cpu.arg(0).unwrap(), 5);
    // Complete the call: result in eax, resume at the thunk's `ret`.
    cpu.set_reg(Reg::Eax, 0x1234);
    cpu.set_reg(Reg::Eip, thunk(BLOCK) + 2);
    assert_eq!(cpu.run(&mut log), Exit::Parked);
    assert_eq!(cpu.reg(Reg::Ebx), 0x1234);
    assert_eq!(log.traps.last().unwrap().0, DONE);
}

/// Each guest context has its own TEB; FS must follow context switches.
#[test]
fn fs_base_is_per_context() {
    let mut cpu = machine(|a| {
        a.mov_eax_fs(0x18).call(thunk(DONE));
    });
    cpu.set_fs_base(TEB1).unwrap();
    let mut c1 = cpu.new_context();
    cpu.save(&mut c1);
    cpu.set_fs_base(TEB2).unwrap();
    let mut c2 = cpu.new_context();
    cpu.save(&mut c2);

    let mut log = Log::default();
    // Restore only; no set_fs_base. The saved context must carry the hidden FS base.
    for (ctx, teb) in [(&c1, TEB1), (&c2, TEB2), (&c1, TEB1)] {
        cpu.restore(ctx);
        assert_eq!(cpu.run(&mut log), Exit::Parked);
        assert_eq!(
            log.traps.last().unwrap().1,
            teb,
            "fs:[0x18] after restoring context"
        );
    }
}

#[test]
fn sidt_queries_the_owned_table_with_both_operand_sizes_and_saved_contexts() {
    let output = TEB1 + 0x100;
    let mut cpu = machine(|a| {
        a.bytes.extend_from_slice(&[0x0f, 0x01, 0x0d]);
        a.bytes.extend_from_slice(&(output + 1).to_le_bytes());
        a.bytes.extend_from_slice(&[0x66, 0x0f, 0x01, 0x0d]);
        a.bytes.extend_from_slice(&(output + 11).to_le_bytes());
        a.call(thunk(DONE));
    });
    let idt = GDT + SegmentSetup::IDT_OFFSET;
    let mut gates = [0xff; SegmentSetup::IDT_LIMIT as usize + 1];
    cpu.read(idt, &mut gates).unwrap();
    assert!(gates.iter().all(|&byte| byte == 0));
    cpu.set_reg(Reg::Eax, 0x1357_2468);
    cpu.set_reg(Reg::Eflags, 0x8d7);
    let mut context = cpu.new_context();
    cpu.save(&mut context);
    for _ in 0..2 {
        cpu.restore(&context);
        cpu.write(output, &[0xa5; 18]).unwrap();
        assert_eq!(cpu.run(&mut Log::default()), Exit::Parked);
        for destination in [output + 1, output + 11] {
            assert_eq!(cpu.read_u16(destination).unwrap(), SegmentSetup::IDT_LIMIT);
            assert_eq!(cpu.read_u32(destination + 2).unwrap(), idt);
        }
        let mut bytes = [0; 18];
        cpu.read(output, &mut bytes).unwrap();
        for index in [0, 7, 10, 17] {
            assert_eq!(bytes[index], 0xa5);
        }
        assert_eq!(cpu.reg(Reg::Eax), 0x1357_2468);
        assert_eq!(cpu.reg(Reg::Eflags), 0x8d7);
    }
}

#[test]
fn unmapped_read_faults_with_address() {
    let mut cpu = machine(|a| {
        a.mov_eax_mem(0x0bad_0000).call(thunk(DONE));
    });
    let exit = cpu.run(&mut Log::default());
    let Exit::Fault(f) = exit else {
        panic!("expected fault, got {exit:?}")
    };
    assert_eq!(f.kind, FaultKind::Read(0x0bad_0000));
    assert_eq!(f.pc, CODE);
}

#[test]
fn pure_loop_exhausts_wall_budget_at_its_pc() {
    let mut cpu = machine(|a| {
        a.bytes.extend_from_slice(&[0xeb, 0xfe]); // jmp $
    });
    let budget = Duration::from_millis(30);
    cpu.set_execution_budget(Some(budget));
    let start = Instant::now();
    let mut log = Log::default();
    assert_eq!(cpu.run(&mut log), Exit::BudgetExceeded { pc: CODE, budget });
    assert!(start.elapsed() >= budget);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "watchdog did not stop promptly"
    );
    assert_eq!(cpu.reg(Reg::Eip), CODE);
    assert!(log.traps.is_empty());
}

#[test]
fn expiration_and_rearming_do_not_leak_stop_requests() {
    let mut cpu = machine(|a| {
        a.bytes.extend_from_slice(&[0xeb, 0xfe]);
        a.pad_to(16, 0xcc).call(thunk(DONE));
    });
    let mut log = Log::default();
    // Tiny budgets exercise cancellation around emu_start's stop-request reset.
    for _ in 0..100 {
        let budget = Duration::from_micros(1);
        cpu.set_execution_budget(Some(budget));
        cpu.set_reg(Reg::Eip, CODE);
        assert_eq!(cpu.run(&mut log), Exit::BudgetExceeded { pc: CODE, budget });
        cpu.set_execution_budget(Some(Duration::from_secs(2)));
        cpu.set_reg(Reg::Esp, STACK_TOP - 0x100);
        cpu.set_reg(Reg::Eip, CODE + 16);
        assert_eq!(cpu.run(&mut log), Exit::Parked);
    }
    cpu.set_execution_budget(None);
    cpu.set_reg(Reg::Eip, CODE + 16);
    assert_eq!(cpu.run(&mut log), Exit::Parked);
}

#[test]
fn normal_parks_win_when_timer_expires_in_host_handler() {
    struct SlowPark;
    impl TrapHandler for SlowPark {
        fn on_trap(&mut self, _: &mut dyn Guest, _: u8) -> TrapAction {
            std::thread::sleep(Duration::from_millis(30));
            TrapAction::Park
        }
    }
    let mut cpu = machine(|a| {
        a.call(thunk(DONE));
    });
    cpu.set_execution_budget(Some(Duration::from_millis(5)));
    assert_eq!(cpu.run(&mut SlowPark), Exit::Parked);
    // Wait past a cancelled deadline, then reuse the engine and finally drop it while
    // the timer is asleep on a much later deadline. Neither path may issue a stale stop.
    std::thread::sleep(Duration::from_millis(10));
    cpu.set_execution_budget(Some(Duration::from_secs(3600)));
    cpu.set_reg(Reg::Eip, CODE);
    assert_eq!(cpu.run(&mut Log::default()), Exit::Parked);
    let start = Instant::now();
    drop(cpu);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn zero_budget_stops_before_guest_execution() {
    let mut cpu = machine(|a| {
        a.call(thunk(DONE));
    });
    cpu.set_execution_budget(Some(Duration::ZERO));
    let mut log = Log::default();
    assert_eq!(
        cpu.run(&mut log),
        Exit::BudgetExceeded {
            pc: CODE,
            budget: Duration::ZERO
        }
    );
    assert!(log.traps.is_empty());
    cpu.set_execution_budget(None);
    assert_eq!(cpu.run(&mut log), Exit::Parked);
}

#[test]
fn dense_traps_and_parks_complete_with_budget_enabled() {
    let n = 20_000;
    for target in [DOUBLE, BLOCK] {
        let mut cpu = machine(|a| {
            a.mov_ecx_imm(n);
            let top = a.here();
            a.push_imm(21)
                .call(thunk(target))
                .add_esp(4)
                .dec_ecx()
                .jnz(top)
                .call(thunk(DONE));
        });
        cpu.set_execution_budget(Some(Duration::from_secs(2)));
        let mut log = Log::default();
        for _ in 0..if target == BLOCK { n } else { 0 } {
            assert_eq!(cpu.run(&mut log), Exit::Parked);
            cpu.set_reg(Reg::Eip, thunk(BLOCK) + 2);
        }
        assert_eq!(cpu.run(&mut log), Exit::Parked);
        assert_eq!(cpu.reg(Reg::Ecx), 0);
        assert_eq!(log.traps.len(), n as usize + 1);
    }
}

/// A fresh CPU's x87 control word and MXCSR, as a new Windows thread sees them.
#[test]
fn fpu_control_words_start_as_on_windows() {
    const OUT: u32 = STACK_TOP - 0x20;
    let mut cpu = machine(|a| {
        a.bytes.extend_from_slice(&[0xd9, 0x3d]); // fnstcw [OUT]
        a.bytes.extend_from_slice(&OUT.to_le_bytes());
        a.bytes.extend_from_slice(&[0x0f, 0xae, 0x1d]); // stmxcsr [OUT + 4]
        a.bytes.extend_from_slice(&(OUT + 4).to_le_bytes());
        a.call(thunk(DONE));
    });
    assert_eq!(cpu.run(&mut Log::default()), Exit::Parked);
    let (cw, mxcsr) = (cpu.read_u16(OUT).unwrap(), cpu.read_u32(OUT + 4).unwrap());
    assert_eq!((cw, mxcsr), (0x27f, 0x1f80));
}
