use std::time::Duration;

use adv_cpu::JitCpu;
use adv_cpu::backend::{
    Backend, Exit, Fault, FaultKind, Guest, GuestExt, Prot, Reg, SegmentSetup, TrapAction,
    TrapHandler,
};
use iced_x86::code_asm::*;

mod support;

const CODE: u32 = 0x0040_0000;
const API: u32 = 0x6000_0000;
const DATA: u32 = 0x6100_0000;
const STACK: u32 = 0x7000_0000;
const PARK: u32 = 0x7e10_0000;

struct UnexpectedTrap;
impl TrapHandler for UnexpectedTrap {
    fn on_trap(&mut self, _: &mut dyn Guest, vector: u8) -> TrapAction {
        panic!("unexpected interrupt {vector:#x}");
    }
}

fn cpu(jit: bool) -> JitCpu {
    let mut cpu = JitCpu::new(PARK, jit).unwrap();
    cpu.set_execution_budget(Some(Duration::from_secs(2)));
    cpu.map(PARK, 4096, Prot::RX).unwrap();
    cpu.map(STACK, 4096, Prot::RW).unwrap();
    cpu
}

fn start(cpu: &mut JitCpu, pc: u32) {
    cpu.write_u32(STACK + 0x800, PARK).unwrap();
    cpu.set_reg(Reg::Esp, STACK + 0x800);
    cpu.set_reg(Reg::Eip, pc);
}

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

#[test]
fn scattered_mapping_permissions_and_remapping_match_execution() {
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        for at in [0x1000, CODE, 0x8000_0000, 0xfffe_e000] {
            cpu.map(at, 4096, Prot::RW).unwrap();
            assert!(cpu.accessible(at, 4096, Prot::RW));
            assert!(!cpu.accessible(at, 1, Prot::EXEC));
            assert!(cpu.map(at, 4096, Prot::RW).is_err());
            cpu.write(at, &[0xb8, 19, 0, 0, 0, 0xc3]).unwrap();
            cpu.protect(at, 4096, Prot::RX).unwrap();
            assert!(!cpu.accessible(at, 1, Prot::WRITE));
            start(&mut cpu, at);
            assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
            assert_eq!(cpu.reg(Reg::Eax), 19);
            assert_eq!(cpu.reg(Reg::Esp), STACK + 0x804);
            cpu.unmap(at, 4096).unwrap();
            assert!(!cpu.accessible(at, 1, Prot::READ));
            assert!(cpu.read_u32(at).is_err());
            cpu.map(at, 4096, Prot::RW).unwrap();
            assert_eq!(cpu.read_u32(at).unwrap(), 0);
            cpu.write(at, &[0xb8, 31, 0, 0, 0, 0xc3]).unwrap();
            cpu.protect(at, 4096, Prot::RX).unwrap();
            start(&mut cpu, at);
            assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
            assert_eq!(
                cpu.reg(Reg::Eax),
                31,
                "remapping must invalidate old code at {at:#x}"
            );
            cpu.unmap(at, 4096).unwrap();
        }
        // No private reference-engine GDT reservation remains in this address space.
        cpu.map(0xffff_0000, 4096, Prot::RW).unwrap();
        cpu.unmap(0xffff_0000, 4096).unwrap();
        assert!(cpu.map(CODE + 1, 4096, Prot::RW).is_err());
        assert!(cpu.accessible(STACK, 4096, Prot::RW));
        assert!(!cpu.accessible(STACK + 4092, 8, Prot::READ));
    }
}

#[test]
fn raw_traps_obey_stdcall_cdecl_and_callback_pc_stack_changes() {
    struct Calls {
        origin: Option<(u32, u32)>,
        pcs: Vec<u32>,
    }
    impl TrapHandler for Calls {
        fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
            assert_eq!(vector, 0x2e);
            let pc = cpu.reg(Reg::Eip);
            self.pcs.push(pc);
            match pc {
                p if p == API + 2 => cpu.set_reg(Reg::Eax, cpu.arg(0).unwrap() + 7),
                p if p == API + 18 => cpu.set_reg(Reg::Eax, cpu.arg(0).unwrap() * 2),
                p if p == API + 34 => {
                    let guest_sp = cpu.reg(Reg::Esp);
                    let arg = cpu.arg(0).unwrap();
                    self.origin = Some((cpu.read_u32(guest_sp).unwrap(), guest_sp + 8));
                    cpu.write_u32(guest_sp - 4, API + 48).unwrap();
                    cpu.write_u32(guest_sp, arg).unwrap();
                    cpu.set_reg(Reg::Esp, guest_sp - 4);
                    cpu.set_reg(Reg::Eip, CODE + 0x100);
                }
                p if p == API + 50 => {
                    let (pc, guest_sp) = self.origin.take().unwrap();
                    assert_eq!(cpu.reg(Reg::Esp), guest_sp - 4);
                    cpu.set_reg(Reg::Eax, cpu.reg(Reg::Eax) + 100);
                    cpu.set_reg(Reg::Esp, guest_sp);
                    cpu.set_reg(Reg::Eip, pc);
                }
                _ => panic!("wrong trap PC {pc:#x}"),
            }
            TrapAction::Continue
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(API, 4096, Prot::RX).unwrap();
        cpu.write(API, &[0xcd, 0x2e, 0xc2, 4, 0]).unwrap();
        cpu.write(API + 16, &[0xcd, 0x2e, 0xc3]).unwrap();
        cpu.write(API + 32, &[0xcd, 0x2e, 0xc2, 4, 0]).unwrap();
        cpu.write(API + 48, &[0xcd, 0x2e, 0xf4]).unwrap();
        cpu.write(
            CODE,
            &assemble(CODE, |a| {
                a.push(4).unwrap();
                a.call(u64::from(API)).unwrap();
                a.push(eax).unwrap();
                a.call(u64::from(API + 16)).unwrap();
                a.add(esp, 4).unwrap();
                a.push(eax).unwrap();
                a.call(u64::from(API + 32)).unwrap();
                a.add(eax, 3).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        cpu.write(
            CODE + 0x100,
            &assemble(CODE + 0x100, |a| {
                a.mov(eax, dword_ptr(esp + 4)).unwrap();
                a.add(eax, 5).unwrap();
                a.ret_1(4).unwrap();
            }),
        )
        .unwrap();
        if jit {
            let mut main = adv_cpu::Region::new(CODE, 0x120);
            main.entries.push(CODE + 0x100);
            let mut stubs = adv_cpu::Region::new(API, 0x60);
            stubs.entries.extend([API + 16, API + 32, API + 48]);
            cpu.executor_mut().add_region(main).unwrap();
            cpu.executor_mut().add_region(stubs).unwrap();
        }
        start(&mut cpu, CODE);
        let mut calls = Calls {
            origin: None,
            pcs: vec![],
        };
        assert_eq!(cpu.run(&mut calls), Exit::Parked);
        assert_eq!(cpu.reg(Reg::Eax), 130);
        assert_eq!(cpu.reg(Reg::Esp), STACK + 0x804);
        assert_eq!(calls.pcs, [API + 2, API + 18, API + 34, API + 50]);
    }
}

#[test]
fn trap_mapping_new_rwx_page_tracks_actual_guest_code_writes() {
    struct Allocate;
    impl TrapHandler for Allocate {
        fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
            assert_eq!(vector, 0x2e);
            assert_eq!(cpu.reg(Reg::Eip), API + 2);
            cpu.map(DATA, 8192, Prot::RW).unwrap();
            cpu.write(DATA, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
            cpu.protect(DATA, 4096, Prot::RWX).unwrap();
            cpu.unmap(DATA + 4096, 4096).unwrap();
            assert!(!cpu.accessible(DATA + 4096, 1, Prot::READ));
            TrapAction::Continue
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);

        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(API, 4096, Prot::RX).unwrap();
        cpu.write(API, &[0xcd, 0x2e, 0xc3]).unwrap();
        cpu.write(
            CODE,
            &assemble(CODE, |a| {
                a.call(u64::from(API)).unwrap();
                let mut warm = a.create_label();
                a.mov(ecx, 40).unwrap();
                a.set_label(&mut warm).unwrap();
                a.call(u64::from(DATA)).unwrap();
                a.dec(ecx).unwrap();
                a.jnz(warm).unwrap();
                a.mov(ebx, eax).unwrap();
                a.mov(dword_ptr(DATA as i32 + 1), 99).unwrap();
                a.call(u64::from(DATA)).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, 0x100))
                .unwrap();
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(API, 3))
                .unwrap();
        }
        start(&mut cpu, CODE);
        assert_eq!(cpu.run(&mut Allocate), Exit::Parked);
        assert_eq!(cpu.reg(Reg::Ebx), 1);
        assert_eq!(
            cpu.reg(Reg::Eax),
            99,
            "new RWX mapping must be monitored before fallback writes"
        );
        if jit {
            assert!(cpu.stats().invalidations >= 1);
            assert!(
                cpu.stats().interpreter_entries >= 1,
                "guest writes to executable pages must leave native code"
            );
        }
    }
}

#[test]
fn host_patch_invalidates_both_warm_native_and_fallback_code() {
    struct Patch;
    impl TrapHandler for Patch {
        fn on_trap(&mut self, cpu: &mut dyn Guest, _: u8) -> TrapAction {
            cpu.write_u32(DATA + 1, 73).unwrap();
            cpu.write(DATA + 0x100 + 3, &47u16.to_le_bytes()).unwrap();
            TrapAction::Continue
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(DATA, 4096, Prot::RX).unwrap();
        cpu.map(API, 4096, Prot::RX).unwrap();
        cpu.write(DATA, &[0xb8, 1, 0, 0, 0, 0xc3]).unwrap();
        // The 16-bit IMUL form stays in the interpreter; warm its decode too.
        cpu.write(DATA + 0x100, &[0x66, 0x69, 0xc0, 3, 0, 0xc3])
            .unwrap();
        cpu.write(API, &[0xcd, 0x2e, 0xc3]).unwrap();
        cpu.write(
            CODE,
            &assemble(CODE, |a| {
                a.call(u64::from(DATA)).unwrap();
                a.call(u64::from(DATA + 0x100)).unwrap();
                a.call(u64::from(API)).unwrap();
                a.call(u64::from(DATA)).unwrap();
                a.call(u64::from(DATA + 0x100)).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, 0x100))
                .unwrap();
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(DATA, 0x120))
                .unwrap();
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(API, 3))
                .unwrap();
        }
        start(&mut cpu, CODE);
        assert_eq!(cpu.run(&mut Patch), Exit::Parked);
        assert_eq!(
            cpu.reg(Reg::Eax),
            73 * 47,
            "jit={jit}, host calls={}, mov immediate={}, multiply immediate={}",
            cpu.stats().trap_calls,
            cpu.read_u32(DATA + 1).unwrap(),
            cpu.read_u16(DATA + 0x103).unwrap()
        );
    }
}

#[test]
fn park_handler_keeps_register_changes_and_stops_at_park_address() {
    struct ParkNow;
    impl TrapHandler for ParkNow {
        fn on_trap(&mut self, cpu: &mut dyn Guest, _: u8) -> TrapAction {
            cpu.set_reg(Reg::Eax, 42);
            cpu.set_reg(Reg::Esp, STACK + 0x900);
            TrapAction::Park
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.write(CODE, &[0xcd, 0x2e, 0xb8, 99, 0, 0, 0, 0xc3])
            .unwrap();
        start(&mut cpu, CODE);
        assert_eq!(cpu.run(&mut ParkNow), Exit::Parked);
        assert_eq!(cpu.reg(Reg::Eip), PARK);
        assert_eq!(cpu.reg(Reg::Eax), 42);
        assert_eq!(cpu.reg(Reg::Esp), STACK + 0x900);
    }
}

#[test]
fn full_context_switch_preserves_x87_xmm_mxcsr_control_and_fs() {
    const TEB_A: u32 = 0x7ffd_e000;
    const TEB_B: u32 = 0x7ffd_d000;
    const SAVE: u32 = CODE + 0x100;
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(DATA, 4096, Prot::RW).unwrap();
        for (at, value) in [(TEB_A, 11), (TEB_B, 29)] {
            cpu.map(at, 4096, Prot::RW).unwrap();
            cpu.write_u32(at + 0x18, value).unwrap();
        }
        cpu.write(
            CODE,
            &assemble(CODE, |a| {
                a.fld(dword_ptr(DATA as i32)).unwrap();
                a.fldcw(word_ptr(DATA as i32 + 4)).unwrap();
                a.ldmxcsr(dword_ptr(DATA as i32 + 8)).unwrap();
                a.movups(xmm6, xmmword_ptr(DATA as i32 + 16)).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        cpu.write(
            SAVE,
            &assemble(SAVE, |a| {
                a.fstp(dword_ptr(DATA as i32 + 0x80)).unwrap();
                a.fstcw(word_ptr(DATA as i32 + 0x84)).unwrap();
                a.stmxcsr(dword_ptr(DATA as i32 + 0x88)).unwrap();
                a.movups(xmmword_ptr(DATA as i32 + 0x90), xmm6).unwrap();
                a.mov(eax, dword_ptr(0x18).fs()).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, 0x40))
                .unwrap();
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(SAVE, 0x60))
                .unwrap();
        }
        let mut blank = cpu.new_context();
        cpu.save(&mut blank);
        let mut contexts = Vec::new();
        for (teb, value, control, mxcsr, xmm) in [
            (TEB_A, 1.25f32, 0x27fu16, 0x1f80u32, 17u8),
            (TEB_B, 9.5, 0x67f, 0x3f80, 29),
        ] {
            cpu.restore(&blank);
            cpu.set_fs_base(teb).unwrap();
            cpu.write(DATA, &value.to_le_bytes()).unwrap();
            cpu.write(DATA + 4, &control.to_le_bytes()).unwrap();
            cpu.write_u32(DATA + 8, mxcsr).unwrap();
            cpu.write(DATA + 16, &[xmm; 16]).unwrap();
            start(&mut cpu, CODE);
            assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
            let mut context = cpu.new_context();
            cpu.save(&mut context);
            contexts.push(context);
        }
        for (index, value, control, mxcsr, xmm, teb_value) in [
            (0, 1.25f32, 0x27fu16, 0x1f80u32, 17u8, 11),
            (1, 9.5, 0x67f, 0x3f80, 29, 29),
            (0, 1.25, 0x27f, 0x1f80, 17, 11),
        ] {
            cpu.restore(&contexts[index]);
            start(&mut cpu, SAVE);
            assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
            assert_eq!(cpu.read_u32(DATA + 0x80).unwrap(), value.to_bits());
            assert_eq!(cpu.read_u16(DATA + 0x84).unwrap(), control);
            assert_eq!(cpu.read_u32(DATA + 0x88).unwrap(), mxcsr);
            let mut saved = [0; 16];
            cpu.read(DATA + 0x90, &mut saved).unwrap();
            assert_eq!(saved, [xmm; 16]);
            assert_eq!(cpu.reg(Reg::Eax), teb_value);
        }
    }
}

#[test]
fn full_context_switch_preserves_mmx_registers() {
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(DATA, 4096, Prot::RW).unwrap();
        cpu.write(
            CODE,
            &assemble(CODE, |a| {
                a.movq(mm3, qword_ptr(DATA as i32)).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        cpu.write(
            CODE + 0x100,
            &assemble(CODE + 0x100, |a| {
                a.movq(qword_ptr(DATA as i32 + 16), mm3).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, 0x20))
                .unwrap();
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE + 0x100, 0x20))
                .unwrap();
        }
        let mut contexts = vec![];
        for value in [0x1234_5678_9abc_def0u64, 0xfedc_ba98_7654_3210] {
            cpu.write(DATA, &value.to_le_bytes()).unwrap();
            start(&mut cpu, CODE);
            assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
            let mut context = cpu.new_context();
            cpu.save(&mut context);
            contexts.push(context);
        }
        for (index, value) in [
            (0, 0x1234_5678_9abc_def0u64),
            (1, 0xfedc_ba98_7654_3210),
            (0, 0x1234_5678_9abc_def0),
        ] {
            cpu.restore(&contexts[index]);
            start(&mut cpu, CODE + 0x100);
            assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
            let mut saved = [0; 8];
            cpu.read(DATA + 16, &mut saved).unwrap();
            assert_eq!(u64::from_le_bytes(saved), value);
        }
    }
}

fn check_double_stack(cpu: &mut dyn Guest) {
    assert!(cpu.pop_f64().is_none());
    for bits in [
        0,
        1u64 << 63,
        1,
        (1u64 << 52) - 1,
        0x3ff0_0000_0000_0001,
        0x4009_21fb_5444_2d18,
        0x7fef_ffff_ffff_ffff,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_1234_5678_9abc,
    ] {
        cpu.push_f64(f64::from_bits(bits));
        assert_eq!(cpu.pop_f64().unwrap().to_bits(), bits);
        assert!(cpu.pop_f64().is_none());
    }
    for n in 1..=8 {
        cpu.push_f64(f64::from(n));
    }
    for n in (1..=8).rev() {
        assert_eq!(cpu.pop_f64(), Some(f64::from(n)));
    }
    assert!(cpu.pop_f64().is_none());
}

#[test]
fn double_stack_returns_preserve_precision_subnormals_and_stack_order() {
    check_double_stack(&mut cpu(false));
    check_double_stack(&mut cpu(true));
    check_double_stack(&mut adv_unicorn::UnicornCpu::new(PARK).unwrap());
}

#[test]
fn raw_double_return_and_pop_preserve_existing_guest_x87_stack() {
    const VALUE: f64 = 1.000_000_000_000_000_2;
    struct Floats {
        calls: usize,
    }
    impl TrapHandler for Floats {
        fn on_trap(&mut self, cpu: &mut dyn Guest, _: u8) -> TrapAction {
            match self.calls {
                0 => cpu.push_f64(VALUE),
                1 => {
                    assert_eq!(cpu.pop_f64().unwrap().to_bits(), VALUE.to_bits());
                    cpu.push_f64(VALUE * 4.0);
                }
                _ => panic!("unexpected float handler call"),
            }
            self.calls += 1;
            TrapAction::Continue
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(API, 4096, Prot::RX).unwrap();
        cpu.map(DATA, 4096, Prot::RW).unwrap();
        cpu.write(API, &[0xcd, 0x2e, 0xc3]).unwrap();
        cpu.write(DATA, &VALUE.to_le_bytes()).unwrap();
        cpu.write(
            CODE,
            &assemble(CODE, |a| {
                a.fld1().unwrap();
                a.call(u64::from(API)).unwrap();
                a.fstp(qword_ptr(DATA as i32 + 16)).unwrap();
                a.fld(qword_ptr(DATA as i32)).unwrap();
                a.call(u64::from(API)).unwrap();
                a.fstp(qword_ptr(DATA as i32 + 24)).unwrap();
                a.fstp(qword_ptr(DATA as i32 + 32)).unwrap();
                a.ret().unwrap();
            }),
        )
        .unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, 0x80))
                .unwrap();
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(API, 3))
                .unwrap();
        }
        start(&mut cpu, CODE);
        let mut floats = Floats { calls: 0 };
        assert_eq!(cpu.run(&mut floats), Exit::Parked);
        assert_eq!(floats.calls, 2);
        for (at, expected) in [
            (DATA + 16, VALUE),
            (DATA + 24, VALUE * 4.0),
            (DATA + 32, 1.0),
        ] {
            let mut bytes = [0; 8];
            cpu.read(at, &mut bytes).unwrap();
            assert_eq!(f64::from_le_bytes(bytes).to_bits(), expected.to_bits());
        }
        assert!(cpu.pop_f64().is_none());
    }
}

#[test]
fn invalidation_recompiles_changed_region_and_revalidates_unchanged_same_page_code() {
    let mut cpu = cpu(true);
    cpu.map(CODE, 4096, Prot::RX).unwrap();
    cpu.map(API, 4096, Prot::RX).unwrap();
    cpu.map(DATA, 4096, Prot::RX).unwrap();
    let main = assemble(CODE, |a| {
        a.call(u64::from(DATA)).unwrap();
        a.mov(ebx, eax).unwrap();
        a.call(u64::from(DATA + 0x100)).unwrap();
        a.add(ebx, eax).unwrap();
        a.call(u64::from(API)).unwrap();
        a.add(eax, ebx).unwrap();
        a.ret().unwrap();
    });
    cpu.write(CODE, &main).unwrap();
    cpu.write(DATA, &[0xb8, 2, 0, 0, 0, 0xc3]).unwrap();
    cpu.write(DATA + 0x100, &[0xb8, 3, 0, 0, 0, 0xc3]).unwrap();
    cpu.write(API, &[0xb8, 5, 0, 0, 0, 0xc3]).unwrap();
    for (at, len) in [(CODE, main.len()), (DATA, 6), (DATA + 0x100, 6), (API, 6)] {
        cpu.executor_mut()
            .add_region(adv_cpu::Region::new(at, len))
            .unwrap();
    }
    start(&mut cpu, CODE);
    assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
    assert_eq!(cpu.reg(Reg::Eax), 10);
    cpu.write_u32(DATA + 1, 7).unwrap();
    start(&mut cpu, CODE);
    assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
    assert_eq!(cpu.reg(Reg::Eax), 15);
    // DATA changed, DATA+0x100 shares its page but its captured bytes did not.
}

#[test]
fn setup_segments_uses_owned_descriptor_tables_and_windows_selectors() {
    const GDT_ADDR: u32 = 0x7e11_0000;
    const TEB: u32 = 0x7ffd_e000;
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        assert!(
            cpu.setup_segments(SegmentSetup { gdt_addr: GDT_ADDR })
                .is_err()
        );
        cpu.map(GDT_ADDR, 4096, Prot::RW).unwrap();
        assert!(
            cpu.setup_segments(SegmentSetup {
                gdt_addr: GDT_ADDR + 1
            })
            .is_err()
        );
        cpu.setup_segments(SegmentSetup { gdt_addr: GDT_ADDR })
            .unwrap();
        let idt = GDT_ADDR + SegmentSetup::IDT_OFFSET;
        let mut gates = [0xff; SegmentSetup::IDT_LIMIT as usize + 1];
        cpu.read(idt, &mut gates).unwrap();
        assert!(gates.iter().all(|&byte| byte == 0));
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(DATA, 4096, Prot::RW).unwrap();
        cpu.map(TEB, 4096, Prot::RW).unwrap();
        cpu.write_u32(TEB + 0x18, 0x1234_5678).unwrap();
        cpu.set_fs_base(TEB).unwrap();
        let bytes = assemble(CODE, |a| {
            a.xor(eax, eax).unwrap();
            for (segment, offset) in [(cs, 0), (ds, 4), (es, 8), (ss, 12), (fs, 16)] {
                a.mov(ax, segment).unwrap();
                a.mov(dword_ptr(DATA as i32 + offset), eax).unwrap();
            }
            a.sgdt(fword_ptr(DATA as i32 + 32)).unwrap();
            a.sidt(fword_ptr(DATA as i32 + 40)).unwrap();
            a.mov(eax, dword_ptr(0x18).fs()).unwrap();
            a.ret().unwrap();
        });
        cpu.write(CODE, &bytes).unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, bytes.len()))
                .unwrap();
        }
        start(&mut cpu, CODE);
        assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
        for (offset, selector) in [(0, 0x23), (4, 0x2b), (8, 0x2b), (12, 0x30), (16, 0x53)] {
            assert_eq!(cpu.read_u32(DATA + offset).unwrap(), selector);
        }
        assert_eq!(cpu.read_u16(DATA + 32).unwrap(), 127);
        assert_eq!(cpu.read_u32(DATA + 34).unwrap(), GDT_ADDR);
        assert_eq!(cpu.read_u16(DATA + 40).unwrap(), SegmentSetup::IDT_LIMIT);
        assert_eq!(cpu.read_u32(DATA + 42).unwrap(), idt);
        assert_eq!(cpu.reg(Reg::Eax), 0x1234_5678);
    }
}

#[test]
fn sidt_descriptor_identity_survives_context_switches_in_both_execution_modes() {
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(DATA, 4096, Prot::RW).unwrap();
        let bytes = assemble(CODE, |a| {
            a.sidt(fword_ptr(DATA as i32 + 1)).unwrap();
            a.ret().unwrap();
        });
        cpu.write(CODE, &bytes).unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, bytes.len()))
                .unwrap();
        }
        let mut contexts = Vec::new();
        for (base, limit) in [(0xf123_4567, 0x7ff), (0xe765_4321, 0x3ff)] {
            let state = &mut cpu.executor_mut().state;
            state.segments.idt_base = base;
            state.segments.idt_limit = limit;
            let mut context = cpu.new_context();
            cpu.save(&mut context);
            contexts.push(context);
        }
        for (index, base, limit) in [
            (0, 0xf123_4567, 0x7ff),
            (1, 0xe765_4321, 0x3ff),
            (0, 0xf123_4567, 0x7ff),
        ] {
            cpu.restore(&contexts[index]);
            cpu.write(DATA, &[0xa5; 8]).unwrap();
            cpu.set_reg(Reg::Eax, 0x1357_2468);
            cpu.set_reg(Reg::Eflags, 0x8d7);
            start(&mut cpu, CODE);
            assert_eq!(cpu.run(&mut UnexpectedTrap), Exit::Parked);
            assert_eq!(cpu.read_u16(DATA + 1).unwrap(), limit);
            assert_eq!(cpu.read_u32(DATA + 3).unwrap(), base);
            assert_eq!(cpu.reg(Reg::Eax), 0x1357_2468);
            assert_eq!(cpu.reg(Reg::Eflags), 0x8d7);
            let mut canaries = [0; 8];
            cpu.read(DATA, &mut canaries).unwrap();
            assert_eq!((canaries[0], canaries[7]), (0xa5, 0xa5));
        }
    }
}

#[test]
fn native_and_fallback_faults_keep_kind_address_and_faulting_pc() {
    const BAD: u32 = 0x0bad_0000;
    for jit in [false, true] {
        for case in 0..5 {
            let mut cpu = cpu(jit);
            cpu.map(CODE, 4096, Prot::RX).unwrap();
            let (bytes, kind) = match case {
                0 => (
                    assemble(CODE, |a| {
                        a.mov(eax, dword_ptr(BAD as i32)).unwrap();
                    }),
                    FaultKind::Read(BAD),
                ),
                1 => (
                    assemble(CODE, |a| {
                        a.mov(dword_ptr(BAD as i32), eax).unwrap();
                    }),
                    FaultKind::Write(BAD),
                ),
                2 => (
                    assemble(CODE, |a| {
                        a.fld(dword_ptr(BAD as i32)).unwrap();
                    }),
                    FaultKind::Read(BAD),
                ),
                3 => (
                    assemble(CODE, |a| {
                        a.fstp(dword_ptr(BAD as i32)).unwrap();
                    }),
                    FaultKind::Write(BAD),
                ),
                _ => (vec![0xc3], FaultKind::Fetch(BAD)),
            };
            cpu.write(CODE, &bytes).unwrap();
            if jit {
                cpu.executor_mut()
                    .add_region(adv_cpu::Region::new(CODE, bytes.len()))
                    .unwrap();
            }
            start(&mut cpu, if case == 4 { BAD } else { CODE });
            cpu.set_reg(Reg::Eax, 0x1234);
            if case == 3 {
                cpu.push_f64(1.25);
            }
            let expected_pc = if case == 4 { BAD } else { CODE };
            assert_eq!(
                cpu.run(&mut UnexpectedTrap),
                Exit::Fault(Fault {
                    kind,
                    pc: expected_pc
                }),
                "case {case}, jit {jit}"
            );
            assert_eq!(cpu.reg(Reg::Eip), expected_pc);
        }
        for (prot, write) in [(Prot(0), false), (Prot::READ, true), (Prot::RW, false)] {
            let mut cpu = cpu(jit);
            cpu.map(CODE, 4096, Prot::RX).unwrap();
            cpu.map(BAD, 4096, prot).unwrap();
            let bytes = if prot == Prot::RW {
                vec![0xc3]
            } else {
                assemble(CODE, |a| {
                    if write {
                        a.mov(dword_ptr(BAD as i32), eax).unwrap();
                    } else {
                        a.mov(eax, dword_ptr(BAD as i32)).unwrap();
                    }
                })
            };
            cpu.write(CODE, &bytes).unwrap();
            start(&mut cpu, if prot == Prot::RW { BAD } else { CODE });
            if jit {
                cpu.executor_mut()
                    .add_region(adv_cpu::Region::new(CODE, bytes.len()))
                    .unwrap();
            }
            let kind = if prot == Prot::RW {
                FaultKind::Fetch(BAD)
            } else if write {
                FaultKind::Write(BAD)
            } else {
                FaultKind::Read(BAD)
            };
            assert_eq!(
                cpu.run(&mut UnexpectedTrap),
                Exit::Fault(Fault {
                    kind,
                    pc: if prot == Prot::RW { BAD } else { CODE }
                })
            );
        }
    }
}

#[test]
fn invalid_opcode_exception_preserves_faulting_pc_and_can_resume_once() {
    struct Recover {
        next_pc: u32,
        calls: usize,
    }
    impl TrapHandler for Recover {
        fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
            assert_eq!(vector, 6);
            assert_eq!(cpu.reg(Reg::Eip), CODE);
            assert_eq!(cpu.reg(Reg::Eax), 0x1234);
            assert_eq!(cpu.reg(Reg::Esp), STACK + 0x800);
            assert_eq!(cpu.read_u32(DATA).unwrap(), 0);
            self.calls += 1;
            cpu.set_reg(Reg::Eip, self.next_pc);
            TrapAction::Continue
        }
    }
    // UD2 and an invalid LOCK NOP decode both use the architectural #UD path.
    for invalid in [[0x0f, 0x0b], [0xf0, 0x90]] {
        for jit in [false, true] {
            let mut cpu = cpu(jit);
            cpu.map(CODE, 4096, Prot::RX).unwrap();
            cpu.map(DATA, 4096, Prot::RW).unwrap();
            let mut bytes = invalid.to_vec();
            bytes.extend(assemble(CODE + 2, |a| {
                a.inc(dword_ptr(DATA as i32)).unwrap();
                a.ret().unwrap();
            }));
            cpu.write(CODE, &bytes).unwrap();
            if jit {
                cpu.executor_mut()
                    .add_region(adv_cpu::Region::new(CODE, bytes.len()))
                    .unwrap();
            }
            start(&mut cpu, CODE);
            cpu.set_reg(Reg::Eax, 0x1234);
            let mut recover = Recover {
                next_pc: CODE + 2,
                calls: 0,
            };
            assert_eq!(cpu.run(&mut recover), Exit::Parked);
            assert_eq!(recover.calls, 1);
            assert_eq!(cpu.read_u32(DATA).unwrap(), 1);
            assert_eq!(cpu.reg(Reg::Eax), 0x1234);
            assert_eq!(cpu.reg(Reg::Esp), STACK + 0x804);
        }
    }
}

#[test]
fn every_interrupt_vector_reaches_raw_handler_and_can_resume() {
    struct Interrupts {
        seen: Vec<(u8, u32)>,
    }
    impl TrapHandler for Interrupts {
        fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
            self.seen.push((vector, cpu.reg(Reg::Eip)));
            cpu.set_reg(Reg::Eax, u32::from(vector));
            TrapAction::Continue
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        let bytes = [0xcc, 0xcd, 0x80, 0xcd, 0x2e, 0xc3];
        cpu.write(CODE, &bytes).unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, bytes.len()))
                .unwrap();
        }
        start(&mut cpu, CODE);
        let mut interrupts = Interrupts { seen: vec![] };
        assert_eq!(cpu.run(&mut interrupts), Exit::Parked);
        assert_eq!(
            interrupts.seen,
            [(3, CODE + 1), (0x80, CODE + 3), (0x2e, CODE + 5)]
        );
        assert_eq!(cpu.reg(Reg::Eax), 0x2e);
    }
}

#[test]
fn division_exception_can_be_recovered_by_handler_pc_change() {
    struct Recover;
    impl TrapHandler for Recover {
        fn on_trap(&mut self, cpu: &mut dyn Guest, vector: u8) -> TrapAction {
            assert_eq!(vector, 0);
            assert_eq!(cpu.reg(Reg::Eip), CODE);
            cpu.set_reg(Reg::Eip, CODE + 2);
            cpu.set_reg(Reg::Eax, 91);
            TrapAction::Continue
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.write(CODE, &[0xf7, 0xf3, 0xc3]).unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, 3))
                .unwrap();
        }
        start(&mut cpu, CODE);
        cpu.set_reg(Reg::Ebx, 0);
        cpu.set_reg(Reg::Edx, 0);
        assert_eq!(cpu.run(&mut Recover), Exit::Parked);
        assert_eq!(cpu.reg(Reg::Eax), 91);
    }
}

#[test]
fn host_double_return_refreshes_mmx_alias_cache_and_emms_empties_x87_stack() {
    const VALUE: f64 = 1.000_000_000_000_000_2;
    struct Float;
    impl TrapHandler for Float {
        fn on_trap(&mut self, cpu: &mut dyn Guest, _: u8) -> TrapAction {
            assert!(
                cpu.pop_f64().is_none(),
                "native EMMS must empty x87 before host call"
            );
            cpu.push_f64(VALUE);
            TrapAction::Continue
        }
    }
    for jit in [false, true] {
        let mut cpu = cpu(jit);
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(API, 4096, Prot::RX).unwrap();
        cpu.map(DATA, 4096, Prot::RW).unwrap();
        cpu.write(API, &[0xcd, 0x2e, 0xc3]).unwrap();
        let bytes = assemble(CODE, |a| {
            a.pxor(mm7, mm7).unwrap();
            a.emms().unwrap();
            a.call(u64::from(API)).unwrap();
            a.movq(mm0, mm7).unwrap();
            a.movq(qword_ptr(DATA as i32), mm0).unwrap();
            a.emms().unwrap();
            a.ret().unwrap();
        });
        cpu.write(CODE, &bytes).unwrap();
        if jit {
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(CODE, bytes.len()))
                .unwrap();
            cpu.executor_mut()
                .add_region(adv_cpu::Region::new(API, 3))
                .unwrap();
        }
        start(&mut cpu, CODE);
        assert_eq!(cpu.run(&mut Float), Exit::Parked);
        let mut bits = [0; 8];
        cpu.read(DATA, &mut bits).unwrap();
        assert_eq!(bits, adv_cpu::backend::x87::encode_f64(VALUE)[..8]);
        assert!(
            cpu.pop_f64().is_none(),
            "EMMS after host return must synchronize outside run"
        );
        cpu.push_f64(VALUE);
        assert_eq!(cpu.pop_f64().unwrap().to_bits(), VALUE.to_bits());
    }
}
