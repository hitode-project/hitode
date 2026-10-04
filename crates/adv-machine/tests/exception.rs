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
