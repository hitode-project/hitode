use super::*;
use crate::{Win32, Win32Config};
use adv_abi::{ApiDiagnostics, RunMode};
use adv_cpu::{
    JitCpu,
    backend::{Backend, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HANDLE: u32 = DATA + 0x1600;
const COUNT: u32 = DATA + 0x1400;
const LENGTH: u32 = DATA + 0x1500;
const BUFFER: u32 = DATA + 0x1300;
const QUERY: u32 = DATA + 0x1700;
const MARKER: u32 = 0x12345678;
#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    L(u32),
}
use Arg::{I, L};
fn emit<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[Arg], index: usize) {
    let api = m.resolve_import("wininet.dll", name).unwrap();
    let set = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    code.push_imm(MARKER).call(set);
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            L(at) => {
                code.mov_eax_mem(*at).push_eax();
            }
        }
    }
    let row = DATA + index as u32 * 16;
    code.call(api)
        .mov_mem_eax(row)
        .call(get)
        .mov_mem_eax(row + 4)
        .mov_eax_mem(COUNT)
        .mov_mem_eax(row + 8);
}
fn offline<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(".", "fixture.exe", "target/internet-closure-tests"),
    )
    .unwrap();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    for (at, text) in [
        (DATA + 0x1000, "fixture"),
        (DATA + 0x1100, "example.invalid"),
        (DATA + 0x1200, "second"),
    ] {
        m.cpu
            .write(
                at,
                &text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    m.cpu.write(BUFFER, &[0xcd; 64]).unwrap();
    let mut code = Asm::new(CODE);
    let open = m.resolve_import("wininet.dll", "InternetOpenW").unwrap();
    code.push_imm(0)
        .push_imm(0)
        .push_imm(0)
        .push_imm(1)
        .push_imm(DATA + 0x1000)
        .call(open)
        .mov_mem_eax(HANDLE);
    let cases = [
        ("HttpAddRequestHeadersW", vec![I(0), I(0), I(0)], false, 0),
        ("HttpEndRequestW", vec![I(0), I(0), I(0)], false, 0),
        ("HttpOpenRequestW", vec![I(0); 7], false, 0),
        (
            "HttpQueryInfoW",
            vec![I(19), I(BUFFER), I(LENGTH), I(0)],
            false,
            0,
        ),
        ("HttpSendRequestExW", vec![I(0); 4], false, 0),
        ("HttpSendRequestW", vec![I(0); 4], false, 0),
        (
            "InternetQueryDataAvailable",
            vec![I(COUNT), I(0), I(0)],
            false,
            0,
        ),
        (
            "InternetSetFilePointer",
            vec![I(1), I(COUNT), I(0), I(0)],
            false,
            u32::MAX,
        ),
        ("InternetReadFile", vec![I(BUFFER), I(8), I(COUNT)], true, 0),
        (
            "InternetWriteFile",
            vec![I(BUFFER), I(8), I(COUNT)],
            true,
            0,
        ),
    ];
    let mut expected = Vec::new();
    for valid in [true, false] {
        for (name, tail, clears, result) in &cases {
            let mut args = vec![if valid { L(HANDLE) } else { I(0) }];
            args.extend(tail);
            code.mov_mem_imm(COUNT, MARKER).mov_mem_imm(LENGTH, 64);
            emit(&mut m, &mut code, name, &args, expected.len());
            expected.push((
                *result,
                Some(if valid { 12018 } else { 6 }),
                if *clears { 0 } else { MARKER },
            ));
        }
    }
    emit(
        &mut m,
        &mut code,
        "InternetConnectW",
        &[
            L(HANDLE),
            I(DATA + 0x1100),
            I(80),
            I(0),
            I(0),
            I(3),
            I(0),
            I(0),
        ],
        expected.len(),
    );
    expected.push((0, Some(12029), 0));
    for reserved in [0, 1] {
        emit(
            &mut m,
            &mut code,
            "InternetAttemptConnect",
            &[I(reserved)],
            expected.len(),
        );
        expected.push((if reserved == 0 { 12029 } else { 87 }, None, 0));
    }
    emit(
        &mut m,
        &mut code,
        "InternetErrorDlg",
        &[I(0), L(HANDLE), I(0), I(0), I(0)],
        expected.len(),
    );
    expected.push((12018, None, 0));
    emit(
        &mut m,
        &mut code,
        "InternetErrorDlg",
        &[I(0), I(0), I(0), I(0), I(0)],
        expected.len(),
    );
    expected.push((0, None, 0));
    code.mov_mem_imm(COUNT, 12345);
    emit(
        &mut m,
        &mut code,
        "InternetSetOptionW",
        &[L(HANDLE), I(2), I(COUNT), I(4)],
        expected.len(),
    );
    expected.push((1, None, 12345));
    code.mov_mem_imm(LENGTH, 4);
    emit(
        &mut m,
        &mut code,
        "InternetQueryOptionA",
        &[L(HANDLE), I(2), I(QUERY), I(LENGTH)],
        expected.len(),
    );
    expected.push((1, None, 12345));
    emit(
        &mut m,
        &mut code,
        "InternetSetOptionW",
        &[L(HANDLE), I(2), I(COUNT), I(3)],
        expected.len(),
    );
    expected.push((0, Some(87), 12345));
    emit(
        &mut m,
        &mut code,
        "InternetSetOptionW",
        &[L(HANDLE), I(41), I(DATA + 0x1200), I(14)],
        expected.len(),
    );
    expected.push((1, None, 12345));
    code.mov_mem_imm(LENGTH, 64);
    emit(
        &mut m,
        &mut code,
        "InternetQueryOptionA",
        &[L(HANDLE), I(41), I(QUERY + 16), I(LENGTH)],
        expected.len(),
    );
    expected.push((1, None, 12345));
    emit(
        &mut m,
        &mut code,
        "InternetCloseHandle",
        &[L(HANDLE)],
        expected.len(),
    );
    expected.push((1, None, 12345));
    emit(
        &mut m,
        &mut code,
        "HttpSendRequestW",
        &[L(HANDLE), I(0), I(0), I(0), I(0)],
        expected.len(),
    );
    expected.push((0, Some(6), 12345));
    code.mov_mem_imm(DATA + 0x1f00, 0)
        .mov_eax_mem(DATA + 0x1f00)
        .ret();
    assert!(code.bytes.len() < 8192);
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("offline requests", CODE, &[], 65536).unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_ne!(m.cpu.read_u32(HANDLE).unwrap(), 0);
    for (index, (result, error, count)) in expected.into_iter().enumerate() {
        let row = DATA + index as u32 * 16;
        assert_eq!(m.cpu.read_u32(row).unwrap(), result, "row {index}");
        if let Some(error) = error {
            assert_eq!(m.cpu.read_u32(row + 4).unwrap(), error, "row {index}");
        }
        assert_eq!(m.cpu.read_u32(row + 8).unwrap(), count, "row {index}");
    }
    let mut buffer = [0; 64];
    m.cpu.read(BUFFER, &mut buffer).unwrap();
    assert_eq!(buffer, [0xcd; 64]);
    assert_eq!(m.cpu.read_u32(QUERY).unwrap(), 12345);
    let mut agent = [0; 7];
    m.cpu.read(QUERY + 16, &mut agent).unwrap();
    assert_eq!(&agent, b"second\0");
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.acceptance_eligible(RunMode::Strict))
    );
}
#[test]
// spec: PERS-NET-6
fn offline_requests_validate_session_types_and_preserve_outputs_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        offline(cpu);
    }
    offline(UnicornCpu::new(PARK_ADDR).unwrap());
}
