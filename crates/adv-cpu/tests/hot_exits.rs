mod support;
use adv_cpu::{CompileStrategy, EXEC, READ, Region, RunOutcome, WRITE};
use iced_x86::code_asm::*;
use support::Executor;

const CODE: u32 = 0x10000;
const BODY: u32 = 0x11000;
const LEAF: u32 = 0x12000;
const STOP: u32 = 0x18000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x2f800;

fn code(at: u32, f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn fixture(strategy: CompileStrategy) -> Executor {
    let mut m = Executor::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    let prefix = code(CODE, |a| {
        a.mov(ah, 1).unwrap();
        a.mul(ebx).unwrap(); // Implicit EAX/EDX writes.
        a.sbb(ecx, 7).unwrap(); // Replace MUL's undefined flags with defined ones.
        a.punpcklbw(xmm7, xmm2).unwrap();
        a.jmp(u64::from(BODY)).unwrap();
    });
    let body = code(BODY, |a| {
        a.mov(edi, dword_ptr(DATA)).unwrap();
        a.call(u64::from(LEAF)).unwrap();
        a.ret().unwrap();
    });
    let leaf = code(LEAF, |a| {
        a.mov(si, 0).unwrap();
        a.paddb(xmm7, xmm1).unwrap();
        a.ret().unwrap();
    });
    for (at, bytes) in [(CODE, prefix), (BODY, body), (LEAF, leaf)] {
        m.write(at, &bytes).unwrap();
        m.protect(at, 4096, READ | EXEC).unwrap();
        let mut region = Region::new(at, bytes.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    m.protect(STOP, 4096, READ | EXEC).unwrap();
    m.state.eip = CODE;
    m.state.eflags = 0x202;
    m.state.regs = [
        0xffff_0021,
        0x7fff_fff9,
        0x1122_3344,
        3,
        STACK,
        5,
        0x1234_ffff,
        7,
    ];
    for (index, xmm) in m.state.xmm.iter_mut().enumerate() {
        *xmm = [index as u8 * 17; 16];
    }
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m.protect(DATA, 4096, 0).unwrap();
    m
}

#[test]
fn cross_region_implicit_partial_and_vector_writes_survive_a_cold_fault_and_return() {
    let mut oracle = fixture(CompileStrategy::Speed);
    let outcome = oracle.run_unicorn(STOP, 100).unwrap();
    assert_eq!(
        outcome,
        RunOutcome::ReadFault {
            pc: BODY,
            address: DATA
        }
    );
    for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
        for linked in [false, true] {
            let mut m = fixture(strategy);
            m.set_region_linking(linked);
            assert_eq!(m.run(STOP, 100).unwrap(), outcome);
            assert_eq!(m.state.eip, oracle.state.eip);
            assert_eq!(m.state.regs, oracle.state.regs);
            assert_eq!(m.state.eflags, oracle.state.eflags);
            assert_eq!(m.state.xmm, oracle.state.xmm);
            assert_eq!(m.state.x87, oracle.state.x87);
            let prefix = m.state.regs[0];
            m.protect(DATA, 4096, READ | WRITE).unwrap();
            m.write(DATA, &123u32.to_le_bytes()).unwrap();
            assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(m.state.regs[0], prefix, "fault retry replayed the multiply");
            assert_eq!(m.state.regs[6], 0x1234_0000);
            assert_eq!(m.state.regs[7], 123);
            assert_eq!(m.state.regs[4], STACK + 4);
            assert_eq!(m.stats.interpreter_entries, 0);
        }
    }
}
