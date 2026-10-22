use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, Registry, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TEXT: u32 = DATA + 0x800;
const ADDRESS: u32 = DATA + 0x900;
const OUTPUT: u32 = DATA + 0xa00;
const MARKER: u32 = 0x12345678;

fn conversions<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/winsock-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.write(TEXT, b"192.0.2.1\0").unwrap();
    m.cpu.write(TEXT + 32, b"127.1\0").unwrap();
    m.cpu.write(TEXT + 64, b"0x7f.1\0").unwrap();
    m.cpu.write(TEXT + 96, b"1.2.3.256\0").unwrap();
    m.cpu
        .write(
            TEXT + 128,
            &"2001:db8::1\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write(ADDRESS, &[0xcd; 48]).unwrap();
    m.cpu.write(OUTPUT, &[0xcd; 128]).unwrap();
    let set_error = m.resolve_import("ws2_32.dll", "WSASetLastError").unwrap();
    let get_error = m.resolve_import("wsock32.dll", "WSAGetLastError").unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    let mut call = |module, name, args: &[u32], value: u32, error: u32| {
        let function = m.resolve_import(module, name).unwrap();
        let row = DATA + expected.len() as u32 * 8;
        code.push_imm(MARKER).call(set_error);
        for &arg in args.iter().rev() {
            code.push_imm(arg);
        }
        code.call(function).mov_mem_eax(row);
        code.call(get_error).mov_mem_eax(row + 4);
        expected.push((value, error));
    };
    for module in ["ws2_32.dll", "wsock32.dll"] {
        call(module, "htonl", &[0x12345678], 0x78563412, MARKER);
        call(module, "ntohl", &[0x12345678], 0x78563412, MARKER);
        call(module, "htons", &[0xabcd1234], 0x3412, MARKER);
        call(module, "ntohs", &[0xabcd1234], 0x3412, MARKER);
        call(module, "inet_addr", &[TEXT], 0x010200c0, MARKER);
        call(module, "inet_addr", &[TEXT + 32], 0x0100007f, MARKER);
        call(module, "inet_addr", &[TEXT + 64], 0x0100007f, MARKER);
        call(module, "inet_addr", &[TEXT + 96], u32::MAX, MARKER);
    }
    call("ws2_32.dll", "inet_pton", &[2, TEXT, ADDRESS], 1, MARKER);
    call(
        "ws2_32.dll",
        "inet_pton",
        &[2, TEXT + 32, ADDRESS + 4],
        0,
        MARKER,
    );
    call(
        "ws2_32.dll",
        "InetPtonW",
        &[23, TEXT + 128, ADDRESS + 8],
        1,
        MARKER,
    );
    call(
        "ws2_32.dll",
        "inet_pton",
        &[99, TEXT, ADDRESS + 24],
        u32::MAX,
        10047,
    );
    call("ws2_32.dll", "inet_pton", &[2, TEXT, 0], u32::MAX, 10014);
    call(
        "ws2_32.dll",
        "inet_ntop",
        &[2, ADDRESS, OUTPUT, 16],
        OUTPUT,
        MARKER,
    );
    call(
        "ws2_32.dll",
        "InetNtopW",
        &[23, ADDRESS + 8, OUTPUT + 32, 40],
        OUTPUT + 32,
        MARKER,
    );
    call(
        "ws2_32.dll",
        "inet_ntop",
        &[2, ADDRESS, OUTPUT + 112, 2],
        0,
        10014,
    );
    call("ws2_32.dll", "closesocket", &[0], u32::MAX, 10093);
    call("wsock32.dll", "WSAStartup", &[0x202, DATA + 0xc00], 0, 0);
    for socket in [0, u32::MAX, 0x11000004] {
        call("ws2_32.dll", "closesocket", &[socket], u32::MAX, 10038);
    }
    call("ws2_32.dll", "WSACleanup", &[], 0, 0);
    call("ws2_32.dll", "closesocket", &[0], u32::MAX, 10093);
    code.mov_mem_imm(DATA + 0x700, 0)
        .mov_eax_mem(DATA + 0x700)
        .ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("local addresses", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, (value, error)) in expected.into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 8).unwrap(),
            value,
            "call {index}"
        );
        assert_eq!(
            m.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(),
            error,
            "error {index}"
        );
    }
    let mut actual = [0; 24];
    m.cpu.read(ADDRESS, &mut actual).unwrap();
    assert_eq!(&actual[..8], &[192, 0, 2, 1, 0xcd, 0xcd, 0xcd, 0xcd]);
    assert_eq!(
        &actual[8..24],
        &[0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
    );
    let mut ansi = [0; 10];
    m.cpu.read(OUTPUT, &mut ansi).unwrap();
    assert_eq!(&ansi, b"192.0.2.1\0");
    let expected: Vec<_> = "2001:db8::1\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let mut wide = vec![0; expected.len()];
    m.cpu.read(OUTPUT + 32, &mut wide).unwrap();
    assert_eq!(wide, expected);
    assert_eq!(m.cpu.read_u32(OUTPUT + 112).unwrap(), 0xcdcdcdcd);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|diag| diag.acceptance_eligible(RunMode::Strict))
    );
}

#[test]
// spec: SYS-NET-2
fn local_address_operations_share_legacy_state_on_all_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        conversions(cpu);
    }
    conversions(UnicornCpu::new(PARK_ADDR).unwrap());
}
