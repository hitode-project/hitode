mod support;

use adv_cpu::backend::{FilePage, Guest, Prot};
use adv_cpu::{CompileStrategy, EXEC, JitCpu, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, OpKind};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const SLOT: u32 = DATA + 16;
const COUNT: u32 = DATA + 0x3000;
const STACK: u32 = 0x2f800;
const ITERATIONS: u32 = 5;

fn program() -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    let mut top = a.create_label();
    a.set_label(&mut top).unwrap();
    a.mov(eax, dword_ptr(ebp + ebx * 4 + 8)).unwrap();
    a.add(eax, 7).unwrap();
    // ESI is written inside the region, so this store is not an invariant
    // operand. It aliases the fixed read on every iteration nevertheless.
    a.lea(esi, dword_ptr(ebp + ebx * 4 + 8)).unwrap();
    a.mov(dword_ptr(esi), eax).unwrap();
    a.mov(dword_ptr(ebp + ebx * 4 + 12), eax).unwrap();
    // A memory counter intentionally prevents the counted-loop proof.
    a.dec(dword_ptr(ecx)).unwrap();
    a.jnz(top).unwrap();
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

fn seed(m: &mut Executor) {
    m.state.eip = CODE;
    m.state.regs = [
        0x1357_9bdf,
        COUNT,
        0x2244_6688,
        2,
        STACK,
        DATA,
        0x6677_8899,
        0x7788_99aa,
    ];
    m.state.eflags = 0xed7;
}

fn fixture(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.write(COUNT, &ITERATIONS.to_le_bytes()).unwrap();
    m.write(SLOT, &10u32.to_le_bytes()).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        // Fault recovery and authored side entries must re-enter native code.
        region.entries.extend(
            Decoder::with_ip(32, bytes, u64::from(CODE), DecoderOptions::NONE)
                .into_iter()
                .filter(|i| i.op_kinds().any(|kind| kind == OpKind::Memory))
                .map(|i| i.ip32()),
        );
        m.add_region(region).unwrap();
    }
    seed(&mut m);
    m
}

fn word(m: &Executor, at: u32) -> u32 {
    u32::from_le_bytes(m.memory().read(at, 4).unwrap().try_into().unwrap())
}

fn oracle_finish(m: &mut Executor) -> RunOutcome {
    for _ in 0..256 {
        let outcome = m.run_unicorn(STOP, 1).unwrap();
        if outcome != RunOutcome::Budget {
            return outcome;
        }
    }
    panic!("authored invariant loop did not finish");
}

fn equal_cpu(jit: &adv_cpu::Executor, reference: &Executor) {
    assert_eq!(jit.state.regs, reference.state.regs);
    assert_eq!(jit.state.eflags, reference.state.eflags);
    assert_eq!(jit.state.eip, reference.state.eip);
}

fn checkpoint(jit: &Executor, reference: &mut Executor, outcome: &RunOutcome) {
    if *outcome == RunOutcome::Budget {
        // Native fuel counts blocks, not Unicorn instructions. The decreasing
        // memory counter plus exact PC identifies the corresponding prefix.
        for step in 0..256 {
            if reference.state.eip == jit.state.eip && word(reference, COUNT) == word(jit, COUNT) {
                break;
            }
            assert!(step != 255, "oracle missed the native budget prefix");
            assert_eq!(reference.run_unicorn(STOP, 1).unwrap(), RunOutcome::Budget);
        }
    } else {
        assert_eq!(oracle_finish(reference), *outcome);
    }
    equal_cpu(jit, reference);
    assert_eq!(word(jit, COUNT), word(reference, COUNT));
    if let RunOutcome::ReadFault { address, .. } | RunOutcome::WriteFault { address, .. } = outcome
    {
        assert_eq!(jit.state.fault_addr, *address);
    }
}

#[test]
fn unproven_alias_writes_remain_visible_to_every_invariant_read() {
    let bytes = program();
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for budget in [1, 2, 7, 100] {
            let mut jit = fixture(&bytes, Some(strategy));
            let mut reference = fixture(&bytes, None);
            assert_eq!(jit.run(STOP, 0).unwrap(), RunOutcome::Budget);
            checkpoint(&jit, &mut reference, &RunOutcome::Budget);
            for round in 0..128 {
                let outcome = jit.run(STOP, budget).unwrap();
                assert!(jit.state.fuel <= budget);
                checkpoint(&jit, &mut reference, &outcome);
                assert_eq!(
                    jit.memory().read(SLOT, 8).unwrap(),
                    reference.memory().read(SLOT, 8).unwrap()
                );
                if outcome == RunOutcome::ReachedStop {
                    break;
                }
                assert!(round != 127);
                assert_eq!(outcome, RunOutcome::Budget);
            }
            assert_eq!(word(&jit, SLOT), 10 + ITERATIONS * 7);
            assert_eq!(word(&jit, SLOT + 4), word(&jit, SLOT));
            assert_eq!(jit.state.regs[2], 0x2244_6688);
            assert_eq!(jit.state.regs[7], 0x7788_99aa);
            assert_eq!(jit.stats.interpreter_entries, 0);
        }
    }
}

#[test]
fn rejected_cross_page_and_executable_writes_keep_exact_prefixes_and_reenter() {
    let bytes = program();
    let slot = DATA + 4094;
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for permissions in [0, READ, READ | WRITE | EXEC] {
            let mut jit = fixture(&bytes, Some(strategy));
            let mut reference = fixture(&bytes, None);
            let mut uc = fixture(&bytes, None);
            for m in [&mut jit, &mut reference, &mut uc] {
                m.state.regs[3] = 0;
                m.state.regs[5] = slot - 8;
                m.write(slot, &10u32.to_le_bytes()).unwrap();
                m.protect(DATA + 4096, 4096, permissions).unwrap();
            }
            let outcome = jit.run(STOP, 100).unwrap();
            // Unicorn can miss the second page's read protection on an
            // unaligned dword and then fault on a partially attempted store.
            // The interpreter checks the full access before committing it.
            let expected = reference.run(STOP, 100).unwrap();
            assert_eq!(
                outcome, expected,
                "cross-page case: strategy={strategy:?}, permissions={permissions:#x}, jit regs={:x?}, interpreter regs={:x?}",
                jit.state.regs, reference.state.regs
            );
            equal_cpu(&jit, &reference);
            match &outcome {
                RunOutcome::ReadFault { pc, address } | RunOutcome::WriteFault { pc, address } => {
                    // Independently verify every completed instruction with
                    // Unicorn, stopping before the faulting memory operation.
                    for step in 0..256 {
                        if uc.state.eip == *pc && word(&uc, COUNT) == word(&jit, COUNT) {
                            break;
                        }
                        assert!(step != 255, "Unicorn missed the checked fault prefix");
                        assert_eq!(uc.run_unicorn(STOP, 1).unwrap(), RunOutcome::Budget);
                    }
                    equal_cpu(&jit, &uc);
                    assert_eq!(jit.state.fault_addr, *address);
                    assert_eq!(
                        jit.memory().read(slot, 8).unwrap(),
                        uc.memory().read(slot, 8).unwrap()
                    );
                }
                RunOutcome::ReachedStop => {
                    assert_eq!(oracle_finish(&mut uc), RunOutcome::ReachedStop);
                    equal_cpu(&jit, &uc);
                }
                other => panic!("unexpected cross-page outcome: {other:?}"),
            }
            assert_eq!(
                jit.memory().read(slot, 8).unwrap(),
                reference.memory().read(slot, 8).unwrap()
            );
            if permissions & WRITE == 0 {
                assert!(matches!(
                    outcome,
                    RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
                ));
                for m in [&mut jit, &mut reference, &mut uc] {
                    m.protect(DATA + 4096, 4096, READ | WRITE).unwrap();
                }
                let resumed = jit.run(STOP, 100).unwrap();
                assert_eq!(resumed, RunOutcome::ReachedStop);
                assert_eq!(reference.run(STOP, 100).unwrap(), resumed);
                assert_eq!(oracle_finish(&mut uc), resumed);
                equal_cpu(&jit, &reference);
                equal_cpu(&jit, &uc);
                assert_eq!(
                    jit.memory().read(slot, 8).unwrap(),
                    uc.memory().read(slot, 8).unwrap()
                );
                assert_eq!(jit.stats.interpreter_entries, 0);
            } else {
                // Executable guest writes use the existing SMC fallback.
                assert_eq!(outcome, RunOutcome::ReachedStop);
            }
            assert_eq!(word(&jit, slot), 10 + ITERATIONS * 7);
            assert_eq!(
                jit.memory().read(slot, 8).unwrap(),
                reference.memory().read(slot, 8).unwrap()
            );
        }
    }
}

#[test]
fn side_entry_budget_restarts_and_wrapped_invariant_addresses_match_the_oracle() {
    let bytes = program();
    let side = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .find(|i| i.memory_base() == iced_x86::Register::ESI)
        .unwrap()
        .ip32();
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for wrapped in [false, true] {
            let mut jit = fixture(&bytes, Some(strategy));
            let mut reference = fixture(&bytes, None);
            for m in [&mut jit, &mut reference] {
                m.state.eip = side;
                m.state.regs[0] = 17;
                m.state.regs[6] = SLOT;
                if wrapped {
                    // Base addition and scaling both wrap at guest i32 width.
                    m.state.regs[5] = 0xffff_fff0;
                    m.state.regs[3] = 0x4000_8006;
                }
            }
            for round in 0..128 {
                let budget = [0, 1, 2, 3][round % 4];
                let outcome = jit.run(STOP, budget).unwrap();
                assert!(jit.state.fuel <= budget);
                checkpoint(&jit, &mut reference, &outcome);
                assert_eq!(
                    jit.memory().read(SLOT, 8).unwrap(),
                    reference.memory().read(SLOT, 8).unwrap()
                );
                if outcome == RunOutcome::ReachedStop {
                    break;
                }
                assert!(round != 127);
            }
            assert_eq!(word(&jit, SLOT), 10 + ITERATIONS * 7);
            assert_eq!(jit.stats.interpreter_entries, 0);
        }
    }
}

#[test]
fn shared_file_operands_keep_checked_io_and_observe_each_alias_write() {
    let bytes = program();
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        let mut cpu = JitCpu::new(STOP, false).unwrap();
        cpu.map(CODE, 4096, Prot::RX).unwrap();
        cpu.map(STOP, 4096, Prot::RX).unwrap();
        cpu.map(COUNT, 4096, Prot::RW).unwrap();
        cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
        let mut initial_page = [0; 4096];
        initial_page[(SLOT - DATA) as usize..(SLOT - DATA) as usize + 4]
            .copy_from_slice(&10u32.to_le_bytes());
        let page = FilePage::new(initial_page);
        cpu.map_file_pages(DATA, &[page], Prot::RW, false).unwrap();
        cpu.write(CODE, &bytes).unwrap();
        cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
        cpu.write(COUNT, &ITERATIONS.to_le_bytes()).unwrap();
        let mut reference = fixture(&bytes, None);
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        cpu.executor_mut().add_region(region).unwrap();
        cpu.executor_mut().state = reference.state.clone();
        assert_eq!(
            cpu.executor_mut().run(STOP, 100).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(oracle_finish(&mut reference), RunOutcome::ReachedStop);
        equal_cpu(cpu.executor_mut(), &reference);
        let mut actual = [0; 8];
        cpu.read(SLOT, &mut actual).unwrap();
        assert_eq!(actual.as_slice(), reference.memory().read(SLOT, 8).unwrap());
        assert_eq!(
            u32::from_le_bytes(actual[..4].try_into().unwrap()),
            10 + ITERATIONS * 7
        );
        assert_eq!(cpu.stats().interpreter_instructions, 0);
    }
}
