use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CALLBACK: u32 = CODE + 512;
const MODULE: &str = "msvcr90.dll";

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.get(MODULE, "_calloc_crt").unwrap().conv,
        Conv::Cdecl { argc: 2 }
    );
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/calloc-crt-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&directory, "fixture.exe", &directory),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn env<B: Backend>(machine: &mut Machine<B>, entry: u32) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: (entry - THUNK_BASE) / THUNK_STRIDE,
    }
}
fn pointer<B: Backend>(machine: &mut Machine<B>, entry: u32, count: u32, size: u32) -> u32 {
    match calloc_crt(&mut env(machine, entry), count, size) {
        Outcome::Return(pointer) => pointer,
        _ => panic!("valid-sized allocation must return a pointer or NULL"),
    }
}

#[test]
fn calloc_crt_zeroing_zero_requests_reuse_and_budget_use_real_process_heap_ownership() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("CRT heap owner", PARK_ADDR, &[], 65536)
        .unwrap();
    let entry = machine.resolve_import(MODULE, "_calloc_crt").unwrap();
    let errno = floating::errno_address(&mut env(&mut machine, entry));
    errno.write(&mut machine.cpu, 79).unwrap();
    crate::util::set_last_error(&mut env(&mut machine, entry), 0x12345678);
    let heap = machine.ext.get::<Heaps>().process_heap;
    let mut live = Vec::new();
    for (count, size, length) in [(256, 2, 512), (0, 2, 0), (2, 0, 0), (0, u32::MAX, 0)] {
        let address = pointer(&mut machine, entry, count, size);
        assert_ne!(address, 0);
        assert!(
            !live.contains(&address),
            "simultaneous blocks must be distinct"
        );
        assert_eq!(machine.ext.get::<Heaps>().owner(address), Some(heap));
        assert_eq!(address & 7, 0); // x86 process-heap alignment.
        let mut bytes = vec![0x77; length];
        machine.cpu.read(address, &mut bytes).unwrap();
        assert!(bytes.iter().all(|byte| *byte == 0));
        assert_eq!(errno.read(&machine.cpu).unwrap(), 79);
        assert_eq!(
            crate::util::last_error(&env(&mut machine, entry)),
            0x12345678
        );
        live.push(address);
    }
    machine.cpu.write(live[0], &[0xff; 512]).unwrap();
    free(&mut env(&mut machine, entry), GuestPtr::new(live.remove(0)));
    let recycled = pointer(&mut machine, entry, 256, 2);
    let mut bytes = [0x77; 512];
    machine.cpu.read(recycled, &mut bytes).unwrap();
    assert_eq!(bytes, [0; 512]);
    live.push(recycled);
    assert_eq!(pointer(&mut machine, entry, 1, CRT_ALLOCATION_LIMIT + 1), 0);
    assert_eq!(errno.read(&machine.cpu).unwrap(), 79);
    assert_eq!(crate::util::last_error(&env(&mut machine, entry)), 8);
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, reason), count) = diagnostics.warnings.iter().next().unwrap();
    assert_eq!(api, "msvcr90.dll!_calloc_crt");
    assert!(reason.contains("virtual process-heap limit"));
    assert_eq!(*count, 1);
    // Caller-owned blocks outlive the allocating thread's CRT errno storage.
    super::super::detach_thread(&mut env(&mut machine, entry)).unwrap();
    for address in live {
        assert_eq!(machine.ext.get::<Heaps>().owner(address), Some(heap));
        free(&mut env(&mut machine, entry), GuestPtr::new(address));
        assert!(machine.ext.get::<Heaps>().owner(address).is_none());
    }
}

fn guest_calloc<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let calloc = machine.resolve_import(MODULE, "_calloc_crt").unwrap();
    let release = machine.resolve_import(MODULE, "free").unwrap();
    let errno = machine.resolve_import(MODULE, "_errno").unwrap();
    let other_errno = machine.resolve_import("ucrtbase.dll", "_errno").unwrap();
    let setter = machine
        .resolve_import(MODULE, "_set_invalid_parameter_handler")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut callback = Asm::new(CALLBACK);
    callback.bytes.extend_from_slice(&[0xff, 0x05]);
    callback.bytes.extend_from_slice(&(DATA + 40).to_le_bytes());
    callback
        .mov_eax_mem(DATA + 4)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 44);
    callback.ret();
    machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
    let mut code = Asm::new(CODE);
    code.call(errno).mov_mem_eax(DATA + 4);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
    code.call(other_errno).mov_mem_eax(DATA + 8);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 71, 0, 0, 0]);
    code.push_imm(0x12345678).call(set_error);
    // Actual game ABI and request: count256,size2, cdecl caller removes8bytes.
    code.push_imm(2)
        .push_imm(256)
        .call(calloc)
        .add_esp(8)
        .mov_mem_eax(DATA);
    code.bytes
        .extend_from_slice(&[0x89, 0xc6, 0xb9, 128, 0, 0, 0, 0x31, 0xd2]);
    // OR all128DWORDs to prove the entire512-byte allocation was zeroed.
    code.bytes
        .extend_from_slice(&[0x0b, 0x16, 0x83, 0xc6, 4, 0x49, 0x75, 0xf8]);
    code.bytes.extend_from_slice(&[0x89, 0x15]);
    code.bytes.extend_from_slice(&(DATA + 32).to_le_bytes());
    code.mov_eax_mem(DATA + 4)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 12);
    code.call(get_error).mov_mem_eax(DATA + 36);
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.call(release).add_esp(4);
    code.push_imm(2)
        .push_imm(0)
        .call(calloc)
        .add_esp(8)
        .mov_mem_eax(DATA + 16);
    code.bytes.extend_from_slice(&[0x50]);
    code.call(release).add_esp(4);
    code.push_imm(CALLBACK).call(setter).add_esp(4);
    code.push_imm(2)
        .push_imm(u32::MAX)
        .call(calloc)
        .add_esp(8)
        .mov_mem_eax(DATA + 20);
    code.mov_eax_mem(DATA + 4)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 24);
    code.mov_eax_mem(DATA + 8)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 28);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("_calloc_crt guest ABI", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let pointer = machine.cpu.read_u32(DATA).unwrap();
    let zero_pointer = machine.cpu.read_u32(DATA + 16).unwrap();
    assert_ne!(pointer, 0);
    assert_ne!(zero_pointer, 0);
    assert!(machine.ext.get::<Heaps>().owner(pointer).is_none());
    assert!(machine.ext.get::<Heaps>().owner(zero_pointer).is_none());
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 79);
    assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 24).unwrap(), 12);
    assert_eq!(machine.cpu.read_u32(DATA + 28).unwrap(), 71);
    assert_eq!(machine.cpu.read_u32(DATA + 32).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 36).unwrap(), 0x12345678);
    assert_eq!(machine.cpu.read_u32(DATA + 40).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 44).unwrap(), 12);
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}
#[test]
fn calloc_crt_actual_cdecl_request_overflow_callback_and_free_run_in_interpreter_and_jit() {
    guest_calloc(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_calloc(JitCpu::new(PARK_ADDR, false).unwrap());
}
