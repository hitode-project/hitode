use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Env, Machine, Object, Outcome, RunResult, Wait};

const CODE: u32 = 0x0040_0000;
const CALLBACK: u32 = CODE + 0x100;
const WORKER: u32 = CODE + 0x200;
const DATA: u32 = 0x0041_0000;

fn sleep(env: &mut Env<'_>) -> Outcome {
    Outcome::block_then_return(
        Wait::Until(env.sys.clock.now_us() + u64::from(env.arg(0)) * 1000),
        0,
    )
}
fn spawn(env: &mut Env<'_>) -> Outcome {
    let entry = env.arg(0);
    let id = env.sys.spawn(env.cpu, "W", entry, &[], 0x1_0000).unwrap();
    Outcome::Return(env.sys.context(id).thread_handle)
}
fn wait(env: &mut Env<'_>) -> Outcome {
    Outcome::block_then_return(
        Wait::Object {
            handle: env.arg(0),
            deadline: None,
        },
        0,
    )
}
fn run_callback(env: &mut Env<'_>) -> Outcome {
    Outcome::call(env.arg(0), [env.arg(1)], |_, result| {
        Outcome::Return(result.value() + 100)
    })
}
fn signal(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(u32::from(env.sys.set_event(env.arg(0))))
}
fn exit(env: &mut Env<'_>) -> Outcome {
    Outcome::ExitProcess(env.arg(0))
}

#[test]
fn callback_event_wait_thread_join_and_fs_survive_real_scheduler_switches() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        registry.add("test", "sleep", Conv::Stdcall { argc: 1 }, sleep);
        registry.add("test", "spawn", Conv::Stdcall { argc: 1 }, spawn);
        registry.add("test", "wait", Conv::Stdcall { argc: 1 }, wait);
        registry.add("test", "callback", Conv::Cdecl { argc: 2 }, run_callback);
        registry.add("test", "signal", Conv::Stdcall { argc: 1 }, signal);
        registry.add("test", "exit", Conv::Stdcall { argc: 1 }, exit);
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let event = m.sys.objects.insert(Object::Event {
            manual_reset: false,
            signaled: false,
        });
        let sleep = m.resolve_import("test", "sleep").unwrap();
        let spawn = m.resolve_import("test", "spawn").unwrap();
        let wait = m.resolve_import("test", "wait").unwrap();
        let callback = m.resolve_import("test", "callback").unwrap();
        let signal = m.resolve_import("test", "signal").unwrap();
        let exit = m.resolve_import("test", "exit").unwrap();
        let mut main = Asm::new(CODE);
        main.mov_eax_fs(0x18).mov_mem_eax(DATA + 8);
        main.push_imm(WORKER).call(spawn).mov_mem_eax(DATA + 4);
        main.push_imm(13)
            .push_imm(CALLBACK)
            .call(callback)
            .add_esp(8)
            .mov_mem_eax(DATA + 12);
        main.mov_eax_mem(DATA + 4).push_eax().call(wait);
        main.mov_eax_fs(0x18).mov_mem_eax(DATA + 16);
        main.mov_eax_mem(DATA + 12).push_eax().call(exit);
        let mut cb = Asm::new(CALLBACK);
        cb.push_imm(event)
            .call(wait)
            .mov_eax_fs(0x18)
            .mov_mem_eax(DATA + 20);
        cb.mov_eax_esp(4).add_eax_imm(7).ret_n(4);
        let mut worker = Asm::new(WORKER);
        worker
            .push_imm(5)
            .call(sleep)
            .mov_eax_fs(0x18)
            .mov_mem_eax(DATA + 24);
        worker
            .mov_mem_imm(DATA, 42)
            .push_imm(event)
            .call(signal)
            .ret();
        m.cpu.write(CODE, &main.bytes).unwrap();
        m.cpu.write(CALLBACK, &cb.bytes).unwrap();
        m.cpu.write(WORKER, &worker.bytes).unwrap();
        if jit {
            let mut code = adv_cpu::Region::new(CODE, 0x300);
            code.entries.extend([CALLBACK, WORKER]);
            let mut thunks = adv_cpu::Region::new(
                adv_machine::layout::THUNK_BASE,
                (m.sys.thunks.len() * adv_machine::layout::THUNK_STRIDE) as usize,
            );
            thunks
                .entries
                .extend((0..m.sys.thunks.len()).map(adv_machine::layout::thunk_addr));
            m.cpu.executor_mut().add_region(code).unwrap();
            m.cpu.executor_mut().add_region(thunks).unwrap();
        }
        m.spawn("M", CODE, &[], 0x1_0000).unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(120)), "{result:?}");
        assert!(m.sys.context(1).is_done());
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), 42);
        for at in [DATA + 8, DATA + 16, DATA + 20] {
            assert_eq!(m.cpu.read_u32(at).unwrap(), m.sys.context(0).teb);
        }
        assert_eq!(m.cpu.read_u32(DATA + 24).unwrap(), m.sys.context(1).teb);
        assert_ne!(m.sys.context(0).teb, m.sys.context(1).teb);
        assert!(
            matches!(
                m.sys.objects.get(event),
                Some(Object::Event {
                    signaled: false,
                    ..
                })
            ),
            "auto-reset event must be consumed"
        );
        if jit {
            #[cfg(target_arch = "x86_64")]
            assert_eq!(m.cpu.stats().interpreter_entries, 0);
        }
    }
}

#[test]
fn pal_float_return_preserves_guest_stack_below_st0() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        registry.add("test", "float", Conv::Cdecl { argc: 0 }, |_| {
            Outcome::ReturnF32(1.25)
        });
        registry.add("test", "done", Conv::Cdecl { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let float = m.resolve_import("test", "float").unwrap();
        let done = m.resolve_import("test", "done").unwrap();
        let mut a = Asm::new(CODE);
        a.bytes.extend_from_slice(&[0xd9, 0xe8]); // fld1
        a.call(float);
        a.bytes.extend_from_slice(&[0xde, 0xc1, 0xd9, 0x1d]); // faddp; fstp dword
        a.bytes.extend_from_slice(&DATA.to_le_bytes());
        a.call(done);
        m.cpu.write(CODE, &a.bytes).unwrap();
        m.spawn("M", CODE, &[], 0x1_0000).unwrap();
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        assert_eq!(f32::from_bits(m.cpu.read_u32(DATA).unwrap()), 2.25);
    }
}

#[test]
fn pal_double_return_retains_f64_precision_x87_stack_and_integer_registers() {
    let precise = f64::from_bits(1.0f64.to_bits() + 1);
    for jit in [false, true] {
        let mut registry = Registry::default();
        registry.add("test", "double", Conv::Cdecl { argc: 0 }, |_| {
            Outcome::ReturnF64(f64::from_bits(1.0f64.to_bits() + 1))
        });
        registry.add("test", "done", Conv::Cdecl { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(CODE, 4096, Prot::RX).unwrap();
        m.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let double = m.resolve_import("test", "double").unwrap();
        let done = m.resolve_import("test", "done").unwrap();
        let mut a = Asm::new(CODE);
        a.bytes.extend_from_slice(&[0xd9, 0xe8]); // fld1
        a.bytes.push(0xb8); // mov eax, immediate
        a.bytes.extend_from_slice(&0x12345678u32.to_le_bytes());
        a.bytes.push(0xba); // mov edx, immediate
        a.bytes.extend_from_slice(&0x76543210u32.to_le_bytes());
        a.call(double).mov_mem_eax(DATA + 16);
        a.bytes.extend_from_slice(&[0x89, 0x15]); // mov [DATA+20], edx
        a.bytes.extend_from_slice(&(DATA + 20).to_le_bytes());
        for destination in [DATA, DATA + 8] {
            a.bytes.extend_from_slice(&[0xdd, 0x1d]); // fstp qword
            a.bytes.extend_from_slice(&destination.to_le_bytes());
        }
        a.call(done);
        m.cpu.write(CODE, &a.bytes).unwrap();
        m.spawn("M", CODE, &[], 0x1_0000).unwrap();
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
        let mut bytes = [0; 8];
        m.cpu.read(DATA, &mut bytes).unwrap();
        assert_eq!(f64::from_le_bytes(bytes), precise);
        m.cpu.read(DATA + 8, &mut bytes).unwrap();
        assert_eq!(f64::from_le_bytes(bytes), 1.0);
        assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0x12345678);
        assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 0x76543210);
    }
}
