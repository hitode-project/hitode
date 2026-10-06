//! Authored shell activation requests: no target or native process is executed.
use adv_abi::{ApiDiagnostics, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = DATA + 4096;
const TEXT: u32 = DATA + 0x700;
const RESULTS: u32 = DATA + 0x800;
const STACK_BEFORE: u32 = DATA + 0xa00;
const STACK_AFTER: u32 = STACK_BEFORE + 4;
const APIS: [&str; 4] = [
    "ShellExecuteA",
    "ShellExecuteW",
    "ShellExecuteExA",
    "ShellExecuteExW",
];

fn registry() -> Registry {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry
}

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut m = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/shell-probe",
            "shell-probe.exe",
            "/nonexistent/shell-probe-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    m.cpu
        .write(TEXT, b"https://example.invalid/fixture\0")
        .unwrap();
    let wide: Vec<_> = "補助.exe"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(TEXT + 64, &wide).unwrap();
    m
}

fn info(mask: u32, file: u32) -> Vec<u8> {
    // SDK x86 SHELLEXECUTEINFOA/W: size 60, hInstApp +32, hProcess +56.
    let mut bytes = vec![0; 60];
    for (offset, value) in [
        (0, 60),
        (4, mask),
        (16, file),
        (28, 1),
        (32, 0xfeed_face),
        (40, 0xdead_beef),
        (56, 0xbeef_cafe),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn call<B: Backend>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[u32], index: u32) {
    code.push_imm(0x9876_5432)
        .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(m.resolve_import("shell32.dll", name).unwrap())
        .mov_mem_eax(RESULTS + index * 8);
    code.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
        .mov_mem_eax(RESULTS + index * 8 + 4);
}

fn start() -> Asm {
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(STACK_BEFORE);
    code
}

fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(STACK_AFTER);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.start_main(CODE, CODE, 65536, "shell-probe.exe").unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        m.sys.contexts.len(),
        1,
        "shell requests must not create a guest process/thread"
    );
    assert_eq!(
        m.cpu.read_u32(STACK_BEFORE).unwrap(),
        m.cpu.read_u32(STACK_AFTER).unwrap()
    );
    assert_eq!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .map_or(0, |d| d.stub_calls),
        0
    );
}

fn result<B: Backend>(m: &Machine<B>, index: u32) -> (u32, u32) {
    (
        m.cpu.read_u32(RESULTS + index * 8).unwrap(),
        m.cpu.read_u32(RESULTS + index * 8 + 4).unwrap(),
    )
}

fn bytes<B: Backend>(m: &Machine<B>, address: u32, length: usize) -> Vec<u8> {
    let mut data = vec![0; length];
    m.cpu.read(address, &mut data).unwrap();
    data
}

fn denied<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let mut code = start();
    call(
        &mut m,
        &mut code,
        "ShellExecuteA",
        &[0, 0, TEXT, 0, 0, 1],
        0,
    );
    call(
        &mut m,
        &mut code,
        "ShellExecuteW",
        &[0, 0, TEXT + 64, 0, 0, 1],
        1,
    );
    for (index, (name, text, mask)) in [
        ("ShellExecuteExA", TEXT, 0),
        ("ShellExecuteExA", TEXT, 0x40 | 0x400),
        ("ShellExecuteExW", TEXT + 64, 0),
        ("ShellExecuteExW", TEXT + 64, 0x40 | 0x400),
    ]
    .into_iter()
    .enumerate()
    {
        let pointer = DATA + 256 + index as u32 * 64;
        m.cpu.write(pointer, &info(mask, text)).unwrap();
        call(&mut m, &mut code, name, &[pointer], 2 + index as u32);
    }
    run(&mut m, code);
    for index in 0..2 {
        assert_eq!(result(&m, index), (5, 5));
    }
    for index in 0..4 {
        assert_eq!(result(&m, index + 2), (0, 5));
        let mask = if index % 2 == 0 { 0 } else { 0x40 | 0x400 };
        let mut expected = info(mask, if index < 2 { TEXT } else { TEXT + 64 });
        expected[32..36].copy_from_slice(&5u32.to_le_bytes());
        expected[56..60].fill(0);
        assert_eq!(bytes(&m, DATA + 256 + index * 64, 60), expected);
    }
    let warnings = &m.ext.get::<ApiDiagnostics>().warnings;
    for api in APIS {
        assert!(warnings.keys().any(|(name, _)| name == api));
    }
}

fn bad_structures<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let mut code = start();
    let mut wrong_size = info(0x40, TEXT);
    wrong_size[0..4].copy_from_slice(&59u32.to_le_bytes());
    m.cpu.write(DATA + 256, &wrong_size).unwrap();
    let valid = info(0x40, TEXT);
    m.cpu.write(READ_ONLY + 256, &valid).unwrap();
    // hInstApp is writable but hProcess crosses into a readonly page.
    let split = READ_ONLY - 40;
    m.cpu.write(split, &valid).unwrap();
    for (index, pointer) in [0, u32::MAX, DATA + 256, READ_ONLY + 256, split]
        .into_iter()
        .enumerate()
    {
        call(
            &mut m,
            &mut code,
            "ShellExecuteExA",
            &[pointer],
            index as u32,
        );
    }
    run(&mut m, code);
    for (index, error) in [87, 998, 87, 998, 998].into_iter().enumerate() {
        assert_eq!(result(&m, index as u32), (0, error));
    }
    assert_eq!(bytes(&m, DATA + 256, 60), wrong_size);
    assert_eq!(bytes(&m, READ_ONLY + 256, 60), valid);
    assert_eq!(bytes(&m, split, 60), valid);
}

fn bad_strings<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let mut code = start();
    m.cpu.write(READ_ONLY + 4094, &[0x41, 0]).unwrap();
    let cases = [
        ("ShellExecuteExA", 16, 0, 87),
        ("ShellExecuteExA", 16, 0xdead_beef, 998),
        ("ShellExecuteExA", 12, 0xdead_beef, 998),
        ("ShellExecuteExW", 16, READ_ONLY + 4094, 998),
    ];
    for (index, &(name, offset, pointer, _)) in cases.iter().enumerate() {
        let output = DATA + 256 + index as u32 * 64;
        let mut input = info(0x40, TEXT);
        input[offset..offset + 4].copy_from_slice(&pointer.to_le_bytes());
        m.cpu.write(output, &input).unwrap();
        call(&mut m, &mut code, name, &[output], index as u32);
    }
    call(
        &mut m,
        &mut code,
        "ShellExecuteA",
        &[0, 0, 0xdead_beef, 0, 0, 1],
        4,
    );
    run(&mut m, code);
    for (index, &(_, _, _, error)) in cases.iter().enumerate() {
        assert_eq!(result(&m, index as u32), (0, error));
        let output = DATA + 256 + index as u32 * 64;
        assert_eq!(m.cpu.read_u32(output + 32).unwrap(), 5);
        assert_eq!(m.cpu.read_u32(output + 56).unwrap(), 0);
    }
    assert_eq!(result(&m, 4), (5, 998));
}

#[test]
fn shell_exports_are_typed_safe_failures_with_real_dynamic_availability() {
    let registry = registry();
    assert!(registry.signature_conflicts().is_empty());
    for api in APIS {
        assert_eq!(
            registry.level("shell32.dll", api),
            Some(ImplementationLevel::SafeFail)
        );
        assert!(registry.dynamic_available("shell32.dll", api));
    }
}

#[test]
fn valid_ansi_wide_url_and_file_requests_fail_without_launch_or_fabricated_handles() {
    denied(JitCpu::new(PARK_ADDR, false).unwrap());
    denied(JitCpu::new(PARK_ADDR, true).unwrap());
    denied(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn structure_size_and_output_permissions_are_validated_before_writing() {
    bad_structures(JitCpu::new(PARK_ADDR, false).unwrap());
    bad_structures(JitCpu::new(PARK_ADDR, true).unwrap());
    bad_structures(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn invalid_required_optional_or_unterminated_strings_have_explicit_failure_outputs() {
    bad_strings(JitCpu::new(PARK_ADDR, false).unwrap());
    bad_strings(JitCpu::new(PARK_ADDR, true).unwrap());
    bad_strings(UnicornCpu::new(PARK_ADDR).unwrap());
}
