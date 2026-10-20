//! Legacy fopen("w")/fprintf/fclose writes formatted bytes through the VFS.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const NAME: u32 = DATA + 0x100;
const MODE: u32 = DATA + 0x200;
const FORMAT: u32 = DATA + 0x300;
const WORD: u32 = DATA + 0x400;
const STREAM: u32 = DATA + 0x500;
const RESULT: u32 = DATA + 0x504;

fn call<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    name: &str,
    args: &[u32],
    stream: bool,
) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    if stream {
        code.mov_eax_mem(STREAM).push_eax();
    }
    code.call(machine.resolve_import("msvcrt.dll", name).unwrap());
    code.add_esp((args.len() as u8 + u8::from(stream)) * 4);
}

fn write<B: Backend>(cpu: B, label: &str) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/legacy-fprintf-tests")
        .join(label);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new(&root, "fixture.exe", &root);
    config.os.write_game = true;
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
    machine.cpu.write(NAME, b"log.txt\0").unwrap();
    machine.cpu.write(MODE, b"w\0").unwrap();
    machine
        .cpu
        .write(FORMAT, b"OpenFile\t: %s (%d)\n\0")
        .unwrap();
    machine.cpu.write(WORD, b"save.dat\0").unwrap();
    let mut code = Asm::new(CODE);
    call(&mut machine, &mut code, "fopen", &[NAME, MODE], false);
    code.mov_mem_eax(STREAM);
    call(
        &mut machine,
        &mut code,
        "fprintf",
        &[FORMAT, WORD, 42],
        true,
    );
    code.mov_mem_eax(RESULT);
    call(&mut machine, &mut code, "fclose", &[], true);
    // Falling off the end faults after every observation is stored.
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("Legacy fprintf", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
    let expected = b"OpenFile\t: save.dat (42)\n";
    assert_ne!(machine.cpu.read_u32(STREAM).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(RESULT).unwrap(), expected.len() as u32);
    assert_eq!(std::fs::read(root.join("log.txt")).unwrap(), expected);
}

#[test]
fn legacy_fprintf_writes_formatted_bytes_to_the_stream() {
    write(JitCpu::new(PARK_ADDR, false).unwrap(), "interpreter");
    write(JitCpu::new(PARK_ADDR, true).unwrap(), "jit");
}
