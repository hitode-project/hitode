mod support;
use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic, Register};
use support::Executor;

const CODE: u32 = 0x10000;
const CONSUMER: u32 = CODE + 0x800;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const BAD: u32 = DATA + 4096 + 3;
const STACK: u32 = 0x2f800;
const INITIAL_FAULT_ADDRESS: u32 = 0xdead_beef;

fn assemble(at: u32, f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn machine(bytes: &[u8], strategy: Option<CompileStrategy>) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, bytes.len());
        region.compile_strategy = strategy;
        // The native island after CWD observes the saved partial/vector state.
        region.entries.extend(
            Decoder::with_ip(32, bytes, u64::from(CODE), DecoderOptions::NONE)
                .into_iter()
                .filter(|i| i.mnemonic() == Mnemonic::Cwd)
                .map(|i| i.next_ip32()),
        );
        m.add_region(region).unwrap();
    }
    m
}

fn seed(m: &mut Executor, selector: u32, left: u32, pointer: u32) {
    m.state.regs = [
        left,
        selector,
        pointer,
        1,
        STACK,
        DATA + 0x80,
        0x7fff_ffff,
        0x1357_9bdf,
    ];
    m.state.eip = CODE;
    m.state.eflags = 0xed7;
    m.state.fs_base = DATA + 0x100;
    m.state.fault_addr = INITIAL_FAULT_ADDRESS;
    m.state.xmm =
        std::array::from_fn(|reg| std::array::from_fn(|byte| (reg * 37 + byte * 13 + 7) as u8));
    m.state.mmx = std::array::from_fn(|reg| 0x8070_6050_4030_2010u64.rotate_left((reg * 7) as u32));
    m.state.mmx_mode = 1;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
}

fn prefix(a: &mut CodeAssembler, subtract: bool) {
    if subtract {
        a.sub(eax, ebx).unwrap();
        a.inc(esi).unwrap(); // Inc supplies its own flags but retains Sub's carry.
        a.mov(si, 0x6cab).unwrap();
        a.paddw(xmm3, xmm2).unwrap();
        a.psubb(mm2, mm3).unwrap();
    } else {
        a.add(eax, ebx).unwrap();
        a.mov(si, 0x5ab7).unwrap();
        a.pxor(xmm3, xmm1).unwrap();
        a.paddb(mm2, mm1).unwrap();
    }
    // This alters the live register after its previous value supplied lazy flags.
    a.mov(ah, 0x3a).unwrap();
}

fn equal_state(jit: &mut Executor, uc: &mut Executor) {
    // Flush pending native MMX aliases exactly as a scheduler/context boundary does.
    jit.save_context().unwrap();
    uc.save_context().unwrap();
    assert_eq!(jit.state.regs, uc.state.regs);
    assert_eq!(jit.state.eip, uc.state.eip);
    assert_eq!(jit.state.eflags, uc.state.eflags);
    assert_eq!(jit.state.fs_base, uc.state.fs_base);
    assert_eq!(jit.state.xmm, uc.state.xmm);
    assert_eq!(jit.state.mmx, uc.state.mmx);
    assert_eq!(jit.state.mmx_mode, 0);
    assert_eq!(uc.state.mmx_mode, 0);
}

#[test]
fn read_and_write_fault_predecessors_keep_their_own_lazy_flags_pc_and_vector_state() {
    let bytes = assemble(CODE, |a| {
        let mut write = a.create_label();
        a.test(ecx, ecx).unwrap();
        a.jnz(write).unwrap();
        prefix(a, false);
        a.mov(edi, dword_ptr(edx)).unwrap();
        a.ret().unwrap();
        a.set_label(&mut write).unwrap();
        prefix(a, true);
        a.mov(dword_ptr(edx), eax).unwrap();
        a.ret().unwrap();
    });
    let fault_pcs: Vec<_> = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .filter(|i| i.memory_base() == Register::EDX)
        .map(|i| i.ip32())
        .collect();
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        for (selector, &pc) in fault_pcs.iter().enumerate() {
            for left in [0, u32::MAX, 0x7fff_ffff] {
                let mut uc = machine(&bytes, None);
                for m in [&mut jit, &mut uc] {
                    seed(m, selector as u32, left, BAD);
                    m.write(BAD, &[0x5a; 16]).unwrap();
                    m.protect(BAD & !4095, 4096, if selector == 0 { 0 } else { READ })
                        .unwrap();
                }
                let expected = if selector == 0 {
                    RunOutcome::ReadFault { pc, address: BAD }
                } else {
                    RunOutcome::WriteFault { pc, address: BAD }
                };
                assert_eq!(jit.run(STOP, 1000).unwrap(), expected);
                assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), expected);
                assert_eq!(jit.state.fault_addr, BAD);
                assert_eq!(jit.state.fuel, 998);
                equal_state(&mut jit, &mut uc);
                assert_eq!(jit.memory().read(BAD, 16).unwrap(), [0x5a; 16]);
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0);
    }
}

#[test]
fn budget_merge_and_dispatch_save_state_before_another_region_consumes_it() {
    let bytes = assemble(CODE, |a| {
        let mut subtract = a.create_label();
        let mut join = a.create_label();
        a.test(ecx, ecx).unwrap();
        a.jnz(subtract).unwrap();
        prefix(a, false);
        a.jmp(join).unwrap();
        a.set_label(&mut subtract).unwrap();
        prefix(a, true);
        a.jmp(join).unwrap();
        a.set_label(&mut join).unwrap();
        a.nop().unwrap();
        a.jmp(u64::from(CONSUMER)).unwrap();
    });
    let join = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .find(|i| i.mnemonic() == Mnemonic::Nop)
        .unwrap()
        .ip32();
    let consumer = assemble(CONSUMER, |a| {
        a.setb(dl).unwrap();
        a.movdqu(xmmword_ptr(DATA as i32 + 64), xmm3).unwrap();
        a.movq(qword_ptr(DATA as i32 + 96), mm2).unwrap();
        a.ret().unwrap();
    });
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        jit.write(CONSUMER, &consumer).unwrap();
        let mut region = Region::new(CONSUMER, consumer.len());
        region.compile_strategy = strategy;
        jit.add_region(region).unwrap();
        for selector in [0, 1] {
            for left in [0, u32::MAX, 0x7fff_ffff] {
                // Keep the oracle independent of previously translated blocks when
                // switching between an internal stop boundary and the final stop.
                let mut uc = machine(&bytes, None);
                uc.write(CONSUMER, &consumer).unwrap();
                for m in [&mut jit, &mut uc] {
                    seed(m, selector, left, 0x89ab_cdef);
                }
                assert_eq!(jit.run(STOP, 2).unwrap(), RunOutcome::Budget);
                assert_eq!(uc.run_unicorn(join, 1000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(jit.state.eip, join);
                assert_eq!(jit.state.fuel, 0);
                assert_eq!(jit.state.fault_addr, INITIAL_FAULT_ADDRESS);
                equal_state(&mut jit, &mut uc);
                let entries = jit.stats.native_entries;
                assert_eq!(jit.run(STOP, 2).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                equal_state(&mut jit, &mut uc);
                assert_eq!(
                    jit.stats.native_entries,
                    entries + 2,
                    "dispatch must reach the second compiled region"
                );
                assert_eq!(jit.state.fuel, 0);
                assert_eq!(
                    jit.memory().read(DATA + 64, 16).unwrap(),
                    uc.memory().read(DATA + 64, 16).unwrap()
                );
                assert_eq!(
                    jit.memory().read(DATA + 96, 8).unwrap(),
                    uc.memory().read(DATA + 96, 8).unwrap()
                );
            }
        }
        assert_eq!(jit.stats.interpreter_entries, 0);
    }
}

#[test]
fn cold_exit_saves_each_predecessor_before_the_interpreted_instruction_runs() {
    let bytes = assemble(CODE, |a| {
        let mut subtract = a.create_label();
        a.test(ecx, ecx).unwrap();
        a.jnz(subtract).unwrap();
        prefix(a, false);
        a.cwd().unwrap();
        a.setb(dl).unwrap();
        a.ret().unwrap();
        a.set_label(&mut subtract).unwrap();
        prefix(a, true);
        a.cwd().unwrap();
        a.setb(dl).unwrap();
        a.ret().unwrap();
    });
    let interpreted_pcs: Vec<_> =
        Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
            .into_iter()
            .filter(|i| i.mnemonic() == Mnemonic::Cwd)
            .map(|i| i.ip32())
            .collect();
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        for (selector, &pc) in interpreted_pcs.iter().enumerate() {
            for left in [0, u32::MAX, 0x7fff_ffff] {
                let mut uc = machine(&bytes, None);
                for m in [&mut jit, &mut uc] {
                    seed(m, selector as u32, left, 0x89ab_cdef);
                }
                let interpreter_entries = jit.stats.interpreter_entries;
                let interpreter_instructions = jit.stats.interpreter_instructions;
                assert_eq!(jit.run(STOP, 2).unwrap(), RunOutcome::Budget);
                assert_eq!(uc.run_unicorn(pc, 1000).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(jit.state.eip, pc);
                assert_eq!(jit.state.fuel, 0);
                assert_eq!(jit.state.fault_addr, INITIAL_FAULT_ADDRESS);
                // Exhausting the native budget before CWD does not execute
                // or count an interpreter instruction.
                assert_eq!(jit.stats.interpreter_entries, interpreter_entries);
                assert_eq!(jit.stats.interpreter_instructions, interpreter_instructions);
                equal_state(&mut jit, &mut uc);
                assert_eq!(jit.state.regs[2], 0x89ab_cdef, "CWD has not executed yet");
                assert_eq!(jit.run(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                assert!(jit.stats.interpreter_entries > interpreter_entries);
                assert!(jit.stats.interpreter_instructions > interpreter_instructions);
                assert_eq!(uc.run_unicorn(STOP, 1000).unwrap(), RunOutcome::ReachedStop);
                equal_state(&mut jit, &mut uc);
            }
        }
    }
}
