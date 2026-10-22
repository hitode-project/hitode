//! Counted ACP input, exact capacity failures and process-heap ownership.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
fn descriptor<B: Backend>(m: &mut Machine<B>, at: u32, length: u16, maximum: u16, buffer: u32) {
    m.cpu.write(at, &length.to_le_bytes()).unwrap();
    m.cpu.write(at + 2, &maximum.to_le_bytes()).unwrap();
    m.cpu.write_u32(at + 4, buffer).unwrap();
}
fn copy_and_free(code: &mut Asm, descriptor: u32, output: u32, length: u32, free: u32) {
    code.bytes.extend_from_slice(&[0x8b, 0x35]);
    code.bytes
        .extend_from_slice(&(descriptor + 4).to_le_bytes());
    code.bytes.push(0xbf);
    code.bytes.extend_from_slice(&output.to_le_bytes());
    code.mov_ecx_imm(length);
    code.bytes.extend_from_slice(&[0xfc, 0xf3, 0xa4]);
    code.push_imm(descriptor).call(free);
}
fn counted<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/rtl-strings",
            "fixture.exe",
            "/nonexistent/rtl-string-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    const SOURCE: u32 = DATA + 0x1000;
    const INPUT: u32 = DATA + 0x1100;
    m.cpu.write(INPUT, b"ab\0c").unwrap();
    descriptor(&mut m, SOURCE, 4, 64, INPUT);
    let convert = m
        .resolve_import("ntdll.dll", "RtlAnsiStringToUnicodeString")
        .unwrap();
    let free = m
        .resolve_import("ntdll.dll", "RtlFreeUnicodeString")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x12345678)
        .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
    for (index, capacity) in [0u16, 2, 4, 6, 8, 10].into_iter().enumerate() {
        let row = DATA + index as u32 * 128;
        descriptor(&mut m, row, 0x5555, capacity, row + 16);
        m.cpu.write(row + 16, &[0xcd; 32]).unwrap();
        code.push_imm(0)
            .push_imm(SOURCE)
            .push_imm(row)
            .call(convert)
            .mov_mem_eax(row + 8);
    }
    const ALLOC: u32 = DATA + 0x1200;
    const ALLOC_COPY: u32 = DATA + 0x1230;
    const ALLOC_POINTER: u32 = DATA + 0x1220;
    descriptor(&mut m, ALLOC, 0x5555, 0, 0xdeadbeef);
    m.cpu.write(ALLOC_COPY, &[0xcd; 32]).unwrap();
    code.push_imm(1)
        .push_imm(SOURCE)
        .push_imm(ALLOC)
        .call(convert)
        .mov_mem_eax(ALLOC + 8)
        .mov_eax_mem(ALLOC + 4)
        .mov_mem_eax(ALLOC_POINTER);
    copy_and_free(&mut code, ALLOC, ALLOC_COPY, 10, free);
    const JP_SOURCE: u32 = DATA + 0x1300;
    const JP_INPUT: u32 = DATA + 0x1320;
    const JP_DEST: u32 = DATA + 0x1340;
    const JP_COPY: u32 = DATA + 0x1360;
    m.cpu.write(JP_INPUT, &[0x82, 0xa0]).unwrap();
    descriptor(&mut m, JP_SOURCE, 2, 2, JP_INPUT);
    descriptor(&mut m, JP_DEST, 0x5555, 0, 0);
    code.push_imm(1)
        .push_imm(JP_SOURCE)
        .push_imm(JP_DEST)
        .call(convert)
        .mov_mem_eax(JP_DEST + 8);
    copy_and_free(&mut code, JP_DEST, JP_COPY, 4, free);
    const BIG_INPUT: u32 = 0x430000;
    const BIG_SOURCE: u32 = DATA + 0x1900;
    const BIG_DEST: u32 = DATA + 0x1920;
    m.cpu.map(BIG_INPUT, 65536, Prot::RW).unwrap();
    m.cpu.write(BIG_INPUT, &vec![b'a'; 32767]).unwrap();
    descriptor(&mut m, BIG_SOURCE, 32767, 32767, BIG_INPUT);
    descriptor(&mut m, BIG_DEST, 0x5555, 0, 0);
    code.push_imm(0)
        .push_imm(BIG_SOURCE)
        .push_imm(BIG_DEST)
        .call(convert)
        .mov_mem_eax(BIG_DEST + 8);
    code.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
        .mov_mem_eax(DATA + 0x1c00);
    code.mov_mem_imm(DATA + 0x1c04, 0)
        .mov_eax_mem(DATA + 0x1c04)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("counted strings", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let expected = [97, 0, 98, 0, 0, 0, 99, 0, 0, 0];
    for (index, capacity) in [0u16, 2, 4, 6, 8, 10].into_iter().enumerate() {
        let row = DATA + index as u32 * 128;
        let success = capacity == 10;
        assert_eq!(
            m.cpu.read_u32(row + 8).unwrap(),
            if success { 0 } else { 0x80000005 }
        );
        assert_eq!(
            m.cpu.read_u16(row).unwrap(),
            if success { 8 } else { 0x5555 }
        );
        assert_eq!(m.cpu.read_u16(row + 2).unwrap(), capacity);
        assert_eq!(m.cpu.read_u32(row + 4).unwrap(), row + 16);
        let mut actual = [0; 32];
        m.cpu.read(row + 16, &mut actual).unwrap();
        let mut bytes = [0xcd; 32];
        if success {
            bytes[..10].copy_from_slice(&expected);
        }
        assert_eq!(actual, bytes);
    }
    for desc in [ALLOC, JP_DEST] {
        assert_eq!(m.cpu.read_u32(desc).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(desc + 4).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(desc + 8).unwrap(), 0);
    }
    let pointer = m.cpu.read_u32(ALLOC_POINTER).unwrap();
    assert_ne!(pointer, 0);
    let heap = m.ext.get::<adv_os::Heaps>().process_heap;
    assert!(
        m.ext
            .get::<adv_os::Heaps>()
            .size(&m.cpu, heap, pointer)
            .is_none()
    );
    let mut actual = [0; 10];
    m.cpu.read(ALLOC_COPY, &mut actual).unwrap();
    assert_eq!(actual, expected);
    let mut japanese = [0; 4];
    m.cpu.read(JP_COPY, &mut japanese).unwrap();
    assert_eq!(japanese, [0x42, 0x30, 0, 0]);
    assert_eq!(m.cpu.read_u32(BIG_DEST + 8).unwrap(), 0xc00000f0);
    assert_eq!(m.cpu.read_u16(BIG_DEST).unwrap(), 0x5555);
    assert_eq!(m.cpu.read_u32(DATA + 0x1c00).unwrap(), 0x12345678);
}
#[test]
// spec: SYS-STR-1
fn counted_acp_conversion_capacity_and_heap_lifetime_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        counted(cpu);
    }
    counted(UnicornCpu::new(PARK_ADDR).unwrap());
}
