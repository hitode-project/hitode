use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Prot, Reg, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const MARKER: u32 = 0x12345678;
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(".", "fixture.exe", "target/signal-wait-tests"),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn guarded<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    m.spawn("signal guards", PARK_ADDR, &[], 65536).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA + 0x800);
    let signal = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let wait = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: true,
    });
    let full = m.sys.objects.insert(Object::Semaphore { count: 2, max: 2 });
    let semaphore = m.sys.objects.insert(Object::Semaphore { count: 0, max: 2 });
    let unowned = m.sys.objects.insert(Object::Mutex {
        owner: None,
        recursion: 0,
    });
    let mutex = m.sys.objects.insert(Object::Mutex {
        owner: Some(0),
        recursion: 2,
    });
    let denied = m.sys.objects.insert(Object::Process {
        identity: m.sys.process_identity.clone(),
        access: 1,
    });
    for (signal, wait, expected, error) in [
        (signal, 0x123456, u32::MAX, 6),
        (signal, denied, u32::MAX, 5),
        (wait, wait, 0, MARKER),
        (0x123456, wait, u32::MAX, 6),
        (full, wait, u32::MAX, 298),
        (unowned, wait, u32::MAX, 288),
        (semaphore, semaphore, 0, MARKER),
        (mutex, mutex, 0, MARKER),
    ] {
        for (index, value) in [signal, wait, 0, 0].into_iter().enumerate() {
            m.cpu
                .write_u32(DATA + 0x804 + index as u32 * 4, value)
                .unwrap();
        }
        let mut env = Env {
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            context: 0,
            thunk: 0,
        };
        set_last_error(&mut env, MARKER);
        let result = native::signal_wait_front(&mut env);
        assert!(matches!(result, Outcome::Return(value) if value == expected));
        assert_eq!(crate::util::last_error(&env), error);
    }
    assert!(matches!(
        m.sys.objects.get(signal),
        Some(Object::Event {
            signaled: false,
            ..
        })
    ));
    assert!(matches!(
        m.sys.objects.get(semaphore),
        Some(Object::Semaphore { count: 0, .. })
    ));
    assert!(matches!(
        m.sys.objects.get(full),
        Some(Object::Semaphore { count: 2, .. })
    ));
    assert!(matches!(
        m.sys.objects.get(mutex),
        Some(Object::Mutex {
            owner: Some(0),
            recursion: 2
        })
    ));
    let diagnostics = m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
}
fn handshake<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let signal = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let done = m.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let signal_wait = m.resolve_import(K32, "SignalObjectAndWait").unwrap();
    let wait = m.resolve_import(K32, "WaitForSingleObject").unwrap();
    let set = m.resolve_import(K32, "SetEvent").unwrap();
    let reset = m.resolve_import(K32, "ResetEvent").unwrap();
    let queue = m.resolve_import(K32, "QueueUserAPC").unwrap();
    let set_error = m.resolve_import(K32, "SetLastError").unwrap();
    let get_error = m.resolve_import(K32, "GetLastError").unwrap();
    let mut worker = Asm::new(CODE + 0x1000);
    worker
        .push_imm(u32::MAX)
        .push_imm(signal)
        .call(wait)
        .mov_mem_eax(DATA + 0x100)
        .mov_mem_imm(DATA + 0x104, 1)
        .push_imm(done)
        .call(set)
        .mov_mem_imm(DATA + 0x108, 0)
        .mov_eax_mem(DATA + 0x108)
        .ret();
    m.cpu.write(CODE + 0x1000, &worker.bytes).unwrap();
    let mut callback = Asm::new(CODE + 0x2000);
    callback.mov_mem_imm(DATA + 0x110, 42).ret_n(4);
    m.cpu.write(CODE + 0x2000, &callback.bytes).unwrap();
    let mut main = Asm::new(CODE);
    main.push_imm(MARKER)
        .call(set_error)
        .push_imm(0)
        .push_imm(u32::MAX)
        .push_imm(done)
        .push_imm(signal)
        .call(signal_wait)
        .mov_mem_eax(DATA)
        .call(get_error)
        .mov_mem_eax(DATA + 4)
        .push_imm(42)
        .push_imm(u32::MAX - 1)
        .push_imm(CODE + 0x2000)
        .call(queue)
        .mov_mem_eax(DATA + 8)
        .push_imm(MARKER)
        .call(set_error)
        .push_imm(1)
        .push_imm(0)
        .push_imm(done)
        .push_imm(signal)
        .call(signal_wait)
        .mov_mem_eax(DATA + 12)
        .call(get_error)
        .mov_mem_eax(DATA + 16)
        .push_imm(signal)
        .call(reset)
        .push_imm(MARKER)
        .call(set_error)
        .push_imm(0)
        .push_imm(0)
        .push_imm(signal)
        .push_imm(signal)
        .call(signal_wait)
        .mov_mem_eax(DATA + 20)
        .call(get_error)
        .mov_mem_eax(DATA + 24)
        .push_imm(0)
        .push_imm(3)
        .push_imm(done)
        .push_imm(signal)
        .call(signal_wait)
        .mov_mem_eax(DATA + 28)
        .mov_mem_imm(DATA + 32, 0)
        .mov_eax_mem(DATA + 32)
        .ret();
    m.cpu.write(CODE, &main.bytes).unwrap();
    m.spawn("signal caller", CODE, &[], 65536).unwrap();
    m.spawn("signal worker", CODE + 0x1000, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, value) in [0, MARKER, 1, 0xc0, MARKER, 0, MARKER, 0x102]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 4).unwrap(),
            value,
            "row {index}"
        );
    }
    assert_eq!(m.cpu.read_u32(DATA + 0x100).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 0x104).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(DATA + 0x110).unwrap(), 42);
    assert!(m.sys.clock.now_us() >= 3000);
    let diagnostics = m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(diagnostics.warnings.is_empty());
}
#[test]
// spec: PERS-SYNC-1
fn signal_and_wait_checks_objects_before_releasing_state_on_all_backends() {
    for jit in [false, true] {
        guarded(JitCpu::new(PARK_ADDR, jit).unwrap());
    }
    guarded(UnicornCpu::new(PARK_ADDR).unwrap());
}
#[test]
// spec: PERS-SYNC-1
fn signal_and_wait_blocks_delivers_apcs_and_uses_shared_acquisition_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        handshake(cpu);
    }
    handshake(UnicornCpu::new(PARK_ADDR).unwrap());
}
