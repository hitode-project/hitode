//! Usage: cargo run -p adv-win32 --release --example hle_wait_precision -- --data-dir DIR [--precise] [--interpreter].
//! Inputs: owned data directory, optional host precision/backend switches.
//! Outputs: aggregate real-clock HLE wait distributions for authored API calls;
//! no game, window, audio device or private guest state is involved.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Object, Outcome, RunResult, layout::PARK_ADDR};
use adv_sync::WakeEvent;
use adv_win32::{Win32, Win32Config};
use std::time::Duration;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn call(machine: &mut Machine<JitCpu>, code: &mut Asm, module: &str, name: &str, args: &[u32]) {
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(machine.resolve_import(module, name).unwrap());
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let data_dir = args
        .windows(2)
        .find(|pair| pair[0] == "--data-dir")
        .expect("--data-dir DIR is required")[1]
        .clone();
    let precise = args.iter().any(|arg| arg == "--precise");
    let jit = !args.iter().any(|arg| arg == "--interpreter");
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.add("fixture", "done", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitProcess(0)
    });
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::real(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(&data_dir, "wait-fixture.exe", &data_dir),
    )
    .unwrap();
    Win32::prepare_process_infrastructure(&mut machine).unwrap();
    machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
        .cpu
        .write(DATA, &(-160_000i64).to_le_bytes())
        .unwrap();
    let event = machine.sys.objects.insert(Object::Event {
        manual_reset: false,
        signaled: false,
    });
    let timer = machine.sys.objects.insert(Object::Timer {
        manual_reset: false,
        signaled: false,
        deadline: None,
        period_us: 0,
    });
    let mut code = Asm::new(CODE);
    for _ in 0..32 {
        call(&mut machine, &mut code, "kernel32.dll", "Sleep", &[16]);
        call(&mut machine, &mut code, "kernel32.dll", "SleepEx", &[16, 1]);
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "WaitForSingleObject",
            &[event, 16],
        );
        call(
            &mut machine,
            &mut code,
            "user32.dll",
            "MsgWaitForMultipleObjects",
            &[0, 0, 0, 16, 0],
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "SetWaitableTimer",
            &[timer, DATA, 0, 0, 0, 0],
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "WaitForSingleObject",
            &[timer, u32::MAX],
        );
        call(
            &mut machine,
            &mut code,
            "winmm.dll",
            "timeSetEvent",
            &[16, 1, event, 0, 0x10],
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "WaitForSingleObject",
            &[event, u32::MAX],
        );
    }
    call(&mut machine, &mut code, "fixture", "done", &[]);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("wait fixture", CODE, &[], 0x10000).unwrap();
    machine.sys.stats.wait_diagnostics =
        Some(adv_machine::wait_diagnostics::WaitDiagnostics::default());
    let wake = WakeEvent::new().unwrap();
    loop {
        match machine.run() {
            RunResult::Exited(0) => break,
            RunResult::Idle { next_deadline } => {
                let delay = next_deadline
                    .map(|at| Duration::from_micros(at.saturating_sub(machine.sys.clock.now_us())));
                let start = std::time::Instant::now();
                let signaled = if precise {
                    wake.wait_precise(delay)
                } else {
                    wake.wait(delay)
                }
                .unwrap();
                machine
                    .sys
                    .stats
                    .wait_diagnostics
                    .as_mut()
                    .unwrap()
                    .host_wait(delay, start.elapsed(), !signaled, false);
            }
            other => panic!("synthetic wait failed: {other:?}"),
        }
    }
    println!(
        "{}",
        machine.sys.stats.wait_diagnostics.as_ref().unwrap().json()
    );
}
