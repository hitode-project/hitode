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
