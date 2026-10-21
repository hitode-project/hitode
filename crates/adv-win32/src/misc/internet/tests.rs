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
const AGENT_A: u32 = DATA + 0x800;
const AGENT_W: u32 = DATA + 0x900;
const URL: u32 = DATA + 0xa00;
const READ_ONLY: u32 = 0x420000;

fn sessions<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/internet-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu.write_u32(READ_ONLY, 64).unwrap();
    m.cpu.write(AGENT_A, b"fixture\0").unwrap();
    m.cpu
        .write(
            AGENT_W,
            &"fixture\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write(URL, b"https://example.invalid/\0").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let mut code = Asm::new(CODE);
    for (index, (dll, open, query, close, agent, flags, other_dll, other_close)) in [
        (
            "wininet.dll",
            "InternetOpenA",
            "InternetQueryOptionA",
            "InternetCloseHandle",
            AGENT_A,
            0,
            "winhttp.dll",
            "WinHttpCloseHandle",
        ),
        (
            "wininet.dll",
            "InternetOpenW",
            "InternetQueryOptionW",
            "InternetCloseHandle",
            AGENT_W,
            0,
            "winhttp.dll",
            "WinHttpCloseHandle",
        ),
        (
            "winhttp.dll",
            "WinHttpOpen",
            "WinHttpQueryOption",
            "WinHttpCloseHandle",
            AGENT_W,
            0,
            "wininet.dll",
            "InternetCloseHandle",
        ),
        (
            "wininet.dll",
            "InternetOpenA",
            "InternetQueryOptionA",
            "InternetCloseHandle",
            AGENT_A,
            0x01000000,
            "winhttp.dll",
            "WinHttpCloseHandle",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let row = DATA + index as u32 * 0x100;
        let open = m.resolve_import(dll, open).unwrap();
        let query = m.resolve_import(dll, query).unwrap();
        let close = m.resolve_import(dll, close).unwrap();
        let wrong = m.resolve_import(other_dll, other_close).unwrap();
        code.push_imm(flags)
            .push_imm(0)
            .push_imm(0)
            .push_imm(1)
            .push_imm(agent)
            .call(open)
            .mov_mem_eax(row);
        // Querying the required size does not need an output buffer.
        code.mov_mem_imm(row + 12, 0)
            .push_imm(row + 12)
            .push_imm(0)
            .push_imm(41);
        code.mov_eax_mem(row)
            .push_eax()
            .call(query)
            .mov_mem_eax(row + 4);
        code.call(get_error).mov_mem_eax(row + 8);
        code.push_imm(row + 12).push_imm(row + 0x50).push_imm(41);
        code.mov_eax_mem(row)
            .push_eax()
            .call(query)
            .mov_mem_eax(row + 16);
        // A readable but immutable length pointer must not publish buffer data.
        code.mov_mem_imm(row + 0x70, 0xcdcdcdcd);
        code.push_imm(READ_ONLY).push_imm(row + 0x70).push_imm(41);
        code.mov_eax_mem(row)
            .push_eax()
            .call(query)
            .mov_mem_eax(row + 52);
        code.call(get_error).mov_mem_eax(row + 56);
        // Closing through the other Internet library cannot consume this owner.
        code.mov_eax_mem(row)
            .push_eax()
            .call(wrong)
            .mov_mem_eax(row + 36);
        code.call(get_error).mov_mem_eax(row + 40);
        if dll == "wininet.dll" {
            let url = m.resolve_import(dll, "InternetOpenUrlA").unwrap();
            code.push_imm(0)
                .push_imm(0)
                .push_imm(0)
                .push_imm(0)
                .push_imm(URL);
            code.mov_eax_mem(row)
                .push_eax()
                .call(url)
                .mov_mem_eax(row + 44);
            code.call(get_error).mov_mem_eax(row + 48);
        }
        code.mov_eax_mem(row)
            .push_eax()
            .call(close)
            .mov_mem_eax(row + 24);
        code.mov_eax_mem(row)
            .push_eax()
            .call(close)
            .mov_mem_eax(row + 28);
        code.call(get_error).mov_mem_eax(row + 32);
    }
    let connected = m
        .resolve_import("wininet.dll", "InternetGetConnectedState")
        .unwrap();
    code.mov_mem_imm(DATA + 0x400, 0xffffffff);
    code.push_imm(0)
        .push_imm(DATA + 0x400)
        .call(connected)
        .mov_mem_eax(DATA + 0x404);
    code.mov_eax_mem(DATA + 0x404).ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("Internet sessions", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for index in 0..4 {
        let row = DATA + index * 0x100;
        assert_ne!(m.cpu.read_u32(row).unwrap(), 0);
        for (offset, value) in [
            (4, 0),
            (8, 122),
            (12, if matches!(index, 0 | 3) { 8 } else { 16 }),
            (16, 1),
            (24, 1),
            (28, 0),
            (32, 6),
            (36, 0),
            (40, 6),
            (52, 0),
            (56, 998),
            (0x70, 0xcdcdcdcd),
        ] {
            assert_eq!(
                m.cpu.read_u32(row + offset).unwrap(),
                value,
                "row {index}, offset {offset}"
            );
        }
        if index != 2 {
            assert_eq!(m.cpu.read_u32(row + 44).unwrap(), 0);
            assert_eq!(
                m.cpu.read_u32(row + 48).unwrap(),
                if index == 3 { 2 } else { 12029 }
            );
        }
        let expected = if matches!(index, 0 | 3) {
            b"fixture\0".to_vec()
        } else {
            "fixture\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect()
        };
        let mut actual = vec![0; expected.len()];
        m.cpu.read(row + 0x50, &mut actual).unwrap();
        assert_eq!(actual, expected);
    }
    assert_eq!(m.cpu.read_u32(DATA + 0x400).unwrap(), 0);
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|diag| diag.acceptance_eligible(RunMode::Strict))
    );
}

#[test]
// spec: PERS-NET-1
fn offline_library_initialization_keeps_real_session_ownership() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        sessions(cpu);
    }
    sessions(UnicornCpu::new(PARK_ADDR).unwrap());
}
