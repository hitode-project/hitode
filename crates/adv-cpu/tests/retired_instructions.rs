//! Completed-instruction counts (PROC-CLOCK-5): the native backend counts the
//! same instructions in every tier, so equal loop iterations count alike.

use adv_cpu::JitCpu;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{
    Backend, Exit, Guest, GuestExt, Prot, Reg, SegmentSetup, TrapAction, TrapHandler,
};

const CODE: u32 = 0x0040_0000;
const THUNKS: u32 = 0x0050_0000;
const PARK: u32 = THUNKS + 0xff0;
const GDT: u32 = 0x0070_0000;
const STACK_TOP: u32 = 0x0020_0000;
const TEB: u32 = 0x7ffd_e000;

fn load(cpu: &mut JitCpu) {
    cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    cpu.map(THUNKS, 0x1000, Prot::RX).unwrap();
    cpu.map(GDT, 0x1000, Prot::RW).unwrap();
    cpu.map(STACK_TOP - 0x10000, 0x10000, Prot::RW).unwrap();
    cpu.map(TEB, 0x1000, Prot::RW).unwrap();
    cpu.write_u32(TEB + 0x18, TEB).unwrap();
    let mut t = Asm::new(THUNKS);
    t.trap().ret();
    cpu.write(THUNKS, &t.bytes).unwrap();
    cpu.write(PARK, &[0xf4]).unwrap();
    cpu.setup_segments(SegmentSetup { gdt_addr: GDT }).unwrap();
    cpu.set_fs_base(TEB).unwrap();
}

/// Spin `spins` times, then park through the thunk.
fn program(cpu: &mut JitCpu, spins: u32) {
    let mut a = Asm::new(CODE);
    a.mov_ecx_imm(spins);
    let top = a.here();
    a.dec_ecx().jnz(top).call(THUNKS);
    cpu.write(CODE, &a.bytes).unwrap();
}

struct Host;

impl TrapHandler for Host {
    fn on_trap(&mut self, _cpu: &mut dyn Guest, _vector: u8) -> TrapAction {
        TrapAction::Park
    }
}

/// Instructions completed by one pass of the program.
fn pass(cpu: &mut JitCpu) -> u64 {
    cpu.set_reg(Reg::Esp, STACK_TOP - 0x100);
    cpu.set_reg(Reg::Eip, CODE);
    let before = cpu.retired_instructions();
    assert!(matches!(cpu.run(&mut Host), Exit::Parked));
    cpu.retired_instructions() - before
}

// spec: PROC-CLOCK-5
#[test]
fn every_tier_counts_the_same_instructions() {
    let mut cpu = JitCpu::new(PARK, true).unwrap();
    load(&mut cpu);
    program(&mut cpu, 3);
    let first = pass(&mut cpu);
    assert!(first > 0);
    // Repeated passes give the tiered backend the chance to run translated code.
    for _ in 0..16 {
        assert_eq!(pass(&mut cpu), first);
    }
    program(&mut cpu, 4);
    assert_eq!(
        pass(&mut cpu),
        first + 2,
        "one more spin is one dec and one jnz"
    );
}
