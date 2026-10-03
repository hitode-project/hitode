use adv_cpu::{
    EXEC, Executor, IndirectBranch, IndirectBranchKind as Kind, READ, Region, RunOutcome, WRITE,
};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const TARGET: u32 = 0x12000;
const DATA: u32 = 0x18000;
const STACK: u32 = 0x1ff00;
const BAD: u32 = 0xdead0000;

fn assemble(at: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    a.assemble(u64::from(at)).unwrap()
}

fn executor(bytes: &[u8], native: bool) -> Executor {
    let mut cpu = Executor::new(CODE, 0x10000).unwrap();
    cpu.protect(CODE, 0x10000, READ | WRITE).unwrap();
    cpu.write(CODE, bytes).unwrap();
    cpu.protect(CODE, 4096, READ | EXEC).unwrap();
    cpu.state.eip = CODE;
    cpu.state.regs[4] = STACK;
    if native {
        cpu.add_region(Region::new(CODE, bytes.len())).unwrap();
    }
    cpu
}

#[test]
fn null_register_jump_and_invalid_target_preserve_the_committed_source() {
    let bytes = assemble(CODE, |a| {
        a.jmp(eax).unwrap();
    });
    for native in [false, true] {
        for target in [0, BAD] {
            let mut cpu = executor(&bytes, native);
            assert_eq!(cpu.state.last_indirect_branch.observed(), None);
            cpu.state.regs[0] = target;
            assert_eq!(
                cpu.run(DATA, 100).unwrap(),
                RunOutcome::FetchFault {
                    pc: target,
                    address: target
                }
            );
            assert_eq!(
                cpu.state.last_indirect_branch,
                IndirectBranch {
                    source: CODE,
                    target,
                    kind: Kind::Jump
                }
            );
            assert_eq!(cpu.state.regs[4], STACK);
        }
    }
}

#[test]
fn null_indirect_call_keeps_its_return_address_and_exact_source() {
    let bytes = assemble(CODE, |a| {
        a.call(dword_ptr(DATA as i32)).unwrap();
    });
    for native in [false, true] {
        let mut cpu = executor(&bytes, native);
        cpu.write(DATA, &0u32.to_le_bytes()).unwrap();
        assert_eq!(
            cpu.run(TARGET, 100).unwrap(),
            RunOutcome::FetchFault { pc: 0, address: 0 }
        );
        assert_eq!(
            cpu.state.last_indirect_branch,
            IndirectBranch {
                source: CODE,
                target: 0,
                kind: Kind::Call
            }
        );
        assert_eq!(cpu.state.regs[4], STACK - 4);
        assert_eq!(
            cpu.memory().read(STACK - 4, 4).unwrap(),
            (CODE + bytes.len() as u32).to_le_bytes()
        );
    }
}

#[test]
fn null_return_keeps_the_return_instruction_and_committed_stack_adjustment() {
    for pop in [0, 12] {
        let bytes = assemble(CODE, |a| {
            if pop == 0 {
                a.ret().unwrap();
            } else {
                a.ret_1(pop).unwrap();
            }
        });
        for native in [false, true] {
            let mut cpu = executor(&bytes, native);
            cpu.write(STACK, &0u32.to_le_bytes()).unwrap();
            assert_eq!(
                cpu.run(DATA, 100).unwrap(),
                RunOutcome::FetchFault { pc: 0, address: 0 }
            );
            assert_eq!(
                cpu.state.last_indirect_branch,
                IndirectBranch {
                    source: CODE,
                    target: 0,
                    kind: Kind::Return
                }
            );
            assert_eq!(cpu.state.regs[4], STACK + 4 + pop as u32);
        }
    }
}

#[test]
fn faults_before_the_transfer_never_replace_the_previous_branch() {
    let previous = IndirectBranch {
        source: CODE - 16,
        target: CODE,
        kind: Kind::Jump,
    };
    for mode in 0..3 {
        let bytes = assemble(CODE, |a| match mode {
            0 => a.call(dword_ptr(BAD as i32)).unwrap(),
            1 => a.call(eax).unwrap(),
            _ => a.ret().unwrap(),
        });
        for native in [false, true] {
            let mut cpu = executor(&bytes, native);
            cpu.state.last_indirect_branch = previous;
            cpu.state.regs[0] = TARGET;
            if mode != 0 {
                cpu.protect(STACK & !4095, 4096, 0).unwrap();
            }
            let result = cpu.run(DATA, 100).unwrap();
            assert!(matches!(
                result,
                RunOutcome::ReadFault { .. } | RunOutcome::WriteFault { .. }
            ));
            assert_eq!(cpu.state.eip, CODE);
            assert_eq!(cpu.state.regs[4], STACK);
            assert_eq!(cpu.state.last_indirect_branch, previous);
        }
    }
}

#[test]
fn successful_linked_jump_is_retained_through_direct_transfers_and_context_restore() {
    let bytes = assemble(CODE, |a| {
        a.jmp(eax).unwrap();
    });
    let target = assemble(TARGET, |a| {
        a.inc(ebx).unwrap();
        a.jmp(0u64).unwrap();
    });
    for native in [false, true] {
        let mut cpu = executor(&bytes, native);
        cpu.write(TARGET, &target).unwrap();
        cpu.protect(TARGET, 4096, READ | EXEC).unwrap();
        if native {
            cpu.add_region(Region::new(TARGET, target.len())).unwrap();
        }
        cpu.state.regs[0] = TARGET;
        assert_eq!(
            cpu.run(DATA, 100).unwrap(),
            RunOutcome::FetchFault { pc: 0, address: 0 }
        );
        let record = IndirectBranch {
            source: CODE,
            target: TARGET,
            kind: Kind::Jump,
        };
        assert_eq!(cpu.state.last_indirect_branch, record);
        assert_eq!(cpu.state.regs[3], 1);
        let context = cpu.save_context().unwrap();
        cpu.state.last_indirect_branch = IndirectBranch::default();
        cpu.restore_context(&context).unwrap();
        assert_eq!(cpu.state.last_indirect_branch, record);
        if native {
            assert!(cpu.stats.linked_entries > 0);
        }
    }
}
