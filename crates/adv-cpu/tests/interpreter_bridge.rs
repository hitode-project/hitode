mod support;
use adv_cpu::{EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const STACK: u32 = 0x1f800;

fn setup(fault: bool) -> Executor {
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(eax, -101).unwrap();
    a.cdq().unwrap();
    // The 32-bit form is native; keep this bridge check on a cold 16-bit form.
    a.idiv(word_ptr(edi)).unwrap();
    a.add(ebx, eax).unwrap();
    a.ret().unwrap();
    let bytes = a.assemble(CODE as u64).unwrap();
    let mut m = Executor::new(CODE, 0x10000).unwrap();
    m.protect(CODE, 0x10000, READ | WRITE).unwrap();
    m.write(CODE, &bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    let mut region = Region::new(CODE, bytes.len());
    region.entries.extend(
        Decoder::with_ip(32, &bytes, CODE as u64, DecoderOptions::NONE)
            .into_iter()
            .filter(|i| i.mnemonic() == Mnemonic::Idiv)
            .map(|i| i.next_ip32()),
    );
    m.add_region(region).unwrap();
    m.state.eip = CODE;
    m.state.regs[3] = 100;
    m.state.regs[4] = STACK;
    m.state.regs[7] = 0x16000;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.write(0x16000, &7u32.to_le_bytes()).unwrap();
    if fault {
        m.protect(0x16000, 4096, 0).unwrap();
    }
    m
}

#[test]
fn native_word_idiv_exit_uses_shared_state_then_resumes_native_without_unicorn() {
    let mut hybrid = setup(false);
    let mut reference = setup(false);
    assert_eq!(hybrid.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(
        reference.run_unicorn(STOP, 100).unwrap(),
        RunOutcome::ReachedStop
    );
    assert_eq!(hybrid.state.regs, reference.state.regs);
    assert_eq!(hybrid.state.eflags, reference.state.eflags);
    assert_eq!(hybrid.state.regs[0] as i32, -14);
    assert_eq!(hybrid.state.regs[2] as i32, -3);
    support::x64_shape!(assert_eq!(hybrid.stats.interpreter_entries, 1));
    support::x64_shape!(assert_eq!(hybrid.stats.interpreter_instructions, 1));
}

#[test]
fn interpreter_fault_preserves_native_prefix_and_resumes_only_faulting_instruction() {
    let mut m = setup(true);
    let result = m.run(STOP, 100).unwrap();
    let RunOutcome::ReadFault { pc, address } = result else {
        panic!("{result:?}");
    };
    assert_eq!(address, 0x16000);
    assert_eq!(m.state.eip, pc);
    assert_eq!(m.state.regs[0] as i32, -101);
    assert_eq!(m.state.regs[2], u32::MAX);
    assert_eq!(m.state.regs[3], 100);
    assert_eq!(m.state.fault_addr, address);
    support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 1));
    support::x64_shape!(assert_eq!(m.stats.interpreter_instructions, 0));
    m.protect(address, 4096, READ | WRITE).unwrap();
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    assert_eq!(m.state.regs[3], 86);
    support::x64_shape!(assert_eq!(m.stats.interpreter_instructions, 1));
    support::x64_shape!(assert_eq!(m.stats.interpreter_entries, 2));
}
