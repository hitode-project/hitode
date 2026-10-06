//! Desktop CRTs read process flags through fs:[0x30], independently of APIs.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, PEB_ADDR, peb},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn unicode<B: Backend>(machine: &Machine<B>, descriptor: u32) -> (String, u32) {
    let length = machine.cpu.read_u16(descriptor).unwrap();
    let maximum = machine.cpu.read_u16(descriptor + 2).unwrap();
    let pointer = machine.cpu.read_u32(descriptor + 4).unwrap();
    assert_eq!(maximum, length + 2);
    let units: Vec<_> = (0..u32::from(length) / 2)
        .map(|i| machine.cpu.read_u16(pointer + i * 2).unwrap())
        .collect();
    assert_eq!(
        machine.cpu.read_u16(pointer + u32::from(length)).unwrap(),
        0
    );
    (String::from_utf16(&units).unwrap(), pointer)
}

fn desktop_startup<B: Backend>(cpu: B, with_crt: bool) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    if with_crt {
        adv_win32::msvcrt::register(&mut registry);
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/process-parameters",
            "向日葵.exe",
            "/nonexistent/process-parameters-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let get_command_line = machine
        .resolve_import("kernel32.dll", "GetCommandLineW")
        .unwrap();
    let mut code = Asm::new(CODE);
    // Same indirect read that failed during EntisGLS CRT exit: PEB ->
    // ProcessParameters -> Flags. Execute it inside the real guest backend.
    code.mov_eax_fs(0x30)
        .add_eax_imm(peb::PROCESS_PARAMETERS)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA)
        .add_eax_imm(8)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 4)
        .call(get_command_line)
        .mov_mem_eax(DATA + 8);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("desktop startup parameters", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let parameters = machine.cpu.read_u32(DATA).unwrap();
    assert_ne!(parameters, 0);
    assert_eq!(
        machine
            .cpu
            .read_u32(PEB_ADDR + peb::PROCESS_PARAMETERS)
            .unwrap(),
        parameters
    );
    let flags = machine.cpu.read_u32(DATA + 4).unwrap();
    assert_eq!(flags & 1, 1, "absolute, normalized string pointers");
    assert_eq!(flags & 0x80000000, 0, "desktop CRT path");
    let (image, _) = unicode(&machine, parameters + 0x38);
    let (command, pointer) = unicode(&machine, parameters + 0x40);
    assert_eq!(image, r"C:\Game\向日葵.exe");
    assert_eq!(command, format!("\"{image}\""));
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), pointer);
    assert!(
        !command.contains("nonexistent"),
        "host paths must not enter the PEB"
    );
}

#[test]
fn process_parameters_are_available_to_guest_crts_and_win32_only_processes() {
    desktop_startup(JitCpu::new(PARK_ADDR, false).unwrap(), false);
    desktop_startup(JitCpu::new(PARK_ADDR, true).unwrap(), true);
    desktop_startup(UnicornCpu::new(PARK_ADDR).unwrap(), true);
}
