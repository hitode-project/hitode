mod support;

use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use support::Executor;

const CODE: u32 = 0x10000;
const BODY: u32 = 0x11000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const BAD: u32 = DATA + 4096;
const STACK: u32 = 0x2f800;
const INITIAL: [u32; 8] = [
    0x1133_5577,
    0x7fff_fffe,
    0x2244_6688,
    0x1357_9bdf,
    STACK,
    0x3456_789a,
    DATA + 12,
    0x5566_7788,
];

fn code(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn prefix() -> Vec<u8> {
    code(CODE, |a| {
        a.add(ebp, 7).unwrap();
        a.mov(edi, 0x7654_3210).unwrap();
        a.jmp(u64::from(BODY)).unwrap();
    })
}

fn machine(
    regions: &[(u32, Vec<u8>, Vec<u32>)],
    strategy: Option<CompileStrategy>,
    linked: bool,
) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    for (at, bytes, entries) in regions {
        m.write(*at, bytes).unwrap();
        m.protect(*at, 4096, READ | EXEC).unwrap();
        if let Some(strategy) = strategy {
            let mut region = Region::new(*at, bytes.len());
            region.entries.extend(entries.iter().copied());
            region.compile_strategy = strategy;
            m.add_region(region).unwrap();
        }
    }
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.write(DATA + 12, &0xffff_fffeu32.to_le_bytes()).unwrap();
    m.set_region_linking(linked);
    m.state.eip = regions[0].0;
    m.state.regs = INITIAL;
    m.state.eflags = 0xed7;
    m
}

// Single instructions canonicalize Unicorn's lazy flags before a memory fault.
fn oracle(m: &mut Executor, stop: u32) -> RunOutcome {
    for _ in 0..128 {
        let outcome = m.run_unicorn(stop, 1).unwrap();
        if outcome != RunOutcome::Budget {
            return outcome;
        }
    }
    panic!("authored footprint fixture did not finish");
}

fn equal_state(jit: &Executor, reference: &Executor) {
    assert_eq!(jit.state.regs, reference.state.regs);
    assert_eq!(jit.state.eflags, reference.state.eflags);
    assert_eq!(jit.state.eip, reference.state.eip);
    support::x64_shape!(assert_eq!(jit.stats.interpreter_entries, 0));
}

fn equal_fault_state(jit: &Executor, reference: &Executor, address: u32) {
    equal_state(jit, reference);
    // Unicorn reports the fault address in RunOutcome, without writing this
    // diagnostic CpuState field. Its value is meaningful only on a JIT fault.
    assert_eq!(jit.state.fault_addr, address);
}

#[cfg_attr(target_arch = "aarch64", ignore = "Cranelift execution shape")]
#[test]
fn a_later_region_budget_exit_keeps_prior_writes_and_untouched_sentinels() {
    let body = code(BODY, |a| {
        let mut write = a.create_label();
        a.jmp(write).unwrap();
        a.set_label(&mut write).unwrap();
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.add(eax, ecx).unwrap();
        a.ret().unwrap();
    });
    let write_pc = Decoder::with_ip(32, &body, u64::from(BODY), DecoderOptions::NONE)
        .into_iter()
        .find(|i| i.mnemonic() == Mnemonic::Mov)
        .unwrap()
        .ip32();
    let regions = [(CODE, prefix(), vec![]), (BODY, body, vec![])];
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for linked in [false, true] {
            let mut jit = machine(&regions, Some(strategy), linked);
            let mut reference = machine(&regions, None, false);
            // A and B's jump each charge a block. B stops before its first MOV.
            assert_eq!(jit.run(STOP, 2).unwrap(), RunOutcome::Budget);
            assert_eq!(jit.state.eip, write_pc);
            assert_eq!(oracle(&mut reference, write_pc), RunOutcome::ReachedStop);
            equal_state(&jit, &reference);
            assert_eq!(jit.state.regs[0], INITIAL[0]);
            assert_eq!(jit.state.regs[5], INITIAL[5] + 7);
            assert_eq!(jit.state.regs[7], 0x7654_3210);
            assert_eq!(jit.state.regs[2], INITIAL[2]);
            assert_eq!(jit.state.regs[3], INITIAL[3]);

            assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(oracle(&mut reference, STOP), RunOutcome::ReachedStop);
            equal_state(&jit, &reference);
        }
    }
}

#[test]
fn a_first_load_fault_keeps_full_and_implicit_destinations_before_their_first_write() {
    let body = code(BODY, |a| {
        a.mov(eax, dword_ptr(esi)).unwrap();
        a.mul(ebx).unwrap();
        a.add(ecx, 3).unwrap(); // Define all arithmetic flags after MUL.
        a.ret().unwrap();
    });
    let regions = [(CODE, prefix(), vec![]), (BODY, body, vec![])];
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for linked in [false, true] {
            let mut jit = machine(&regions, Some(strategy), linked);
            let mut reference = machine(&regions, None, false);
            for m in [&mut jit, &mut reference] {
                m.state.regs[6] = BAD;
                m.protect(BAD, 4096, 0).unwrap();
            }
            let fault = RunOutcome::ReadFault {
                pc: BODY,
                address: BAD,
            };
            assert_eq!(jit.run(STOP, 100).unwrap(), fault);
            assert_eq!(oracle(&mut reference, STOP), fault);
            equal_fault_state(&jit, &reference, BAD);
            assert_eq!(jit.state.regs[0], INITIAL[0]);
            assert_eq!(jit.state.regs[2], INITIAL[2]);
            assert_eq!(jit.state.regs[5], INITIAL[5] + 7);
            assert_eq!(jit.state.regs[7], 0x7654_3210);
            for m in [&mut jit, &mut reference] {
                m.protect(BAD, 4096, READ | WRITE).unwrap();
                m.write(BAD, &0xffff_fff1u32.to_le_bytes()).unwrap();
            }
            assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(oracle(&mut reference, STOP), RunOutcome::ReachedStop);
            equal_state(&jit, &reference);
            assert_eq!(jit.state.regs[5], INITIAL[5] + 7, "prefix replayed");
        }
    }
}

#[test]
fn partial_implicit_and_address_registers_survive_a_store_fault_and_retry() {
    let body = code(BODY, |a| {
        a.mov(ah, 0xa5).unwrap();
        a.mov(si, 0x40).unwrap();
        a.mul(ebx).unwrap();
        a.adc(ecx, 7).unwrap(); // Consume MUL's CF and define the other flags.
        a.mov(dword_ptr(edi + edx * 4), ecx).unwrap();
        a.ret().unwrap();
    });
    let store_pc = Decoder::with_ip(32, &body, u64::from(BODY), DecoderOptions::NONE)
        .into_iter()
        .find(|i| i.memory_base() == iced_x86::Register::EDI)
        .unwrap()
        .ip32();
    // Publish the faulting instruction as a native restart entry so retry
    // exercises the footprint prologue rather than an interpreter fallback.
    let regions = [(BODY, body, vec![store_pc])];
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for linked in [false, true] {
            let mut jit = machine(&regions, Some(strategy), linked);
            let mut reference = machine(&regions, None, false);
            for m in [&mut jit, &mut reference] {
                m.state.regs[0] = 0xffff_0011;
                m.state.regs[3] = 3;
                m.state.regs[7] = BAD;
                m.protect(BAD, 4096, READ).unwrap();
            }
            let fault = RunOutcome::WriteFault {
                pc: store_pc,
                address: BAD + 8,
            };
            assert_eq!(jit.run(STOP, 100).unwrap(), fault);
            assert_eq!(oracle(&mut reference, STOP), fault);
            equal_fault_state(&jit, &reference, BAD + 8);
            assert_eq!(jit.state.regs[2], 2);
            assert_eq!(jit.state.regs[6], DATA + 0x40);
            assert_eq!(jit.state.regs[5], INITIAL[5]);
            let before_retry = jit.state.regs;
            for m in [&mut jit, &mut reference] {
                m.protect(BAD, 4096, READ | WRITE).unwrap();
            }
            assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(oracle(&mut reference, STOP), RunOutcome::ReachedStop);
            equal_state(&jit, &reference);
            assert_eq!(jit.state.regs[0], before_retry[0], "MUL replayed");
            assert_eq!(jit.state.regs[1], before_retry[1], "ADC replayed");
            assert_eq!(
                jit.memory().read(BAD + 8, 4).unwrap(),
                &jit.state.regs[1].to_le_bytes()
            );
        }
    }
}

#[test]
fn a_secondary_entry_keeps_conditional_destinations_and_read_only_address_inputs() {
    let body = code(BODY, |a| {
        a.mov(eax, 0xdead_beefu32 as i32).unwrap(); // Bypassed at the side entry.
        a.cmp(ecx, 0).unwrap();
        a.cmove(eax, edx).unwrap();
        a.mov(edi, dword_ptr(ebp + esi * 4 + 8)).unwrap();
        a.ret().unwrap();
    });
    let side_pc = Decoder::with_ip(32, &body, u64::from(BODY), DecoderOptions::NONE)
        .into_iter()
        .find(|i| i.mnemonic() == Mnemonic::Cmp)
        .unwrap()
        .ip32();
    let regions = [(BODY, body, vec![side_pc])];
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for linked in [false, true] {
            for selector in [0, 1] {
                let mut jit = machine(&regions, Some(strategy), linked);
                let mut reference = machine(&regions, None, false);
                for m in [&mut jit, &mut reference] {
                    m.state.eip = side_pc;
                    m.state.regs[1] = selector;
                    m.state.regs[5] = DATA;
                    m.state.regs[6] = 3;
                    m.write(DATA + 20, &0x6ac4_9137u32.to_le_bytes()).unwrap();
                }
                assert_eq!(jit.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(oracle(&mut reference, STOP), RunOutcome::ReachedStop);
                equal_state(&jit, &reference);
                assert_eq!(
                    jit.state.regs[0],
                    if selector == 0 {
                        INITIAL[2]
                    } else {
                        INITIAL[0]
                    }
                );
                assert_eq!(jit.state.regs[3], INITIAL[3]);
                assert_eq!(jit.state.regs[7], 0x6ac4_9137);
                assert_eq!(jit.state.regs[5], DATA);
                assert_eq!(jit.state.regs[6], 3);
            }
        }
    }
}
