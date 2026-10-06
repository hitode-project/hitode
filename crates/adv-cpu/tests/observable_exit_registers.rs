//! A register overwritten later can still be observed at a CPU boundary or
//! in a fault context before that overwrite. No game code or assets are used.
use adv_cpu::{CompileStrategy, EXEC, Executor, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const BAD: u32 = 0x21000;
const STACK: u32 = 0x2f800;
const OLD: u32 = 0x1234_abcd;
const COMMITTED: u32 = 0x9876_4321;

fn machine(bytes: &[u8], strategy: CompileStrategy) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.write(CODE, bytes).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    let mut region = Region::new(CODE, bytes.len());
    region.compile_strategy = strategy;
    m.add_region(region).unwrap();
    m.state.eip = CODE;
    m.state.regs[3] = OLD;
    m.state.regs[2] = BAD;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m
}

#[test]
fn yielded_register_and_committed_store_survive_a_fault_before_overwrite() {
    for write in [false, true] {
        let mut a = CodeAssembler::new(32).unwrap();
        let mut join = a.create_label();
        a.mov(ebx, COMMITTED).unwrap();
        a.mov(dword_ptr(DATA as i32), ebx).unwrap();
        a.jmp(join).unwrap();
        a.set_label(&mut join).unwrap();
        if write {
            a.mov(dword_ptr(edx), eax).unwrap();
        } else {
            a.mov(eax, dword_ptr(edx)).unwrap();
        }
        // EBX is dead in ordinary successor liveness, but not at the fault.
        a.mov(ebx, 0_u32).unwrap();
        a.mov(dword_ptr(DATA as i32 + 4), ebx).unwrap();
        a.ret().unwrap();
        let bytes = a.assemble(u64::from(CODE)).unwrap();
        let join = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
            .into_iter()
            .find(|i| i.mnemonic() == Mnemonic::Jmp)
            .unwrap()
            .near_branch32();
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            for yield_first in [false, true] {
                let mut m = machine(&bytes, strategy);
                m.write(DATA + 4, &OLD.to_le_bytes()).unwrap();
                if yield_first {
                    // Native fuel counts basic blocks, not guest instructions.
                    assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
                    assert_eq!(m.state.eip, join);
                    assert_eq!(m.state.regs[3], COMMITTED);
                    // An external observer changes a prefix byte after yield.
                    // Resumption must not replay the preceding store.
                    m.write(DATA, &OLD.to_le_bytes()).unwrap();
                }
                m.protect(BAD, 4096, if write { READ } else { 0 }).unwrap();
                let fault = if write {
                    RunOutcome::WriteFault {
                        pc: join,
                        address: BAD,
                    }
                } else {
                    RunOutcome::ReadFault {
                        pc: join,
                        address: BAD,
                    }
                };
                assert_eq!(m.run(STOP, 100).unwrap(), fault);
                assert_eq!(m.state.eip, join);
                assert_eq!(m.state.regs[3], COMMITTED);
                let prefix = if yield_first { OLD } else { COMMITTED };
                assert_eq!(m.memory().read(DATA, 4).unwrap(), prefix.to_le_bytes());
                assert_eq!(m.memory().read(DATA + 4, 4).unwrap(), OLD.to_le_bytes());
                assert_eq!(m.stats.interpreter_instructions, 0);
                m.protect(BAD, 4096, READ | WRITE).unwrap();
                assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
                assert_eq!(m.state.regs[3], 0);
                assert_eq!(m.memory().read(DATA, 4).unwrap(), prefix.to_le_bytes());
                assert_eq!(m.memory().read(DATA + 4, 4).unwrap(), 0_u32.to_le_bytes());
            }
        }
    }
}
