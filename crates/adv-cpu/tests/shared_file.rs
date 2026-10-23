//! A view of a host file shared with the file's other mappings, as the
//! in-process Wine server's session memory is.
#![cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;

use adv_cpu::JitCpu;
use adv_cpu::RunOutcome;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use iced_x86::code_asm::*;

const CODE: u32 = 0x10000;
const STOP: u32 = CODE + 0x1000;
const DATA: u32 = 0x20000;
const STACK: u32 = 0x4ff00;
const PAGE: u64 = 0x1000;

// spec: A64-MEM-13
#[test]
fn guest_reads_see_writes_through_other_mappings_and_cannot_write() {
    let path = std::env::temp_dir().join(format!("adv-shared-file-{}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    file.set_len(4 * PAGE).unwrap();

    let mut a = CodeAssembler::new(32).unwrap();
    a.mov(eax, dword_ptr(DATA as i32 + 8)).unwrap();
    a.ret().unwrap();
    let code = a.assemble(CODE as u64).unwrap();
    let mut cpu = JitCpu::new(STOP, false).unwrap();
    for at in [CODE, STOP] {
        cpu.map(at, 4096, Prot::RX).unwrap();
    }
    cpu.map(STACK & !4095, 4096, Prot::RW).unwrap();
    cpu.write(CODE, &code).unwrap();
    // An offset that is no multiple of a 16 KiB host page.
    cpu.map_shared_file(DATA, 2 * PAGE as u32, file.as_fd(), PAGE, Prot::READ)
        .unwrap();

    for value in [0x1234_5678u32, 0x9abc_def0] {
        file.write_all_at(&value.to_le_bytes(), PAGE + 8).unwrap();
        assert_eq!(cpu.read_u32(DATA + 8).unwrap(), value);
        cpu.write(STACK, &STOP.to_le_bytes()).unwrap();
        let machine = cpu.executor_mut();
        machine.state.eip = CODE;
        machine.state.regs[4] = STACK;
        assert_eq!(
            machine.run(STOP, 1_000_000).unwrap(),
            RunOutcome::ReachedStop
        );
        assert_eq!(cpu.executor().state.regs[0], value);
    }
    assert!(!cpu.accessible(DATA, PAGE as u32, Prot::WRITE));
    assert!(cpu.write(DATA, &[1]).is_err());
    assert!(
        cpu.map_shared_file(DATA, PAGE as u32, file.as_fd(), 0, Prot::READ)
            .is_err()
    );
}
