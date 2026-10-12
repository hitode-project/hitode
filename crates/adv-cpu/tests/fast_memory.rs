//! Dynamic operands exercise the fast page table and its exact cold fallback.
// Tests x64 engine mechanisms; the a64 engine has its own.
#![cfg(not(target_arch = "aarch64"))]
use adv_cpu::{
    CompileStrategy, CpuState, EXEC, ExecutionTrap, Executor, Memory, READ, Region, RunOutcome,
    WRITE,
};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x40000;
const STOP: u32 = 0x18000;

fn machine(code: &[u8], full: bool, strategy: Option<CompileStrategy>) -> Executor {
    let mut m = if full {
        let mut m = Executor::address_space().unwrap();
        for (address, length) in [(CODE, 4096), (DATA, 8192), (STACK, 4096)] {
            m.map(address, length, READ | WRITE).unwrap();
        }
        m
    } else {
        let mut m = Executor::new(CODE, 0x40000).unwrap();
        m.protect(CODE, 0x40000, READ | WRITE).unwrap();
        m
    };
    m.enable_auto_jit(false);
    m.write(CODE, code).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    let bytes: Vec<_> = (0..8192).map(|n| (n * 37 + 13) as u8).collect();
    m.write(DATA, &bytes).unwrap();
    if let Some(strategy) = strategy {
        let mut region = Region::new(CODE, code.len());
        region.compile_strategy = strategy;
        m.add_region(region).unwrap();
    }
    reset(&mut m);
    m
}

fn reset(m: &mut Executor) {
    m.state = CpuState::default();
    m.state.eip = CODE;
    m.state.regs[0] = 0xabcd_ef12;
    m.state.regs[3] = 0x7fff_ffff;
    m.state.regs[4] = STACK;
    m.state.regs[6] = DATA;
    m.state.eflags = 0x803;
    m.state.xmm[0] = [0x93; 16];
    m.state.mmx[0] = 0x1234_5678_9abc_def0;
}

fn operand(width: u32, write: bool) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    // An observable prefix must survive a later memory fault exactly.
    a.inc(ebx).unwrap();
    match (width, write) {
        (1, false) => a.mov(al, byte_ptr(esi)).unwrap(),
        (1, true) => a.mov(byte_ptr(esi), al).unwrap(),
        (2, false) => a.mov(ax, word_ptr(esi)).unwrap(),
        (2, true) => a.mov(word_ptr(esi), ax).unwrap(),
        (4, false) => a.mov(eax, dword_ptr(esi)).unwrap(),
        (4, true) => a.mov(dword_ptr(esi), eax).unwrap(),
        (8, false) => a.movq(mm0, qword_ptr(esi)).unwrap(),
        (8, true) => a.movq(qword_ptr(esi), mm0).unwrap(),
        (16, false) => a.movdqu(xmm0, xmmword_ptr(esi)).unwrap(),
        (16, true) => a.movdqu(xmmword_ptr(esi), xmm0).unwrap(),
        _ => unreachable!(),
    }
    a.ret().unwrap();
    a.assemble(u64::from(CODE)).unwrap()
}

fn compare(mut jit: Executor, mut reference: Executor) {
    // Both engines report the first inaccessible byte, as hardware does.
    let expected = reference.run(STOP, 100).unwrap();
    let actual = jit.run(STOP, 100).unwrap();
    assert_eq!(actual, expected);
    assert_eq!(jit.state.eip, reference.state.eip);
    assert_eq!(jit.state.regs, reference.state.regs);
    assert_eq!(jit.state.eflags, reference.state.eflags);
    assert_eq!(jit.state.xmm, reference.state.xmm);
    // MMX uses a deferred mode internally; compare architectural state after
    // the public execution boundary commits it.
    assert_eq!(jit.state.mmx, reference.state.mmx);
    assert_eq!(
        jit.memory().read(DATA, 8192).unwrap(),
        reference.memory().read(DATA, 8192).unwrap()
    );
    assert!(jit.stats.native_entries > 0);
}

#[test]
fn dynamic_widths_preserve_fault_prefixes_and_neighbor_fallbacks() {
    for full in [false, true] {
        for strategy in [CompileStrategy::Fast, CompileStrategy::Speed] {
            for width in [1, 2, 4, 8, 16] {
                for write in [false, true] {
                    let code = operand(width, write);
                    for (offset, next) in [
                        (32, READ | WRITE),
                        (32, 0), // A fast miss must still accept this access.
                        (4096 - width / 2, 0),
                        (4096 - width / 2, READ),
                        (32, READ | WRITE | EXEC),
                    ] {
                        let mut jit = machine(&code, full, Some(strategy));
                        let mut reference = machine(&code, full, None);
                        for m in [&mut jit, &mut reference] {
                            m.state.regs[6] = DATA + offset;
                            m.protect(DATA + 4096, 4096, next).unwrap();
                        }
                        compare(jit, reference);
                    }
                }
            }
        }
    }
}

#[test]
fn full_window_end_never_wraps_and_short_window_checks_remain() {
    let code = operand(4, false);
    for full in [false, true] {
        for address in if full {
            [u32::MAX - 1, u32::MAX - 3, 0]
        } else {
            [CODE - 1, 0x50000 - 2, 0x50000]
        } {
            let mut jit = machine(&code, full, Some(CompileStrategy::Speed));
            let mut reference = machine(&code, full, None);
            for m in [&mut jit, &mut reference] {
                if full {
                    m.map(0xffff_f000, 4096, READ | WRITE).unwrap();
                    m.write(u32::MAX - 3, &42_u32.to_le_bytes()).unwrap();
                }
                m.state.regs[6] = address;
            }
            compare(jit, reference);
        }
    }
}

#[test]
fn trap_revocation_exits_before_a_cached_fast_grant_is_reused() {
    struct Revoke;
    impl ExecutionTrap for Revoke {
        fn call(&mut self, _: &mut CpuState, memory: &mut Memory, _: u32) -> Result<bool, String> {
            memory.protect(DATA, 4096, 0)?;
            Ok(false)
        }
    }
    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(eax, dword_ptr(esi)).unwrap();
    a.int(0x2e).unwrap();
    let fault = CODE + a.assemble(u64::from(CODE)).unwrap().len() as u32;
    a.mov(edx, dword_ptr(esi)).unwrap();
    a.ret().unwrap();
    let code = a.assemble(u64::from(CODE)).unwrap();
    let mut m = machine(&code, true, Some(CompileStrategy::Speed));
    assert_eq!(
        m.run_with_traps(STOP, 100, &mut Revoke, false).unwrap(),
        RunOutcome::ReadFault {
            pc: fault,
            address: DATA,
        }
    );
    assert_eq!(m.state.regs[0], 0x7c57_320d);
    assert_eq!(m.state.regs[2], 0);
}

#[test]
fn unmap_and_remap_refresh_the_preceding_pages_fast_grants() {
    let code = operand(4, false);
    let mut m = machine(&code, true, Some(CompileStrategy::Speed));
    m.state.regs[6] = DATA + 4094;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    m.unmap(DATA + 4096, 4096).unwrap();
    reset(&mut m);
    m.state.regs[6] = DATA + 4094;
    assert!(matches!(
        m.run(STOP, 100).unwrap(),
        RunOutcome::ReadFault { .. }
    ));
    m.map(DATA + 4096, 4096, READ | WRITE).unwrap();
    reset(&mut m);
    m.state.regs[6] = DATA + 4094;
    assert_eq!(m.run(STOP, 100).unwrap(), RunOutcome::ReachedStop);
}
