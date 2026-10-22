//! Extended creators share the existing Win32 and native waitable objects.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HANDLE: u32 = DATA + 0x800;
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-sync-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write(DATA + 0x900, b"AuthoredSharedEvent\0").unwrap();
    m.cpu
        .write(
            DATA + 0xa00,
            &"AuthoredSharedEvent\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    let create = m.resolve_import("kernel32.dll", "CreateEventExW").unwrap();
    for a in [0x1f0003, 3, DATA + 0xa00, 0] {
        code.push_imm(a);
    }
    code.call(create).mov_mem_eax(HANDLE);
    let mut call = |module: &str, name: &str, args: &[u32], value: u32, handle: bool| {
        for &a in args.iter().rev() {
            code.push_imm(a);
        }
        if handle {
            code.mov_eax_mem(HANDLE).push_eax();
        }
        code.call(m.resolve_import(module, name).unwrap())
            .mov_mem_eax(DATA + expected.len() as u32 * 4);
        expected.push(value);
    };
    call("kernel32.dll", "WaitForSingleObject", &[0], 0, true);
    call("kernel32.dll", "WaitForSingleObject", &[0], 0, true);
    call("ntdll.dll", "NtResetEvent", &[DATA + 0x880], 0, true);
    call("kernel32.dll", "WaitForSingleObject", &[0], 258, true);
    call(
        "kernel32.dll",
        "OpenEventA",
        &[0x1f0003, 0, DATA + 0x900],
        0,
        false,
    );
    // The opened alias is observed separately below; it is a real nonzero handle.
    expected.pop();
    code.mov_mem_eax(HANDLE + 4);
    let close = m.resolve_import("kernel32.dll", "CloseHandle").unwrap();
    code.mov_eax_mem(HANDLE).push_eax().call(close);
    code.mov_eax_mem(HANDLE + 4)
        .push_eax()
        .call(close)
        .mov_mem_eax(DATA + 0x700);
    let create = m
        .resolve_import("kernel32.dll", "CreateSemaphoreExW")
        .unwrap();
    for a in [0x1f0003, 0, 0, 2, 1, 0] {
        code.push_imm(a);
    }
    code.call(create).mov_mem_eax(HANDLE + 8);
    for (name, args, value) in [
        ("WaitForSingleObject", vec![0], 0),
        ("ReleaseSemaphore", vec![2, DATA + 0x884], 1),
        ("WaitForSingleObject", vec![0], 0),
        ("WaitForSingleObject", vec![0], 0),
        ("WaitForSingleObject", vec![0], 258),
        ("CloseHandle", vec![], 1),
    ] {
        for a in args.iter().rev() {
            code.push_imm(*a);
        }
        code.mov_eax_mem(HANDLE + 8)
            .push_eax()
            .call(m.resolve_import("kernel32.dll", name).unwrap())
            .mov_mem_eax(DATA + expected.len() as u32 * 4);
        expected.push(value);
    }
    for (name, args) in [
        ("CreateEventExW", vec![0, 0, 4, 0x1f0003]),
        ("CreateSemaphoreExW", vec![0, 0, 2, 0, 1, 0x1f0003]),
    ] {
        for a in args.iter().rev() {
            code.push_imm(*a);
        }
        code.call(m.resolve_import("kernel32.dll", name).unwrap())
            .mov_mem_eax(DATA + expected.len() as u32 * 4);
        expected.push(0);
    }
    code.mov_mem_imm(DATA + 0x704, 0)
        .mov_eax_mem(DATA + 0x704)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("extended synchronization", CODE, &[], 65536)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, value) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + i as u32 * 4).unwrap(),
            value,
            "row {i}"
        );
    }
    assert_ne!(m.cpu.read_u32(HANDLE).unwrap(), 0);
    assert_ne!(m.cpu.read_u32(HANDLE + 4).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 0x700).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(DATA + 0x880).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(DATA + 0x884).unwrap(), 0);
}
#[test]
// spec: SYS-SYNC-2
fn extended_creators_share_legacy_and_native_object_state_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}
