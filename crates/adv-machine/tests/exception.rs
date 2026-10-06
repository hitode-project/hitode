//! Exception delivery uses the ordinary callback/wait scheduler, not a second loop.
use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::exception::{Checkpoint, Exception};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Env, Machine, Outcome, RunResult, Wait};

const CODE: u32 = 0x400000;
const HANDLER: u32 = CODE + 0x200;
const PEER: u32 = CODE + 0x300;
const DATA: u32 = 0x500000;
const SEEN: u32 = DATA + 0x1000;

fn deliver(env: &mut Env<'_>, event: Exception, checkpoint: Checkpoint) -> Outcome {
    assert!(matches!(
        event,
        Exception::Fault(adv_cpu::backend::Fault {
            kind: adv_cpu::backend::FaultKind::Read(DATA),
            pc: CODE
        })
    ));
    assert_eq!(checkpoint.state.eip, CODE);
    let scratch = checkpoint.state.regs[4] - 32;
    env.cpu.write_u32(scratch, 0xdecafbad).unwrap();
    Outcome::call_on_stack(HANDLER, [], scratch, move |env, _| {
        assert_eq!(
            env.cpu.read_u32(scratch).unwrap(),
            0xdecafbad,
            "callback stack overwrote reserved scratch"
        );
        assert_eq!(
            env.cpu.read_u32(SEEN).unwrap(),
            1,
            "peer must have run while handler yielded"
        );
        env.cpu.protect(DATA, 4096, Prot::READ).unwrap();
        Outcome::ResumeExecution(checkpoint)
    })
}

#[test]
fn handler_can_yield_to_peer_then_retry_precise_fault_without_losing_outer_callback() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        registry.add("test", "outer", Conv::Stdcall { argc: 0 }, |env| {
            let original = env.cpu.reg(Reg::Esp);
            Outcome::call(CODE, [], move |env, value| {
                assert_eq!(value.value(), 0x12345678);
                assert_eq!(env.cpu.reg(Reg::Esp), original);
                Outcome::ExitProcess(0)
            })
        });
        registry.add("test", "yield", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::block_then_return(Wait::Yield, 0)
        });
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.sys.exception_handler = Some(deliver);
        machine.cpu.map(CODE, 4096, Prot::RWX).unwrap();
        machine.cpu.map(DATA, 4096, Prot(0)).unwrap();
        machine.cpu.map(SEEN, 4096, Prot::RW).unwrap();
        machine.cpu.write_u32(DATA, 0x12345678).unwrap();
        let outer = machine.resolve_import("test", "outer").unwrap();
        let yield_ = machine.resolve_import("test", "yield").unwrap();
        let mut main = Asm::new(CODE);
        main.mov_eax_mem(DATA).ret();
        machine.cpu.write(CODE, &main.bytes).unwrap();
        let mut handler = Asm::new(HANDLER);
        handler.call(yield_).ret();
        machine.cpu.write(HANDLER, &handler.bytes).unwrap();
        let mut peer = Asm::new(PEER);
        peer.mov_mem_imm(SEEN, 1).ret();
        machine.cpu.write(PEER, &peer.bytes).unwrap();
        machine.spawn("main", outer, &[], 0x10000).unwrap();
        machine.spawn("peer", PEER, &[], 0x10000).unwrap();
        let time = machine.sys.clock.now_us();
        assert!(matches!(machine.run_virtual(100000), RunResult::Exited(0)));
        assert_eq!(machine.sys.clock.now_us(), time);
    }
}

#[test]
fn interruption_cannot_accidentally_use_host_api_return_semantics() {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    machine.sys.exception_handler = Some(|_, _, _| Outcome::Return(0xfeed));
    machine.cpu.map(CODE, 4096, Prot::RWX).unwrap();
    let mut code = Asm::new(CODE);
    code.mov_eax_mem(DATA).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("main", CODE, &[], 0x10000).unwrap();
    let RunResult::Failed(error) = machine.run_virtual(100000) else {
        panic!("invalid disposition escaped")
    };
    assert!(error.message.contains("without an architectural resume"));
    assert_ne!(machine.cpu.reg(Reg::Eax), 0xfeed);
}

#[test]
fn lower_callback_stack_cannot_escape_into_reserved_scratch_or_consume_another_return() {
    for api_call in [false, true] {
        let mut registry = Registry::default();
        registry.add("test", "outer", Conv::Stdcall { argc: 0 }, |env| {
            let original = env.cpu.reg(Reg::Esp);
            env.cpu.write_u32(SEEN, original).unwrap();
            Outcome::call_on_stack(HANDLER, [1, 2], original - 0x100, |_, _| {
                panic!("escaped callback must not consume continuation")
            })
        });
        registry.add("test", "side_effect", Conv::Stdcall { argc: 0 }, |_| {
            panic!("escaped callback must not execute a host API")
        });
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, true).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RWX).unwrap();
        machine.cpu.map(SEEN, 4096, Prot::RW).unwrap();
        let outer = machine.resolve_import("test", "outer").unwrap();
        let side_effect = machine.resolve_import("test", "side_effect").unwrap();
        let mut handler = Asm::new(HANDLER);
        handler.bytes.extend([0x58, 0x8b, 0x25]); // pop eax (return pad); mov esp,[SEEN].
        handler.bytes.extend(SEEN.to_le_bytes());
        if api_call {
            handler.call(side_effect);
        } else {
            handler.bytes.extend([0xff, 0xe0]);
        }
        machine.cpu.write(HANDLER, &handler.bytes).unwrap();
        machine.spawn("main", outer, &[], 0x10000).unwrap();
        let RunResult::Failed(error) = machine.run_virtual(100000) else {
            panic!("stack escape must fail")
        };
        assert!(
            error.message.contains("escaped its continuation stack"),
            "{}",
            error.message
        );
    }
}

#[test]
fn first_fetch_fault_keeps_original_branch_and_stack_after_exception_handler_returns() {
    use adv_cpu::{
        IndirectBranch, IndirectBranchKind,
        backend::{Fault, FaultKind},
    };
    const TARGET: u32 = DATA + 0x200;
    for jit in [false, true] {
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.write_u32(DATA, TARGET).unwrap();
        let mut main = Asm::new(CODE);
        main.bytes.extend([0x83, 0xec, 0x40]); // Reserve readable stack words for capture.
        main.mov_eax_mem(DATA);
        main.bytes.extend([0xff, 0xd0]); // CALL EAX commits before the target fetch fails.
        main.ret();
        machine.cpu.write(CODE, &main.bytes).unwrap();
        let mut handler = Asm::new(HANDLER);
        handler.bytes.extend([0x31, 0xc0]);
        handler.ret();
        machine.cpu.write(HANDLER, &handler.bytes).unwrap();
        machine.sys.exception_handler = Some(|env, _, mut checkpoint| {
            assert!(env.sys.first_cpu_fault.is_some());
            if checkpoint.state.eip == TARGET + 4 {
                return Outcome::Fail("later fault was not handled".into());
            }
            let scratch = checkpoint.state.regs[4] - 32;
            Outcome::call_on_stack(HANDLER, [], scratch, move |env, _| {
                assert_eq!(
                    env.cpu.last_indirect_branch().unwrap().kind,
                    IndirectBranchKind::Return
                );
                // Recover the first exception, then encounter a different fetch fault.
                checkpoint.state.eip = TARGET + 4;
                Outcome::ResumeExecution(checkpoint)
            })
        });
        machine.spawn("main", CODE, &[], 0x10000).unwrap();
        let RunResult::Failed(error) = machine.run_virtual(100000) else {
            panic!("declined exception should terminate")
        };
        assert!(error.message.contains("later fault was not handled"));
        let original = machine.sys.first_cpu_fault.as_ref().unwrap().clone();
        assert_eq!(original.context, 0);
        assert_eq!(
            original.fault,
            Fault {
                kind: FaultKind::Fetch(TARGET),
                pc: TARGET
            }
        );
        assert_eq!(
            original.last_indirect_branch,
            Some(IndirectBranch {
                source: CODE + 8,
                target: TARGET,
                kind: IndirectBranchKind::Call,
            })
        );
        assert_eq!(original.stack.len(), 16);
        assert_eq!(original.stack[0], CODE + 10);
        assert!(original.registers.contains(&(Reg::Eip, TARGET)));
        assert!(
            original
                .registers
                .contains(&(Reg::Esp, original.stack_address))
        );
        assert!(original.registers.contains(&(Reg::Eax, TARGET)));
        let last = machine.cpu.last_indirect_branch().unwrap();
        assert_eq!(last.source, HANDLER + 2);
        assert_eq!(last.kind, IndirectBranchKind::Return);
        assert_ne!(last, original.last_indirect_branch.unwrap());

        let latest = machine.sys.latest_cpu_fault.as_ref().unwrap();
        assert_eq!(
            latest.fault,
            Fault {
                kind: FaultKind::Fetch(TARGET + 4),
                pc: TARGET + 4
            }
        );
        assert_eq!(latest.last_indirect_branch, Some(last));
        assert_eq!(latest.context, 0);
        assert_eq!(machine.sys.first_cpu_fault.as_ref().unwrap(), &original);
    }
}
