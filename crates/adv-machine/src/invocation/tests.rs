//! Owned host-invocation contracts across backends; no OS or application image.
use super::*;
use crate::{
    Clock, Conv, Machine, RunResult, Wait,
    exception::{Checkpoint, Exception},
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
#[derive(Clone, Copy)]
struct Plan {
    binding: u32,
    mode: u32,
}
trait TestCpu: Backend {
    fn region(&mut self, _length: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn region(&mut self, length: usize) {
        self.executor_mut()
            .add_region(Region::new(CODE, length))
            .unwrap();
    }
}

fn dispatch(env: &mut Env<'_>, _: Exception, mut cp: Checkpoint) -> Outcome {
    let plan = *env.ext.get::<Plan>();
    let arguments = if plan.mode == 4 { vec![] } else { vec![7, 11] };
    env.invoke_host_u32(plan.binding, arguments, move |env, value| {
        assert!(env.sys.invocation_arguments.is_none());
        env.cpu.write_u32(DATA + 8, 1).unwrap();
        cp.state.regs[0] = value.value();
        Outcome::ResumeExecution(cp)
    })
}
fn inner(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(env.arg(0) + env.arg(1))
}
fn invoked(env: &mut Env<'_>) -> Outcome {
    assert_eq!((env.arg(0), env.arg(1)), (7, 11));
    env.cpu.write_u32(DATA + 4, 1).unwrap();
    match env.ext.get::<Plan>().mode {
        0 => Outcome::Return(env.arg(0) + env.arg(1)),
        1 => Outcome::block(Wait::Yield, |env, _| {
            assert_eq!((env.arg(0), env.arg(1)), (7, 11));
            Outcome::Return(env.arg(0) * env.arg(1))
        }),
        2 => {
            let mut cp = env.checkpoint().unwrap();
            cp.state.regs[0] = 42;
            Outcome::ResumeExecution(cp)
        }
        3 => Outcome::call(CODE + 0x100, [23, 31], |env, value| {
            assert_eq!((env.arg(0), env.arg(1)), (7, 11));
            Outcome::Return(value.value() + env.arg(0))
        }),
        5 => {
            let target = env.sys.thunks.resolve(env.cpu, "test", "inner").unwrap();
            let index = (target - THUNK_BASE) / THUNK_STRIDE;
            env.invoke_host_u32(index, vec![17, 19], |env, value| {
                assert_eq!((env.arg(0), env.arg(1)), (7, 11));
                Outcome::Return(value.value() + env.arg(1))
            })
        }
        6 => Outcome::ExitThread(19),
        _ => unreachable!(),
    }
}
struct Bindings;
impl crate::Resolver for Bindings {
    fn host_fn(&self, _: &str, name: &str) -> Option<crate::HostFn> {
        Some(crate::HostFn {
            leaf: None,
            env_leaf: None,
            conv: Conv::Stdcall { argc: 2 },
            handler: match name {
                "invoked" => invoked,
                "inner" => inner,
                _ => return None,
            },
            data: 0,
            trace: true,
            guest: None,
        })
    }
    fn provides(&self, module: &str) -> bool {
        module == "test"
    }
}
fn run<B: TestCpu>(cpu: B, mode: u32) {
    let mut m = Machine::new(cpu, Bindings, Clock::virtual_time()).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let binding = m.resolve_import("test", "invoked").unwrap();
    // Mutable guest entry code is not the implementation of an explicit host invocation.
    m.cpu.write(binding, &[0xf4]).unwrap();
    m.ext.insert(Plan {
        binding: (binding - THUNK_BASE) / THUNK_STRIDE,
        mode,
    });
    m.sys.exception_handler = Some(dispatch);
    let inner = m.resolve_import("test", "inner").unwrap();
    let mut callback = Asm::new(CODE + 0x100);
    callback.push_imm(31).push_imm(23).call(inner).ret_n(8);
    m.cpu.write(CODE + 0x100, &callback.bytes).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x1234);
    code.bytes.extend([0x89, 0x25]);
    code.bytes.extend((DATA + 12).to_le_bytes());
    code.bytes.extend([0xcd, 0x80]);
    code.mov_mem_eax(DATA);
    code.bytes.extend([0x89, 0x25]);
    code.bytes.extend((DATA + 16).to_le_bytes());
    code.add_esp(4);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.cpu.region(0x100 + callback.bytes.len());
    m.spawn("explicit invocation", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(m.sys.invocation_arguments.is_none());
    if mode == 4 {
        assert!(
            matches!(result,RunResult::Failed(ref error) if error.message.contains("argument count")),
            "{result:?}"
        );
        assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0);
        return;
    }
    if mode == 6 {
        assert!(matches!(result, RunResult::Exited(19)), "{result:?}");
        assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0);
        return;
    }
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.cpu.read_u32(DATA).unwrap(),
        [18, 77, 42, 61, 0, 47][mode as usize]
    );
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), u32::from(mode != 2));
    assert_eq!(
        m.cpu.read_u32(DATA + 12).unwrap(),
        m.cpu.read_u32(DATA + 16).unwrap()
    );
}

#[test]
fn explicit_arguments_survive_waits_callbacks_nested_calls_and_nonlocal_returns() {
    for mode in 0..7 {
        run(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        run(JitCpu::new(PARK_ADDR, true).unwrap(), mode);
        run(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}
