//! Production execution contracts; this file never constructs a reference engine.
use adv_jit::{EXEC, Machine, READ, Region, RunOutcome, WRITE};

const CODE: u32 = 0x1000;
const STOP: u32 = 0x3000;

fn machine(bytes: &[u8]) -> Machine {
    let mut m = Machine::new(CODE, 0x4000).unwrap();
    m.protect(CODE, 0x4000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 0x1000, READ | EXEC).unwrap();
    m.write(0x4f00, &STOP.to_le_bytes()).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = 0x4f00;
    m
}

#[test]
fn cold_code_executes_without_native_compilation_or_reference_engine() {
    // mov eax,-101; cdq; mov ecx,7; idiv ecx; ret
    let mut m = machine(&[
        0xb8, 0x9b, 0xff, 0xff, 0xff, 0x99, 0xb9, 7, 0, 0, 0, 0xf7, 0xf9, 0xc3,
    ]);
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0] as i32, -14);
    assert_eq!(m.state.regs[2] as i32, -3);
    assert_eq!(m.stats.compilations, 0);
    assert_eq!(m.stats.interpreter_instructions, 5);
}

#[test]
fn debug_step_overrides_a_cached_native_region() {
    let mut m = machine(&[0x40, 0x40, 0xc3]);
    m.set_region(Region::new(CODE, 3)).unwrap();
    m.state.eflags |= 1 << 8;
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::Trap {
            pc: CODE,
            vector: 1
        }
    );
    assert_eq!(m.state.eip, CODE + 1);
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.stats.native_entries, 0);
    m.state.eflags &= !(1 << 8);
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 2);
}

#[test]
fn divide_fault_keeps_prior_progress_and_does_not_repeat_it_after_repair() {
    // inc ebx; idiv ecx; ret
    let mut m = machine(&[0x43, 0xf7, 0xf9, 0xc3]);
    m.state.regs[0] = 9;
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::Trap {
            pc: CODE + 1,
            vector: 0
        }
    );
    assert_eq!(m.state.eip, CODE + 1);
    assert_eq!(m.state.regs[3], 1);
    m.state.regs[1] = 3;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 3);
    assert_eq!(m.state.regs[3], 1);
}

#[test]
fn pending_x87_exception_cannot_be_bypassed_by_cached_mmx() {
    // inc eax; pxor mm0,mm0; ret. Integer progress precedes precise #MF.
    let mut m = machine(&[0x40, 0x0f, 0xef, 0xc0, 0xc3]);
    m.set_region(Region::new(CODE, 5)).unwrap();
    m.state.x87.control &= !1;
    m.state.x87.status = 0x81;
    let registers = m.state.x87.regs;
    assert_eq!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::Trap {
            pc: CODE + 1,
            vector: 16
        }
    );
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.state.x87.regs, registers);
    assert_eq!(m.stats.native_entries, 0);
    m.state.x87.status = 0;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 1);
}

#[test]
fn complete_guest_address_space_includes_the_final_page() {
    let mut m = Machine::address_space().unwrap();
    m.map(0xffff_f000, 4096, READ | WRITE).unwrap();
    m.write(u32::MAX, &[0x5a]).unwrap();
    assert_eq!(m.memory().read(u32::MAX, 1).unwrap(), &[0x5a]);
    assert!(m.memory().read(u32::MAX, 2).is_err());
    m.unmap(0xffff_f000, 4096).unwrap();
    assert!(m.memory().read(u32::MAX, 1).is_err());
}

#[test]
fn native_guard_with_one_unit_yields_then_delivers_the_precise_fault() {
    // div ecx is native, but a zero divisor must execute its precise slow path.
    let mut m = machine(&[0xf7, 0xf1, 0xc3]);
    m.set_region(Region::new(CODE, 3)).unwrap();
    m.state.regs[0] = 9;
    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
    assert_eq!(m.state.eip, CODE);
    assert_eq!(
        m.run(STOP, 1).unwrap(),
        RunOutcome::Trap {
            pc: CODE,
            vector: 0
        }
    );
    assert_eq!(m.stats.native_entries, 1);
}

#[test]
fn feature_queries_expose_reference_tsc_without_host_avx_or_rdtscp() {
    for leaf in [0, 1, 7, 0x8000_0000, 0x8000_0001] {
        let mut m = machine(&[0x0f, 0xa2, 0xc3]);
        m.state.regs[0] = leaf;
        m.state.eflags = 0x8d7;
        assert_eq!(m.run(STOP, 10).unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.state.eflags, 0x8d7);
        match leaf {
            0 => {
                assert_eq!(m.state.regs[0], 1);
                let vendor: Vec<_> = [3, 2, 1]
                    .into_iter()
                    .flat_map(|r| m.state.regs[r].to_le_bytes())
                    .collect();
                assert_eq!(vendor, b"GenuineIntel");
            }
            1 => {
                assert_eq!(m.state.regs[1], 0);
                assert_ne!(m.state.regs[2] & (1 << 4), 0);
                assert_ne!(m.state.regs[2] & (1 << 26), 0);
            }
            0x8000_0000 => assert_eq!(m.state.regs[0], leaf),
            _ => assert_eq!(&m.state.regs[..4], &[0; 4]),
        }
    }
}

#[cfg(feature = "pal")]
#[test]
fn raw_api_enabling_single_step_exits_native_before_the_next_instruction() {
    use adv_jit::pal::JitCpu;
    use pal_cpu::{Backend, Exit, Guest, Prot, Reg, TrapAction, TrapHandler};
    struct Debugger {
        traps: Vec<(u8, u32, u32)>,
    }
    impl TrapHandler for Debugger {
        fn on_trap(&mut self, g: &mut dyn Guest, vector: u8) -> TrapAction {
            self.traps.push((vector, g.reg(Reg::Eip), g.reg(Reg::Eax)));
            if vector == 0x2e {
                g.set_reg(Reg::Eflags, g.reg(Reg::Eflags) | (1 << 8));
                TrapAction::Continue
            } else {
                assert_eq!(vector, 1);
                TrapAction::Park
            }
        }
    }
    let mut cpu = JitCpu::new(STOP, true).unwrap();
    cpu.map(CODE, 4096, Prot::RX).unwrap();
    cpu.write(CODE, &[0xcd, 0x2e, 0x40, 0x40]).unwrap();
    cpu.machine_mut().set_region(Region::new(CODE, 4)).unwrap();
    cpu.set_reg(Reg::Eip, CODE);
    let mut debugger = Debugger { traps: Vec::new() };
    assert_eq!(cpu.run(&mut debugger), Exit::Parked);
    assert_eq!(debugger.traps, [(0x2e, CODE + 2, 0), (1, CODE + 3, 1)]);
}
