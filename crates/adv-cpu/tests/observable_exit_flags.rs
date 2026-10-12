//! A future guest flag writer does not make the current EFLAGS unobservable:
//! budget, interrupt and exception contexts expose the committed prefix.
mod support;
use adv_cpu::{
    CompileStrategy, CpuState, EXEC, ExecutionTrap, Memory, READ, Region, RunOutcome, WRITE,
};
use iced_x86::code_asm::*;
use iced_x86::{Decoder, DecoderOptions, Mnemonic};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use support::Executor;

const CODE: u32 = 0x10000;
const STOP: u32 = 0x18000;
const BAD: u32 = 0x21000;
const STACK: u32 = 0x2f800;

fn code(build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(CODE)).unwrap()
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
        m.add_region(region).unwrap();
    }
    m.state.eip = CODE;
    m.state.eflags = 0x202;
    m.state.regs[0] = 0;
    m.state.regs[2] = BAD;
    m.state.regs[3] = 1;
    m.state.regs[4] = STACK;
    m.write(STACK, &STOP.to_le_bytes()).unwrap();
    m
}

#[cfg_attr(target_arch = "aarch64", ignore = "Cranelift execution shape")]
#[test]
fn budget_and_memory_faults_publish_flags_before_a_future_full_writer() {
    for write in [false, true] {
        let bytes = code(|a| {
            let mut join = a.create_label();
            a.cmp(eax, ebx).unwrap();
            a.jmp(join).unwrap();
            a.set_label(&mut join).unwrap();
            if write {
                a.mov(dword_ptr(edx), eax).unwrap();
            } else {
                a.mov(ecx, dword_ptr(edx)).unwrap();
            }
            a.add(edi, 1).unwrap();
            a.ret().unwrap();
        });
        let join = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
            .into_iter()
            .find(|i| i.mnemonic() == Mnemonic::Jmp)
            .unwrap()
            .near_branch32();
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut uc = machine(&bytes, None);
            assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::Budget);
            assert_eq!(uc.run_unicorn(join, 100).unwrap(), RunOutcome::ReachedStop);
            assert_eq!(jit.state.eip, join);
            assert_eq!(jit.state.regs, uc.state.regs);
            assert_eq!(jit.state.eflags, uc.state.eflags);
            assert_eq!(jit.state.eflags, 0x297);

            for m in [&mut jit, &mut uc] {
                m.protect(BAD, 4096, if write { READ } else { 0 }).unwrap();
            }
            let expected = uc.run_unicorn(STOP, 100).unwrap();
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
            assert_eq!(expected, fault);
            assert_eq!(jit.run(STOP, 100).unwrap(), expected);
            assert_eq!(jit.state.regs, uc.state.regs);
            assert_eq!(jit.state.eflags, uc.state.eflags);

            // Also fault in the same native invocation that produced CMP's
            // lazy flags, without the preceding budget exit canonicalizing it.
            let mut jit = machine(&bytes, Some(strategy));
            jit.protect(BAD, 4096, if write { READ } else { 0 })
                .unwrap();
            assert_eq!(jit.run(STOP, 100).unwrap(), fault);
            assert_eq!(jit.state.regs, uc.state.regs);
            assert_eq!(jit.state.eflags, uc.state.eflags);
            support::x64_shape!(assert_eq!(jit.stats.interpreter_instructions, 0));
        }
    }
}

struct NotifyAtTrap(Arc<AtomicBool>);

#[cfg_attr(target_arch = "aarch64", ignore = "Cranelift execution shape")]
#[test]
fn carry_controls_publish_exact_flags_at_budget_and_fault_boundaries() {
    for carry in 0..3 {
        let bytes = code(|a| {
            a.cmp(eax, ebx).unwrap();
            match carry {
                0 => a.clc(),
                1 => a.stc(),
                _ => a.cmc(),
            }
            .unwrap();
            let mut boundary = a.create_label();
            a.jmp(boundary).unwrap();
            a.set_label(&mut boundary).unwrap();
            a.mov(esi, dword_ptr(edx)).unwrap();
            // A later complete flag writer cannot hide the carry edit at
            // either observable boundary before this instruction executes.
            a.add(edi, 1).unwrap();
            a.ret().unwrap();
        });
        let boundary = Decoder::with_ip(32, &bytes, u64::from(CODE), DecoderOptions::NONE)
            .into_iter()
            .find(|i| i.mnemonic() == Mnemonic::Jmp)
            .unwrap()
            .near_branch32();
        for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
            let mut jit = machine(&bytes, Some(strategy));
            let mut reference = machine(&bytes, None);
            assert_eq!(jit.run(STOP, 1).unwrap(), RunOutcome::Budget);
            assert_eq!(
                reference.run_unicorn(boundary, 100).unwrap(),
                RunOutcome::ReachedStop
            );
            assert_eq!(jit.state.eip, boundary);
            assert_eq!(jit.state.regs, reference.state.regs);
            assert_eq!(jit.state.eflags, reference.state.eflags);
            assert_eq!(jit.state.eflags, if carry == 1 { 0x297 } else { 0x296 });
            for m in [&mut jit, &mut reference] {
                m.protect(BAD, 4096, 0).unwrap();
            }
            let fault = RunOutcome::ReadFault {
                pc: boundary,
                address: BAD,
            };
            assert_eq!(reference.run_unicorn(STOP, 100).unwrap(), fault);
            assert_eq!(jit.run(STOP, 100).unwrap(), fault);
            assert_eq!(jit.state.regs, reference.state.regs);
            assert_eq!(jit.state.eflags, reference.state.eflags);

            let mut fresh = machine(&bytes, Some(strategy));
            fresh.protect(BAD, 4096, 0).unwrap();
            assert_eq!(fresh.run(STOP, 100).unwrap(), fault);
            assert_eq!(fresh.state.regs, reference.state.regs);
            assert_eq!(fresh.state.eflags, reference.state.eflags);
            support::x64_shape!(assert_eq!(fresh.stats.interpreter_instructions, 0));
        }
    }
}

impl ExecutionTrap for NotifyAtTrap {
    fn call(
        &mut self,
        _state: &mut CpuState,
        _memory: &mut Memory,
        vector: u32,
    ) -> Result<bool, String> {
        assert_eq!(vector, 0x2e);
        self.0.store(true, Ordering::Release);
        Ok(false)
    }
}

#[cfg_attr(target_arch = "aarch64", ignore = "Cranelift execution shape")]
#[test]
fn a_native_backedge_interrupt_publishes_the_last_lazy_flags() {
    let bytes = code(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.add(edi, 1).unwrap();
        a.int(0x2e).unwrap();
        a.cmp(eax, ebx).unwrap();
        a.jmp(top).unwrap();
    });
    let reference = code(|a| {
        a.cmp(eax, ebx).unwrap();
        a.ret().unwrap();
    });
    let mut uc = machine(&reference, None);
    assert_eq!(uc.run_unicorn(STOP, 100).unwrap(), RunOutcome::ReachedStop);
    for strategy in [CompileStrategy::Speed, CompileStrategy::Fast] {
        let mut jit = machine(&bytes, Some(strategy));
        let flag = Arc::new(AtomicBool::new(false));
        jit.set_host_interrupt_flag(flag.clone());
        let mut handler = NotifyAtTrap(flag.clone());
        assert_eq!(
            jit.run_with_traps(STOP, 1000, &mut handler, false).unwrap(),
            RunOutcome::Interrupted
        );
        assert!(flag.load(Ordering::Acquire));
        assert_eq!(jit.state.eip, CODE);
        assert_eq!(
            jit.state.regs[7], 1,
            "no instruction from the next iteration ran"
        );
        assert_eq!(jit.state.eflags, uc.state.eflags);
        support::x64_shape!(assert_eq!(jit.stats.interpreter_instructions, 0));
    }
}
