mod support;
use adv_cpu::{CpuState, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const SETUP: u32 = 0x11000;
const STOP: u32 = 0x18000;
const SRC: u32 = 0x20000;
const OUT: u32 = 0x28000;
const SCALE: u32 = 0x38000;
const CW: u32 = SCALE + 4;
const STACK: u32 = 0x4f800;
const SAMPLES: usize = 2048;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    code_at(CODE, build)
}
fn code_at(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}
fn pcm_code() -> Vec<u8> {
    code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(ebx, eax).unwrap();
        a.fld(dword_ptr(esi)).unwrap();
        a.fmul(dword_ptr(SCALE as i32)).unwrap();
        a.fistp(dword_ptr(edi)).unwrap();
        a.add(esi, 4).unwrap();
        a.add(edi, 4).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.fnstsw(ax).unwrap();
        a.ret().unwrap();
    })
}
fn machine(bytes: &[u8], native: bool, linked: bool) -> Executor {
    let mut m = Executor::new(CODE, 0x40000).unwrap();
    m.protect(CODE, 0x40000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 8192, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(SCALE, &32768.0f32.to_le_bytes()).unwrap();
    m.set_region_linking(linked);
    if native {
        m.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    m
}
fn samples() -> Vec<u8> {
    (0..SAMPLES)
        .map(|i| ((i % 129) as f32 - 64.0) / 65536.0)
        .flat_map(f32::to_le_bytes)
        .collect()
}
fn expected(truncate: bool) -> Vec<u8> {
    (0..SAMPLES)
        .flat_map(|i| {
            let value = ((i % 129) as f64 - 64.0) / 2.0;
            (if truncate {
                value.trunc()
            } else {
                value.round_ties_even()
            } as i32)
                .to_le_bytes()
        })
        .collect()
}
fn start(m: &mut Executor, source: u32, destination: u32, stack: u32) {
    m.write(stack, &STOP.to_le_bytes()).unwrap();
    m.state = CpuState {
        regs: [0, SAMPLES as u32, 0, 0, stack, 0, source, destination],
        eip: CODE,
        eflags: 0x202,
        ..CpuState::default()
    };
}

fn assert_pcm_registers(actual: &Executor, oracle: &Executor, exceptions: u32) {
    // FNSTSW AX exposes masked precision/denormal/invalid status. Unicorn
    // omits these for FISTP, so assert the architectural result independently.
    assert_eq!(actual.state.regs[0] & 0x3f, exceptions);
    let mut actual_regs = actual.state.regs;
    let mut oracle_regs = oracle.state.regs;
    actual_regs[0] &= !0x3f;
    oracle_regs[0] &= !0x3f;
    assert_eq!(actual_regs, oracle_regs);
}

#[test]
fn shared_float_execution_matches_pcm_with_linking_and_oracle() {
    let bytes = pcm_code();
    let mut off = machine(&bytes, true, false);
    let mut on = machine(&bytes, true, true);
    let mut uc = machine(&bytes, false, false);
    for m in [&mut off, &mut on, &mut uc] {
        m.write(SRC, &samples()).unwrap();
        start(m, SRC, OUT, STACK);
    }
    assert_eq!(off.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(on.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    for m in [&off, &on, &uc] {
        assert_eq!(m.memory().read(OUT, SAMPLES * 4).unwrap(), expected(false));
    }
    assert_eq!(on.state.regs, off.state.regs);
    assert_pcm_registers(&on, &uc, 0x20);
    assert_eq!(on.state.eflags, off.state.eflags);
    assert_eq!(on.state.eflags, uc.state.eflags);
    assert!(on.stats.interpreter_instructions > 0);
    assert_eq!(
        on.stats.interpreter_instructions,
        off.stats.interpreter_instructions
    );
}

#[test]
fn float_boundaries_keep_nan_infinity_denormal_results_and_reference_status() {
    let bytes = pcm_code();
    let bits = [
        0x7fc0_1234u32,
        0x7f80_0000,
        0xff80_0000,
        1,
        0x8000_0001,
        0x0080_0000,
        0x7f7f_ffff,
        0xff7f_ffff,
    ];
    let source: Vec<_> = (0..SAMPLES)
        .flat_map(|i| bits[i % bits.len()].to_le_bytes())
        .collect();
    let mut on = machine(&bytes, true, true);
    let mut off = machine(&bytes, true, false);
    let mut uc = machine(&bytes, false, false);
    for m in [&mut on, &mut off, &mut uc] {
        m.write(SRC, &source).unwrap();
        start(m, SRC, OUT, STACK);
    }
    assert_eq!(on.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(off.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        uc.run_unicorn(STOP, 100_000).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(
        on.memory().read(OUT, SAMPLES * 4).unwrap(),
        uc.memory().read(OUT, SAMPLES * 4).unwrap()
    );
    assert_eq!(
        off.memory().read(OUT, SAMPLES * 4).unwrap(),
        uc.memory().read(OUT, SAMPLES * 4).unwrap()
    );
    assert_pcm_registers(&on, &uc, 0x23);
    assert_pcm_registers(&off, &uc, 0x23);
    assert_eq!(on.state.mmx, uc.state.mmx);
    // Invalid, denormal and precision flags were checked independently above.
    assert!(on.stats.interpreter_instructions > 0);
}

#[test]
fn execution_slices_preserve_x87_rounding_modes_and_stack_for_two_contexts() {
    let bytes = pcm_code();
    let setup = code(|a| {
        a.fldcw(word_ptr(CW as i32)).unwrap();
        a.ret().unwrap();
    });
    let mut m = machine(&bytes, true, true);
    m.write(SETUP, &setup).unwrap();
    m.write(SRC, &samples()).unwrap();
    m.write(SRC + 0x4000, &samples()).unwrap();
    let mut contexts = Vec::new();
    for (index, control) in [0x27fu16, 0xe7f].into_iter().enumerate() {
        m.write(CW, &control.to_le_bytes()).unwrap();
        m.write(STACK - index as u32 * 4096, &STOP.to_le_bytes())
            .unwrap();
        m.state.eip = SETUP;
        m.state.regs[4] = STACK - index as u32 * 4096;
        assert_eq!(m.run_unicorn(STOP, 100).unwrap(), RunOutcome::ReachedStop);
        // GPR restart must preserve this thread's explicitly configured FPU.
        // A fresh CpuState now resets the entire canonical CPU, including x87.
        let floating = m.state.x87.clone();
        let mxcsr = m.state.mxcsr;
        start(
            &mut m,
            SRC + index as u32 * 0x4000,
            OUT + index as u32 * 0x4000,
            STACK - index as u32 * 4096,
        );
        m.state.x87 = floating;
        m.state.mxcsr = mxcsr;
        contexts.push(m.save_context().unwrap());
    }
    let mut done = [false; 2];
    for _ in 0..1000 {
        for index in 0..2 {
            if done[index] {
                continue;
            }
            m.restore_context(&contexts[index]).unwrap();
            let outcome = m.run(STOP, 200).unwrap();
            done[index] = outcome == RunOutcome::ReachedStop;
            assert!(done[index] || outcome == RunOutcome::Budget);
            contexts[index] = m.save_context().unwrap();
        }
        if done.iter().all(|done| *done) {
            break;
        }
    }
    assert_eq!(done, [true, true]);
    assert_eq!(m.memory().read(OUT, SAMPLES * 4).unwrap(), expected(false));
    assert_eq!(
        m.memory().read(OUT + 0x4000, SAMPLES * 4).unwrap(),
        expected(true)
    );
    assert!(m.stats.interpreter_instructions > 0);
}

#[test]
fn interpreted_x87_fault_keeps_restartable_stack_and_finishes_after_permission_repair() {
    let bytes = pcm_code();
    let mut machines = [
        machine(&bytes, true, false),
        machine(&bytes, true, true),
        machine(&bytes, false, false),
    ];
    for (index, m) in machines.iter_mut().enumerate() {
        m.write(SRC, &samples()).unwrap();
        start(m, SRC, OUT, STACK);
        let run = if index == 2 {
            m.run_unicorn(STOP, 100_000)
        } else {
            m.run(STOP, 100_000)
        };
        assert_eq!(run.unwrap(), RunOutcome::ReachedStop);
        m.protect(OUT + 4096, 4096, READ).unwrap();
        start(m, SRC, OUT, STACK);
    }
    let mut faults = Vec::new();
    for (index, m) in machines.iter_mut().enumerate() {
        let outcome = if index == 2 {
            m.run_unicorn(STOP, 100_000)
        } else {
            m.run(STOP, 100_000)
        }
        .unwrap();
        assert!(matches!(outcome, RunOutcome::WriteFault {address,..} if address==OUT+4096));
        faults.push(outcome);
        let saved = m.save_context().unwrap();
        m.restore_context(&saved).unwrap();
        m.protect(OUT + 4096, 4096, READ | WRITE).unwrap();
        let outcome = if index == 2 {
            m.run_unicorn(STOP, 100_000)
        } else {
            m.run(STOP, 100_000)
        };
        assert_eq!(outcome.unwrap(), RunOutcome::ReachedStop);
        assert_eq!(m.memory().read(OUT, SAMPLES * 4).unwrap(), expected(false));
    }
    assert_eq!(faults[0], faults[1]);
    assert_eq!(faults[1], faults[2]);
    assert_eq!(machines[0].state.regs, machines[1].state.regs);
    assert_pcm_registers(&machines[1], &machines[2], 0x20);
    assert!(machines[1].stats.interpreter_instructions > 0);
}

#[test]
fn code_writes_invalidate_before_native_reentry() {
    let bytes = pcm_code();
    let mut m = machine(&bytes, true, true);
    m.write(SRC, &samples()).unwrap();
    start(&mut m, SRC, OUT, STACK);
    assert_eq!(m.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    let interpreted_before = m.stats.interpreter_instructions;
    assert!(interpreted_before > 0);
    m.write(CODE, &bytes).unwrap();
    start(&mut m, SRC, OUT, STACK);
    m.state.regs[1] = 16;
    assert_eq!(m.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert!(m.stats.interpreter_instructions > interpreted_before);
    start(&mut m, SRC, OUT, STACK);
    assert_eq!(m.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    let interpreted_before = m.stats.interpreter_instructions;
    m.protect(CODE, 4096, READ | WRITE | EXEC).unwrap();
    // Protection changed the code version: readmit, then write the active code page.
    start(&mut m, SRC, OUT, STACK);
    assert_eq!(m.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    start(&mut m, SRC, CODE + 0x300, STACK);
    m.state.regs[1] = 8;
    assert_eq!(m.run(STOP, 100_000).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        m.memory().read(CODE + 0x300, 32).unwrap(),
        &expected(false)[..32]
    );
    assert!(m.stats.invalidations > 0);
    assert!(m.stats.interpreter_instructions > interpreted_before);
}

#[test]
fn interpreted_busy_x87_loop_obeys_wall_deadline_and_restarts_without_state_loss() {
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Backend, Exit, Guest, GuestExt, Prot, Reg, TrapAction, TrapHandler};
    use std::time::{Duration, Instant};
    struct NoTrap;
    impl TrapHandler for NoTrap {
        fn on_trap(&mut self, _: &mut dyn Guest, vector: u8) -> TrapAction {
            panic!("unexpected {vector}")
        }
    }
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(ebx, eax).unwrap();
        a.fld(dword_ptr(esi)).unwrap();
        a.fmul(dword_ptr(SCALE as i32)).unwrap();
        a.fistp(dword_ptr(edi)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    let mut cpu = JitCpu::new(STOP, true).unwrap();
    for at in [CODE, STOP] {
        cpu.map(at, 4096, Prot::RX).unwrap();
    }
    for at in [SRC, OUT, SCALE, STACK & !4095] {
        cpu.map(at, 4096, Prot::RW).unwrap();
    }
    cpu.write(CODE, &bytes).unwrap();
    cpu.write(SRC, &0.25f32.to_le_bytes()).unwrap();
    cpu.write(SCALE, &32768.0f32.to_le_bytes()).unwrap();
    cpu.write_u32(STACK, STOP).unwrap();
    cpu.executor_mut()
        .add_region(Region::new(CODE, bytes.len()))
        .unwrap();
    for (reg, value) in [
        (Reg::Eip, CODE),
        (Reg::Esp, STACK),
        (Reg::Esi, SRC),
        (Reg::Edi, OUT),
        (Reg::Ecx, 5000),
    ] {
        cpu.set_reg(reg, value);
    }
    cpu.set_execution_budget(Some(Duration::from_secs(2)));
    assert_eq!(cpu.run(&mut NoTrap), Exit::Parked);
    assert!(cpu.stats().interpreter_instructions > 0);
    for (reg, value) in [(Reg::Eip, CODE), (Reg::Esp, STACK), (Reg::Ecx, 5_000_000)] {
        cpu.set_reg(reg, value);
    }
    cpu.set_execution_budget(Some(Duration::from_millis(1)));
    let start = Instant::now();
    assert!(matches!(cpu.run(&mut NoTrap), Exit::BudgetExceeded { .. }));
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(cpu.reg(Reg::Ecx) > 0 && cpu.reg(Reg::Ecx) < 5_000_000);
    cpu.set_execution_budget(Some(Duration::from_secs(2)));
    cpu.set_reg(Reg::Ecx, 1);
    assert_eq!(cpu.run(&mut NoTrap), Exit::Parked);
    assert_eq!(cpu.read_u32(OUT).unwrap(), 8192);
}
