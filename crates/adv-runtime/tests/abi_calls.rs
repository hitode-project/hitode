//! Execute ABI regressions in a virtual machine, without a host audio device or games.
use adv_abi::{Conv, Registry};
use adv_cpu::backend::{Guest, GuestExt, Prot, asm::Asm};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PAGE, PARK_ADDR},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;

fn machine() -> Machine<UnicornCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_dx::dsound::register(&mut registry);
    registry.add("fixture.dll", "done", Conv::Cdecl { argc: 0 }, |_| {
        adv_machine::Outcome::ExitProcess(0)
    });
    let mut machine = Machine::new(
        UnicornCpu::new(PARK_ADDR).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let directory = std::env::temp_dir().join("adv-abi-fixtures");
    Win32::install(
        &mut machine,
        Win32Config::new(&directory, "fixture.exe", &directory),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RWX).unwrap();
    machine.cpu.map(DATA, PAGE, Prot::RW).unwrap();
    machine
}

fn execute(machine: &mut Machine<UnicornCpu>, mut code: Asm) {
    let done = machine.resolve_import("fixture.dll", "done").unwrap();
    code.call(done);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("abi-fixture", CODE, &[], 0x1_0000).unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
}

#[test]
fn wsprintf_variadic_tail_and_caller_cleanup_remain_correct() {
    let mut machine = machine();
    machine.cpu.write(DATA + 128, b"%u/%s\0").unwrap();
    machine.cpu.write(DATA + 160, b"tail\0").unwrap();
    let wsprintf = machine.resolve_import("user32.dll", "wsprintfA").unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(DATA + 64)
        .push_imm(DATA + 160)
        .push_imm(42)
        .push_imm(DATA + 128)
        .push_imm(DATA)
        .call(wsprintf)
        .mov_mem_eax(DATA + 68);
    code.bytes
        .extend_from_slice(&[0x83, 0xc4, 0x10, 0x89, 0xe0]); // add esp,16; mov eax,esp
    code.mov_mem_eax(DATA + 72);
    execute(&mut machine, code);
    assert_eq!(machine.cpu.read_cstr(DATA, 64).unwrap(), b"42/tail");
    assert_eq!(machine.cpu.read_u32(DATA + 68).unwrap(), 7);
    assert_eq!(
        machine.cpu.read_u32(DATA + 64).unwrap(),
        machine.cpu.read_u32(DATA + 72).unwrap()
    );
}

#[test]
fn sound_buffer_initialize_cleans_three_words_and_returns_existing_failure() {
    let mut machine = machine();
    adv_dx::dsound::install(&mut machine).unwrap();
    machine.ext.get_mut::<adv_dx::dsound::Sound>().enabled = true;
    // A primary buffer has no PCM allocation or playback. No host audio stream is opened.
    let descriptor = DATA + 128;
    machine.cpu.write_u32(descriptor, 36).unwrap();
    machine.cpu.write_u32(descriptor + 4, 1).unwrap();
    let create_device = machine
        .resolve_import("dsound.dll", "DirectSoundCreate8")
        .unwrap();
    let create_buffer = machine
        .resolve_import("dsound.dll", "IDirectSound8::CreateSoundBuffer")
        .unwrap();
    let initialize = machine
        .resolve_import("dsound.dll", "IDirectSoundBuffer8::Initialize")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0)
        .push_imm(DATA)
        .push_imm(0)
        .call(create_device)
        .push_imm(0)
        .push_imm(DATA + 4)
        .push_imm(descriptor)
        .mov_eax_mem(DATA)
        .push_eax()
        .call(create_buffer);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 16)
        .push_imm(descriptor)
        .mov_eax_mem(DATA)
        .push_eax()
        .mov_eax_mem(DATA + 4)
        .push_eax()
        .call(initialize)
        .mov_mem_eax(DATA + 8);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 20);
    execute(&mut machine, code);
    assert_ne!(machine.cpu.read_u32(DATA).unwrap(), 0);
    assert_ne!(machine.cpu.read_u32(DATA + 4).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 0x8878_0082);
    assert_eq!(
        machine.cpu.read_u32(DATA + 16).unwrap(),
        machine.cpu.read_u32(DATA + 20).unwrap()
    );
}
