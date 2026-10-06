//! Authored cdecl calls store the real x87 ST0 return. Golden decimal boundaries
//! were checked with the installed x86 VC80 oracle under target/strtod-native.
use adv_abi::{AbiKind, Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const SOURCE: u32 = DATA + 0x2000;
const ERRNO: u32 = DATA + 0x1800;
const TAIL: u32 = 0x430000;
const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    for module in MODULES {
        let api = registry.get(module, "strtod").unwrap();
        assert_eq!(api.conv, Conv::Cdecl { argc: 2 });
        assert!(registry.dynamic_available(module, "strtod"));
        assert_eq!(
            adv_abi::signature::lookup(module, "strtod")
                .unwrap()
                .return_kind,
            AbiKind::F64
        );
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-strtod-tests");
    adv_win32::Win32::install(
        &mut machine,
        adv_win32::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    machine.cpu.map(CODE, 8192, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    machine.cpu.map(TAIL, 4096, Prot::RW).unwrap();
    machine
}
fn call(code: &mut Asm, api: u32, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(api);
    if !args.is_empty() {
        code.add_esp((args.len() * 4) as u8);
    }
}
fn fstp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0xdd, 0x1d]); // fstp qword ptr [absolute]
    code.bytes.extend_from_slice(&output.to_le_bytes());
}
fn esp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}
fn seed_errno(code: &mut Asm, api: u32) {
    code.call(api).mov_mem_eax(ERRNO);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
}
fn finish<B: Backend>(machine: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("strtod authored guest", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}
fn bits<B: Backend>(machine: &Machine<B>, at: u32) -> u64 {
    let mut bytes = [0; 8];
    machine.cpu.read(at, &mut bytes).unwrap();
    u64::from_le_bytes(bytes)
}

fn decimal<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let mut code = Asm::new(CODE);
    let mut golden = Vec::new();
    let cases: [(&[u8], u64, u32, u32); 14] = [
        (b" \t-12.5e+2tail\0", (-1250.0f64).to_bits(), 10, 79),
        (b"1e+\0", 1.0f64.to_bits(), 1, 79),
        (b"  +.\0", 0, 0, 79),
        (b".25tail\0", 0.25f64.to_bits(), 3, 79),
        (b"0x1.8p2!\0", 0, 1, 79),
        (b"inf\0", 0, 0, 79),
        (b"nan\0", 0, 0, 79),
        (b"1.#INF\0", 1.0f64.to_bits(), 2, 79),
        (b"1e309\0", f64::INFINITY.to_bits(), 5, 34),
        (b"-1e309\0", f64::NEG_INFINITY.to_bits(), 6, 34),
        (b"1e-400\0", 0, 6, 34),
        (b"5e-324\0", 1, 6, 79),
        (b"0e-9999\0", 0, 7, 79),
        (b"-0\0", (-0.0f64).to_bits(), 2, 79),
    ];
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    code.push_imm(0x12345678).call(set_error);
    let mut source = SOURCE;
    for module in MODULES {
        let api = m.resolve_import(module, "strtod").unwrap();
        let errno = m.resolve_import(module, "_errno").unwrap();
        for (text, expected, count, error) in cases {
            m.cpu.write(source, text).unwrap();
            let output = DATA + golden.len() as u32 * 32;
            seed_errno(&mut code, errno);
            esp(&mut code, output + 20);
            call(&mut code, api, &[source, output + 8]);
            esp(&mut code, output + 24);
            fstp(&mut code, output);
            code.mov_eax_mem(ERRNO)
                .mov_eax_ptr_eax()
                .mov_mem_eax(output + 12);
            code.call(get_error).mov_mem_eax(output + 16);
            golden.push((output, expected, source + count, error));
            source += text.len() as u32;
        }
    }
    // NUMERIC and CTYPE are separate CRT categories; neither follows Win32 ACP.
    let setlocale = m.resolve_import("msvcr90.dll", "setlocale").unwrap();
    let locale = source;
    m.cpu
        .write(locale, b"Japanese_Japan.932\0English_United States.1252\0")
        .unwrap();
    call(&mut code, setlocale, &[4, locale]);
    call(&mut code, setlocale, &[2, locale + 19]);
    let text = locale + 64;
    m.cpu.write(text, b"\xa01.5tail\0").unwrap();
    let output = DATA + golden.len() as u32 * 32;
    seed_errno(
        &mut code,
        m.resolve_import("msvcr90.dll", "_errno").unwrap(),
    );
    esp(&mut code, output + 20);
    call(
        &mut code,
        m.resolve_import("msvcr90.dll", "strtod").unwrap(),
        &[text, output + 8],
    );
    esp(&mut code, output + 24);
    fstp(&mut code, output);
    code.mov_eax_mem(ERRNO)
        .mov_eax_ptr_eax()
        .mov_mem_eax(output + 12);
    code.call(get_error).mov_mem_eax(output + 16);
    golden.push((output, 1.5f64.to_bits(), text + 4, 79));
    let result = finish(&mut m, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (output, expected, end, errno) in golden {
        assert_eq!(bits(&m, output), expected, "ST0 bits at {output:#x}");
        assert_eq!(m.cpu.read_u32(output + 8).unwrap(), end);
        assert_eq!(m.cpu.read_u32(output + 12).unwrap(), errno);
        assert_eq!(m.cpu.read_u32(output + 16).unwrap(), 0x12345678);
        assert_eq!(
            m.cpu.read_u32(output + 20).unwrap(),
            m.cpu.read_u32(output + 24).unwrap()
        );
    }
    assert_eq!(m.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
fn decimal_prefix_end_errno_locale_and_three_cdecl_aliases_return_binary64_in_st0() {
    decimal(UnicornCpu::new(PARK_ADDR).unwrap());
    decimal(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn guarded<B: Backend>(mut cpu: impl FnMut() -> B) {
    let mut m = machine(cpu());
    let api = m.resolve_import("msvcr80.dll", "strtod").unwrap();
    let errno = m.resolve_import("msvcr80.dll", "_errno").unwrap();
    let setter = m
        .resolve_import("msvcr80.dll", "_set_invalid_parameter_handler")
        .unwrap();
    let handler = CODE + 4096;
    let mut callback = Asm::new(handler);
    callback
        .call(errno)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 64);
    callback.mov_eax_mem(DATA + 8).mov_mem_eax(DATA + 68);
    callback.ret();
    m.cpu.write(handler, &callback.bytes).unwrap();
    m.cpu.write_u32(DATA + 8, 0xcccccccc).unwrap();
    m.cpu.write(TAIL + 4094, b"1x").unwrap(); // Invalid future page must not be touched.
    let mut code = Asm::new(CODE);
    call(&mut code, setter, &[handler]);
    seed_errno(&mut code, errno);
    call(&mut code, api, &[0, DATA + 8]);
    fstp(&mut code, DATA);
    seed_errno(&mut code, errno);
    call(&mut code, api, &[TAIL + 4094, DATA + 16]);
    fstp(&mut code, DATA + 24);
    assert!(matches!(finish(&mut m, code), RunResult::Exited(0)));
    assert_eq!(bits(&m, DATA), 0);
    assert_eq!(m.cpu.read_u32(DATA + 64).unwrap(), 22);
    assert_eq!(m.cpu.read_u32(DATA + 68).unwrap(), 0);
    assert_eq!(bits(&m, DATA + 24), 1.0f64.to_bits());
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), TAIL + 4095);

    // Failed output/input checks do not publish an end pointer or consume ST0.
    for (source, output, text, reason) in [
        (TAIL + 4094, TAIL + 4092, b"1x", "readonly end pointer"),
        (
            TAIL + 4094,
            DATA + 8,
            b"1e",
            "incomplete exponent crosses unmapped page",
        ),
    ] {
        let mut m = machine(cpu());
        let api = m.resolve_import("msvcr80.dll", "strtod").unwrap();
        m.cpu.write_u32(DATA + 8, 0xcccccccc).unwrap();
        m.cpu.write(TAIL + 4094, text).unwrap();
        let before = m.cpu.read_u32(output).unwrap();
        if output >= TAIL {
            m.cpu.protect(TAIL, 4096, Prot::READ).unwrap();
        }
        let mut code = Asm::new(CODE);
        call(&mut code, api, &[source, output]);
        let result = finish(&mut m, code);
        assert!(
            matches!(result, RunResult::Failed(_)),
            "{reason}: {result:?}"
        );
        assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 0xcccccccc);
        assert_eq!(m.cpu.read_u32(output).unwrap(), before);
        if output >= TAIL {
            m.cpu.protect(TAIL, 4096, Prot::RW).unwrap();
        }
    }
    for non_nearest in [false, true] {
        let mut m = machine(cpu());
        let api = m.resolve_import("msvcr80.dll", "strtod").unwrap();
        let errno = m.resolve_import("msvcr80.dll", "_errno").unwrap();
        let mut code = Asm::new(CODE);
        seed_errno(&mut code, errno);
        if non_nearest {
            m.cpu.write(SOURCE, b"1.5\0").unwrap();
            let control = m.resolve_import("msvcr80.dll", "_controlfp").unwrap();
            call(&mut code, control, &[0x100, 0x300]);
        } else {
            m.cpu
                .write(SOURCE, &[vec![b'1'; 4096], vec![0]].concat())
                .unwrap();
        }
        call(&mut code, api, &[SOURCE, DATA + 8]);
        fstp(&mut code, DATA);
        assert!(matches!(finish(&mut m, code), RunResult::Exited(0)));
        assert_eq!(bits(&m, DATA), 0);
        assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), SOURCE);
        let address = m.cpu.read_u32(ERRNO).unwrap();
        assert_eq!(m.cpu.read_u32(address).unwrap(), 40);
        assert!(
            m.ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .keys()
                .any(|(api, _)| api == "msvcr80.dll!strtod")
        );
    }
}

#[test]
fn null_callback_and_page_output_guards_keep_end_pointer_publication_atomic() {
    guarded(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guarded(|| JitCpu::new(PARK_ADDR, false).unwrap());
}
