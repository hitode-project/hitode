//! Synthetic end-to-end selector checks; no external assets.
#![cfg(feature = "gir-execute")]
use adv_cpu::{EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x12000;
const STOP: u32 = 0x16000;

fn machine(f: impl FnOnce(&mut CodeAssembler)) -> Executor {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    let bytes = a.assemble(u64::from(CODE)).unwrap();
    let mut m = Executor::new(CODE, 0x8000).unwrap();
    m.protect(CODE, 0x8000, READ | WRITE).unwrap();
    m.write(CODE, &bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.state.eip = CODE;
    m.state.regs[4] = DATA + 0x1000;
    m.write(DATA + 0x1000, &STOP.to_le_bytes()).unwrap();
    m.add_region(Region::new(CODE, bytes.len())).unwrap();
    m
}

#[test]
fn gir_environment_selector_executes_native_ssa_and_strict_reference() {
    let mut m = machine(|a| {
        let mut top = a.create_label();
        a.mov(ecx, 5).unwrap();
        a.set_label(&mut top).unwrap();
        a.add(eax, 7).unwrap();
        a.adc(ebx, eax).unwrap();
        a.mov(dword_ptr(DATA as i32), ebx).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
        a.ret().unwrap();
    });
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[0], 35);
    let selected = std::env::var("ADV_GIR").as_deref() == Ok("state-ssa");
    assert_eq!(m.gir_stats().admitted > 0, selected);
    if selected {
        assert_eq!(m.gir_stats().units, 27);
    }
    if selected && std::env::var("ADV_JIT_VERIFY").as_deref() == Ok("strict") {
        assert!(m.stats.verification.verified > 0);
        assert_eq!(m.stats.verification.inconclusive, 0);
        assert_eq!(m.stats.verification.mismatches, 0);
    }
    assert_eq!(
        m.state.last_indirect_branch.kind,
        adv_cpu::IndirectBranchKind::Return
    );
    assert_eq!(m.state.last_indirect_branch.target, STOP);
}

#[test]
fn gir_fault_after_prefix_resumes_once_after_permission_repair() {
    let mut m = machine(|a| {
        a.inc(ebx).unwrap();
        a.add(dword_ptr(DATA as i32), eax).unwrap();
        a.ret().unwrap();
    });
    m.state.regs[0] = 7;
    m.protect(DATA, 4096, READ).unwrap();
    assert!(matches!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::WriteFault { address: DATA, .. }
    ));
    assert_eq!(m.state.regs[3], 1);
    assert_eq!(m.memory().read(DATA, 4).unwrap(), [0; 4]);
    m.protect(DATA, 4096, READ | WRITE).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[3], 1);
    assert_eq!(m.memory().read(DATA, 4).unwrap(), 7u32.to_le_bytes());
    assert_eq!(m.stats.verification.mismatches, 0);
}

#[test]
fn gir_unrepresented_fp_exits_before_instruction_and_continues() {
    let mut m = machine(|a| {
        a.inc(ebx).unwrap();
        a.fld1().unwrap();
        a.inc(eax).unwrap();
        a.ret().unwrap();
    });
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[3], 1);
    assert_eq!(m.state.regs[0], 1);
    assert_eq!(m.state.x87.top(), 7);
    if std::env::var("ADV_GIR").as_deref() == Ok("state-ssa") {
        assert!(m.gir_stats().cold > 0);
    }
    assert_eq!(m.stats.verification.mismatches, 0);
}
