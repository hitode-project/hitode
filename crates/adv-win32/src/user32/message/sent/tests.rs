//! The small authored race that game runs cannot reliably force: A sends to B,
//! B sends back to A, then sleeps inside its WndProc before replying.
use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, SliceResult, layout::PARK_ADDR};
use std::time::{Duration, Instant};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const APP: u32 = 0x8001;
#[derive(Default)]
struct Observed(Vec<u32>);

fn setup(env: &mut Env<'_>) -> Outcome {
    let proc = env
        .sys
        .thunks
        .resolve(env.cpu, "sent-test", "proc")
        .unwrap();
    user_mut(env).register_class("SentTest", 0, proc, 0, 0, 0, 0, 0);
    window::create_window(
        env,
        window::CreateParams {
            ex_style: 0,
            class: window::ClassKey::Name("SentTest".into()),
            class_ptr: 0,
            name_ptr: 0,
            text: String::new(),
            style: window::WS_POPUP,
            x: 0,
            y: 0,
            w: 32,
            h: 32,
            parent: 0,
            menu: 0,
            instance: 0,
            param: 0,
        },
        |_, hwnd| Outcome::Return(hwnd),
    )
}

fn procedure(env: &mut Env<'_>) -> Outcome {
    match env.arg(1) {
        window::wm::NCCREATE => Outcome::Return(1),
        APP => {
            assert_eq!(env.context, 1);
            assert_eq!(
                env.cpu.capture_architecture().unwrap().fs_base,
                env.ctx().teb
            );
            assert!(matches!(in_send(env), Outcome::Return(1)));
            env.ext.get_mut::<Observed>().0.push(1);
            let target = env.cpu.read_u32(DATA).unwrap();
            super::super::send(env, target, APP + 1, 0, 0, |env, result| {
                assert_eq!(env.context, 1);
                assert_eq!(result, 7);
                Outcome::block(Wait::Until(env.sys.clock.now_us() + 1000), |env, _| {
                    assert_eq!(env.context, 1);
                    Outcome::Return(17)
                })
            })
        }
        message if message == APP + 1 => {
            assert_eq!(env.context, 0);
            assert_eq!(
                env.cpu.capture_architecture().unwrap().fs_base,
                env.ctx().teb
            );
            env.ext.get_mut::<Observed>().0.push(0);
            Outcome::Return(7)
        }
        _ => Outcome::Return(0),
    }
}

fn fixture() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    registry.add("sent-test", "setup", Conv::Stdcall { argc: 0 }, setup);
    registry.add("sent-test", "proc", Conv::Stdcall { argc: 4 }, procedure);
    registry.add("sent-test", "finish", Conv::Stdcall { argc: 0 }, |env| {
        exit_thread(env);
        Outcome::ExitThread(0)
    });
    let cpu = JitCpu::new(PARK_ADDR, true).unwrap();
    let mut m = Machine::new(cpu, registry, Clock::real()).unwrap();
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(".", "sent-test.exe", "target/sent-tests"),
    )
    .unwrap();
    m.ext.insert(Observed::default());
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m
}

#[test]
fn sent_callbacks_reenter_the_sender_and_keep_owning_context() {
    let mut m = fixture();
    let setup = m.resolve_import("sent-test", "setup").unwrap();
    let done = m.resolve_import("sent-test", "finish").unwrap();
    let send = m.resolve_import(USER32, "SendMessageA").unwrap();
    let peek = m.resolve_import(USER32, "PeekMessageA").unwrap();
    let mut a = Asm::new(CODE);
    a.call(setup).mov_mem_eax(DATA);
    let wait = a.here();
    a.cmp_mem_imm8(DATA + 4, 0).jz(wait);
    a.push_imm(0)
        .push_imm(0)
        .push_imm(APP)
        .mov_eax_mem(DATA + 4)
        .push_eax()
        .call(send);
    a.mov_mem_eax(DATA + 8)
        .mov_mem_imm(DATA + 12, 1)
        .call(done)
        .hlt();
    let mut b = Asm::new(CODE + 0x200);
    b.call(setup).mov_mem_eax(DATA + 4);
    let poll = b.here();
    b.push_imm(1)
        .push_imm(0)
        .push_imm(0)
        .push_imm(0)
        .push_imm(DATA + 64)
        .call(peek);
    b.cmp_mem_imm8(DATA + 12, 0).jz(poll).call(done).hlt();
    for (pc, code) in [(CODE, a), (CODE + 0x200, b)] {
        m.cpu.write(pc, &code.bytes).unwrap();
        m.cpu
            .executor_mut()
            .add_region(Region::new(pc, code.bytes.len()))
            .unwrap();
        m.spawn("sent", pc, &[], 65536).unwrap();
    }
    run(&mut m);
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 17);
    assert_eq!(m.ext.get::<Observed>().0, [1, 0]);
    assert!(
        m.ext
            .get::<Win32>()
            .user
            .queues
            .values()
            .all(|q| q.sent.pending.is_empty() && q.sent.active.is_empty())
    );
}

fn run(m: &mut Machine<JitCpu>) {
    m.set_execution_budget(Some(Duration::from_secs(3)));
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(Instant::now() < deadline, "cross-thread send deadlocked");
        match m.run_slice(Duration::from_millis(8)) {
            SliceResult::Complete(RunResult::Exited(_)) => break,
            SliceResult::Complete(RunResult::Failed(error)) => panic!("{error}"),
            SliceResult::Complete(RunResult::Idle { .. }) => {
                std::thread::sleep(Duration::from_millis(1))
            }
            SliceResult::Complete(RunResult::HostRequest) => panic!("unexpected process request"),
            SliceResult::Yielded => {}
        }
    }
}

#[test]
fn timed_out_send_is_removed_before_receiver_pumps_and_preserves_output() {
    let mut m = fixture();
    let setup = m.resolve_import("sent-test", "setup").unwrap();
    let done = m.resolve_import("sent-test", "finish").unwrap();
    let send = m.resolve_import(USER32, "SendMessageTimeoutA").unwrap();
    let error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let peek = m.resolve_import(USER32, "PeekMessageA").unwrap();
    m.cpu.write_u32(DATA + 8, 0x12345678).unwrap();
    let mut a = Asm::new(CODE);
    a.call(setup).mov_mem_eax(DATA);
    let wait = a.here();
    a.cmp_mem_imm8(DATA + 4, 0).jz(wait);
    a.push_imm(DATA + 8)
        .push_imm(2)
        .push_imm(1)
        .push_imm(0)
        .push_imm(0)
        .push_imm(APP)
        .mov_eax_mem(DATA + 4)
        .push_eax()
        .call(send);
    a.mov_mem_eax(DATA + 12)
        .call(error)
        .mov_mem_eax(DATA + 16)
        .mov_mem_imm(DATA + 20, 1)
        .call(done)
        .hlt();
    let mut b = Asm::new(CODE + 0x200);
    b.call(setup).mov_mem_eax(DATA + 4);
    let wait = b.here();
    b.cmp_mem_imm8(DATA + 20, 0).jz(wait);
    b.push_imm(1)
        .push_imm(0)
        .push_imm(0)
        .push_imm(0)
        .push_imm(DATA + 64)
        .call(peek)
        .call(done)
        .hlt();
    for (pc, code) in [(CODE, a), (CODE + 0x200, b)] {
        m.cpu.write(pc, &code.bytes).unwrap();
        m.cpu
            .executor_mut()
            .add_region(Region::new(pc, code.bytes.len()))
            .unwrap();
        m.spawn("timeout", pc, &[], 65536).unwrap();
    }
    run(&mut m);
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0x12345678);
    assert_eq!(m.cpu.read_u32(DATA + 12).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), ERROR_TIMEOUT);
    assert!(
        m.ext.get::<Observed>().0.is_empty(),
        "cancelled WndProc was invoked"
    );
}
