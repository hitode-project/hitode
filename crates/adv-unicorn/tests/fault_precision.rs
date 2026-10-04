//! A continued host trap must not disable subsequent fault-PC recovery.
use adv_cpu::backend::{
    Backend, Exit, Fault, FaultKind, Guest, GuestExt, Prot, Reg, TrapAction, TrapHandler,
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x10000;
const PAGE: u32 = 0x20000;
const DATA: u32 = 0x30000;
const STACK: u32 = 0x40000;
const PARK: u32 = 0x50000;

struct Continue {
    calls: usize,
    restore: bool,
}
impl TrapHandler for Continue {
    fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
        assert_eq!(vector, 0x80);
        assert_eq!(cpu.reg(Reg::Eip), CODE + 2);
        self.calls += 1;
        if self.restore {
            // Writing EIP from an interrupt hook leaves Unicorn 2.1's internal
            // skip_sync_pc_on_exit set for the remainder of that emu_start.
            let state = cpu.capture_architecture().unwrap();
            cpu.restore_architecture(&state).unwrap();
        }
        TrapAction::Continue
    }
}

#[test]
fn continued_trap_preserves_read_and_write_fault_pc_and_committed_prefix() {
    for restore in [false, true] {
        for store in [false, true] {
            for mapped in [false, true] {
                let mut cpu = UnicornCpu::new(PARK).unwrap();
                for (addr, prot) in [
                    (CODE, Prot::RX),
                    (DATA, Prot::RW),
                    (STACK, Prot::RW),
                    (PARK, Prot::RX),
                ] {
                    cpu.map(addr, 4096, prot).unwrap();
                }
                if mapped {
                    cpu.map(PAGE, 4096, Prot(0)).unwrap();
                }
                let mut code = vec![0xcd, 0x80, 0xb8, 7, 0, 0, 0, 0x83, 0xc0, 4, 0xb9];
                code.extend(PAGE.to_le_bytes());
                code.extend([0xff, 0x05]); // INC dword [DATA]: prefix commits once.
                code.extend(DATA.to_le_bytes());
                for offset in [4u32, 8] {
                    code.extend([0xc7, 0x05]);
                    code.extend((DATA + offset).to_le_bytes());
                    code.extend(0x12345678u32.to_le_bytes());
                }
                let fault_pc = CODE + code.len() as u32;
                code.extend(if store { [0x89, 0x01] } else { [0x8b, 0x01] });
                code.push(0xc3);
                cpu.write(CODE, &code).unwrap();
                cpu.write_u32(STACK, PARK).unwrap();
                cpu.set_reg(Reg::Esp, STACK);
                cpu.set_reg(Reg::Eip, CODE);
                let mut handler = Continue { calls: 0, restore };
                assert_eq!(
                    cpu.run(&mut handler),
                    Exit::Fault(Fault {
                        kind: if store {
                            FaultKind::Write(PAGE)
                        } else {
                            FaultKind::Read(PAGE)
                        },
                        pc: fault_pc,
                    })
                );
                assert_eq!(cpu.capture_architecture().unwrap().eip, fault_pc);
                assert_eq!(cpu.reg(Reg::Eax), 11);
                assert_eq!(cpu.read_u32(DATA).unwrap(), 1);
                assert_eq!(cpu.read_u32(DATA + 4).unwrap(), 0x12345678);
                assert_eq!(cpu.read_u32(DATA + 8).unwrap(), 0x12345678);
                if mapped {
                    cpu.protect(PAGE, 4096, Prot::RW).unwrap();
                } else {
                    cpu.map(PAGE, 4096, Prot::RW).unwrap();
                }
                assert_eq!(cpu.run(&mut handler), Exit::Parked);
                assert_eq!(handler.calls, 1);
                assert_eq!(
                    cpu.read_u32(DATA).unwrap(),
                    1,
                    "fault retry must not replay prefix stores"
                );
                assert_eq!(cpu.read_u32(PAGE).unwrap(), if store { 11 } else { 0 });
            }
        }
    }
}

#[test]
#[ignore = "reference-only trap restart cost diagnostic; no timing gate"]
fn reference_trap_restart_cost() {
    use adv_unicorn::Guarded;
    use std::time::Instant;
    use unicorn_engine::{
        RegisterX86,
        unicorn_const::{Arch, HookType, Mode, Prot as UcProt},
    };

    #[derive(Default)]
    struct Data {
        continued: bool,
        calls: usize,
    }
    const TRAPS: u32 = 100_000;
    for restart in [false, true] {
        let mut cpu = Guarded::new_with_data(Arch::X86, Mode::MODE_32, Data::default()).unwrap();
        cpu.mem_map(u64::from(CODE), 4096, UcProt::READ | UcProt::EXEC)
            .unwrap();
        cpu.mem_map(u64::from(PARK), 4096, UcProt::READ | UcProt::EXEC)
            .unwrap();
        let mut code = vec![0xb9];
        code.extend(TRAPS.to_le_bytes());
        // MOV ECX,N; INT 80; DEC ECX; JNZ -5; INT 81.
        code.extend([0xcd, 0x80, 0x49, 0x75, 0xfb, 0xcd, 0x81]);
        cpu.mem_write(u64::from(CODE), &code).unwrap();
        cpu.add_block_hook(1, 0, |_, _, _| {}).unwrap();
        cpu.add_mem_hook(
            HookType::MEM_READ | HookType::MEM_WRITE,
            1,
            0,
            |_, _, _, _, _| true,
        )
        .unwrap();
        cpu.add_intr_hook(move |uc, vector| {
            if vector == 0x81 {
                uc.set_pc(u64::from(PARK)).unwrap();
            } else {
                assert_eq!(vector, 0x80);
                uc.get_data_mut().calls += 1;
                let pc = uc.reg_read(RegisterX86::EIP).unwrap();
                uc.set_pc(pc).unwrap(); // Same host-origin PC update in both lanes.
                if restart {
                    uc.get_data_mut().continued = true;
                    uc.emu_stop().unwrap();
                }
            }
        })
        .unwrap();
        let mut pc = u64::from(CODE);
        let start = Instant::now();
        loop {
            cpu.get_data_mut().continued = false;
            cpu.emu_start(pc, u64::from(PARK), 0, 0).unwrap();
            pc = cpu.reg_read(RegisterX86::EIP).unwrap();
            if pc == u64::from(PARK) {
                break;
            }
            assert!(restart && cpu.get_data().continued);
        }
        assert_eq!(cpu.get_data().calls, TRAPS as usize);
        assert_eq!(cpu.reg_read(RegisterX86::ECX).unwrap(), 0);
        println!(
            "restart={restart} ns_per_trap={:.1}",
            start.elapsed().as_secs_f64() * 1e9 / f64::from(TRAPS)
        );
    }
}
