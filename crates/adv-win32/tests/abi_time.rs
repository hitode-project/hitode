//! Typed time handlers are reached through real hookable guest thunks.
use adv_abi::{Conv, GuestPtr, ImplementationLevel, Registry};
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PAGE, PARK_ADDR},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;

#[test]
fn typed_time_calls_preserve_clock_results_and_suspend_sleep() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for name in [
        "GetTickCount",
        "QueryPerformanceCounter",
        "QueryPerformanceFrequency",
        "GetSystemTimeAsFileTime",
        "Sleep",
    ] {
        assert_eq!(
            registry.level("kernel32.dll", name),
            Some(ImplementationLevel::Implement)
        );
    }
    registry.add("fixture.dll", "done", Conv::Cdecl { argc: 0 }, |_| {
        adv_machine::Outcome::ExitProcess(0)
    });
    let mut machine = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RWX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    let tick = machine
        .resolve_import("kernel32.dll", "GetTickCount")
        .unwrap();
    let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
    let qpc = machine
        .resolve_import("kernel32.dll", "QueryPerformanceCounter")
        .unwrap();
    let qpf = machine
        .resolve_import("kernel32.dll", "QueryPerformanceFrequency")
        .unwrap();
    let ft = machine
        .resolve_import("kernel32.dll", "GetSystemTimeAsFileTime")
        .unwrap();
    let done = machine.resolve_import("fixture.dll", "done").unwrap();
    let mut code = Asm::new(CODE);
    code.call(tick)
        .mov_mem_eax(DATA)
        .push_imm(7)
        .call(sleep)
        .call(tick)
        .mov_mem_eax(DATA + 4)
        .push_imm(DATA + 8)
        .call(qpc)
        .push_imm(DATA + 16)
        .call(qpf)
        .push_imm(DATA + 24)
        .call(ft)
        .call(done);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("time", CODE, &[], 0x1_0000).unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    assert_eq!(
        machine.cpu.read_u32(DATA + 4).unwrap() - machine.cpu.read_u32(DATA).unwrap(),
        7
    );
    assert_eq!(
        GuestPtr::<i64>::new(DATA + 16).read(&machine.cpu).unwrap(),
        adv_machine::clock::QPC_FREQ as i64
    );
    assert_eq!(
        GuestPtr::<i64>::new(DATA + 8).read(&machine.cpu).unwrap()
            / (adv_machine::clock::QPC_FREQ / 1000) as i64,
        i64::from(machine.cpu.read_u32(DATA + 4).unwrap())
    );
    let ft = GuestPtr::<adv_abi::types::FILETIME>::new(DATA + 24)
        .read(&machine.cpu)
        .unwrap();
    let time = u64::from(ft.dwLowDateTime) | u64::from(ft.dwHighDateTime) << 32;
    assert!((132_223_104_000_070_000..132_223_104_000_080_000).contains(&time));
}

#[test]
fn typed_time_output_fault_stops_without_bypassing_guest_protection() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RWX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::READ).unwrap();
    let qpc = machine
        .resolve_import("kernel32.dll", "QueryPerformanceCounter")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(DATA).call(qpc).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("protected", CODE, &[], 0x1_0000).unwrap();
    let RunResult::Failed(failure) = machine.run_virtual(1_000_000) else {
        panic!("protected output write succeeded");
    };
    assert!(failure.message.contains("ABI guest memory fault"));
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
}

/// Spin until `api` reports `span` units past its first reading, optionally making
/// another host call on every iteration. Returns the machine and the iteration count.
fn spin(
    clock: Clock,
    dll: &str,
    api: &str,
    span: u32,
    interleave: bool,
) -> (Machine<UnicornCpu>, u32) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.add("fixture.dll", "done", Conv::Cdecl { argc: 0 }, |_| {
        adv_machine::Outcome::ExitProcess(0)
    });
    let mut machine = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, clock).unwrap();
    machine.set_time_skip_source(Some(std::sync::Arc::new(|startup| startup)));
    machine.cpu.map(CODE, PAGE, Prot::RWX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    let read = machine.resolve_import(dll, api).unwrap();
    let other = machine
        .resolve_import("kernel32.dll", "GetCurrentThreadId")
        .unwrap();
    let done = machine.resolve_import("fixture.dll", "done").unwrap();
    let qpc = api == "QueryPerformanceCounter";
    let read_eax = |code: &mut Asm| {
        if qpc {
            code.push_imm(DATA + 8).call(read).mov_eax_mem(DATA + 8);
        } else {
            code.call(read);
        }
    };
    let mut code = Asm::new(CODE);
    read_eax(&mut code);
    code.mov_mem_eax(DATA);
    code.bytes.extend_from_slice(&[0x31, 0xf6]); // xor esi, esi
    let top = code.here();
    code.bytes.push(0x46); // inc esi
    if interleave {
        code.call(other);
    }
    read_eax(&mut code);
    code.bytes.extend_from_slice(&[0x2b, 0x05]); // sub eax, [DATA]
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.bytes.push(0x3d); // cmp eax, span
    code.bytes.extend_from_slice(&span.to_le_bytes());
    code.bytes.extend_from_slice(&[0x0f, 0x82]); // jb top
    let next = code.here() + 4;
    code.bytes
        .extend_from_slice(&top.wrapping_sub(next).to_le_bytes());
    code.bytes.extend_from_slice(&[0x89, 0x35]); // mov [DATA + 4], esi
    code.bytes.extend_from_slice(&(DATA + 4).to_le_bytes());
    code.call(done);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("spin", CODE, &[], 0x1_0000).unwrap();
    let started = std::time::Instant::now();
    loop {
        match machine.run() {
            RunResult::Exited(0) => break,
            RunResult::Idle { .. } => std::thread::sleep(std::time::Duration::from_micros(100)),
            other => panic!("spin did not finish: {other:?}"),
        }
        assert!(started.elapsed().as_secs() < 30, "spin never finished");
    }
    let iterations = machine.cpu.read_u32(DATA + 4).unwrap();
    (machine, iterations)
}

#[test]
fn clock_polling_spins_idle_and_skip_like_any_timed_wait() {
    for (dll, api, second) in [
        ("kernel32.dll", "GetTickCount", 1000),
        ("winmm.dll", "timeGetTime", 1000),
        (
            "kernel32.dll",
            "QueryPerformanceCounter",
            adv_machine::clock::QPC_FREQ as u32,
        ),
    ] {
        let started = std::time::Instant::now();
        let (machine, iterations) = spin(Clock::real(), dll, api, second, false);
        // A second of guest time passed by skipping, not by spinning through it.
        assert!(
            started.elapsed().as_millis() < 900,
            "{api} spun in real time"
        );
        assert!(
            machine.sys.scheduling.active_time_skip_us >= 500_000,
            "{api}"
        );
        // About 3,000 polls on an idle host. Under load, real time can end a
        // tick between polls and restart detection; spinning through the
        // second instead would take about a million.
        assert!(iterations < 50_000, "{api}: {iterations} polls");
    }
}

#[test]
// spec: PROC-CLOCK-3
fn reads_between_other_calls_are_not_a_spin() {
    let (_, polled) = spin(
        Clock::virtual_time(),
        "kernel32.dll",
        "GetTickCount",
        6,
        false,
    );
    let (machine, working) = spin(
        Clock::virtual_time(),
        "kernel32.dll",
        "GetTickCount",
        6,
        true,
    );
    // Virtual time advances 1 us per read: only a detected spin waits for the
    // tick, and only after a streak longer than a 256-key scan read twice.
    assert!((520..2_200).contains(&polled), "{polled}");
    assert!(working > 4_000, "{working}");
    assert_eq!(machine.sys.scheduling.active_time_skips, 0);
}
