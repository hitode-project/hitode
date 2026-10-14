//! The 16-bit-era HFILE functions over a writable game directory.
use std::path::PathBuf;

use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const HANDLE: u32 = DATA + 0x10;
const MISSING: u32 = DATA + 0x200;
const CREATED: u32 = DATA + 0x220;
const TEXT: u32 = DATA + 0x240;
const BUFFER: u32 = DATA + 0x300;
const HFILE_ERROR: u32 = u32::MAX;

fn call_with_handle(code: &mut Asm, api: u32, arguments: &[u32]) {
    for &argument in arguments.iter().rev() {
        code.push_imm(argument);
    }
    code.mov_eax_mem(HANDLE).push_eax().call(api);
}

#[test]
// spec: WIN32-API-14
fn hfile_functions_create_transfer_seek_truncate_and_close() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/legacy-file-handles/{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("game")).unwrap();
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let config = Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 0x1000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
    machine.cpu.write(MISSING, b"missing.txt\0").unwrap();
    machine.cpu.write(CREATED, b"created.txt\0").unwrap();
    machine.cpu.write(TEXT, b"hello world").unwrap();
    let api =
        |machine: &mut Machine<JitCpu>, name| machine.resolve_import("kernel32.dll", name).unwrap();
    let (open, create) = (api(&mut machine, "_lopen"), api(&mut machine, "_lcreat"));
    let (read, write) = (api(&mut machine, "_lread"), api(&mut machine, "_lwrite"));
    let (seek, close) = (api(&mut machine, "_llseek"), api(&mut machine, "_lclose"));

    let mut code = Asm::new(CODE);
    code.push_imm(0)
        .push_imm(MISSING)
        .call(open)
        .mov_mem_eax(DATA);
    code.push_imm(0)
        .push_imm(CREATED)
        .call(create)
        .mov_mem_eax(HANDLE);
    call_with_handle(&mut code, write, &[TEXT, 11]);
    code.mov_mem_eax(DATA + 0x14);
    call_with_handle(&mut code, seek, &[0, 0]);
    code.mov_mem_eax(DATA + 0x18);
    call_with_handle(&mut code, read, &[BUFFER, 64]);
    code.mov_mem_eax(DATA + 0x1c);
    // A zero-length write truncates at the current position.
    call_with_handle(&mut code, seek, &[5, 0]);
    call_with_handle(&mut code, write, &[TEXT, 0]);
    code.mov_mem_eax(DATA + 0x20);
    call_with_handle(&mut code, seek, &[0, 2]);
    code.mov_mem_eax(DATA + 0x24);
    call_with_handle(&mut code, close, &[]);
    code.mov_mem_eax(DATA + 0x28);
    code.push_imm(0)
        .push_imm(CREATED)
        .call(open)
        .mov_mem_eax(HANDLE);
    call_with_handle(&mut code, read, &[BUFFER + 0x40, 64]);
    code.mov_mem_eax(DATA + 0x2c);
    call_with_handle(&mut code, close, &[]);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("hfile", CODE, &[], 0x10000).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");

    let word = |offset| machine.cpu.read_u32(DATA + offset).unwrap();
    assert_eq!(word(0), HFILE_ERROR);
    assert_ne!(machine.cpu.read_u32(HANDLE).unwrap(), HFILE_ERROR);
    let results: Vec<u32> = (0x14..=0x2c).step_by(4).map(word).collect();
    assert_eq!(results, [11, 0, 11, 0, 5, 0, 5]);
    let mut text = [0; 11];
    machine.cpu.read(BUFFER, &mut text).unwrap();
    assert_eq!(&text, b"hello world");
    let mut text = [0; 5];
    machine.cpu.read(BUFFER + 0x40, &mut text).unwrap();
    assert_eq!(&text, b"hello");
    let _ = std::fs::remove_dir_all(&root);
}
