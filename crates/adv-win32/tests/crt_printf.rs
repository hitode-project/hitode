//! Authored x86 nonsecure buffer fronts, over the same checked CRT formatter.
//! Native VC80 NULL/success evidence: target/crt-printf-native. Disabled %n and
//! unsupported syntax intentionally fail without native prefix side effects.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = DATA + 4096;
const ERRNO: u32 = DATA + 0x800;
const COUNTER: u32 = ERRNO + 4;
const SEEN_ERRNO: u32 = ERRNO + 8;
const SEEN_OUTPUT: u32 = ERRNO + 12;
const FORMAT: u32 = DATA + 0x900;
const TEXT: u32 = DATA + 0xa00;
const VA: u32 = DATA + 0xb00;
const HANDLER: u32 = CODE + 4096;
const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    for module in MODULES {
        for (name, conv) in [
            ("sprintf", Conv::CdeclVariadic { fixed: 2 }),
            ("vsprintf", Conv::Cdecl { argc: 3 }),
        ] {
            let api = registry.get(module, name).unwrap();
            assert_eq!(api.conv, conv);
            assert!(registry.dynamic_available(module, name));
            assert_eq!(
                adv_abi::signature::lookup(module, name).unwrap().conv,
                Some(conv)
            );
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-printf-tests");
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
    m.cpu.map(CODE, 8192, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.map(DATA + 8192, 4096, Prot::READ).unwrap();
    m
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
fn esp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}
fn errno(code: &mut Asm, api: u32) {
    code.call(api).mov_mem_eax(ERRNO);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
}
fn run<B: Backend>(m: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("nonsecure printf authored guest", CODE, &[], 65536)
        .unwrap();
    m.run_virtual(1_000_000)
}
fn read<B: Backend>(m: &Machine<B>, at: u32, count: usize) -> Vec<u8> {
    let mut bytes = vec![0; count];
    m.cpu.read(at, &mut bytes).unwrap();
    bytes
}

fn success<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    m.cpu.write(FORMAT, b"%.0f|%.14g|%d|%s\0").unwrap();
    m.cpu.write(TEXT, b"ab\0").unwrap();
    let doubles = [2.5f64.to_bits(), 1.0f64.to_bits()];
    let values = [
        doubles[0] as u32,
        (doubles[0] >> 32) as u32,
        doubles[1] as u32,
        (doubles[1] >> 32) as u32,
        (-7i32) as u32,
        TEXT,
    ];
    m.cpu
        .write(
            VA,
            &values
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write(OUT, &[0x5a; 512]).unwrap();
    let mut code = Asm::new(CODE);
    let mut rows = Vec::new();
    for module in MODULES {
        let errno_api = m.resolve_import(module, "_errno").unwrap();
        for va_list in [false, true] {
            let name = if va_list { "vsprintf" } else { "sprintf" };
            let api = m.resolve_import(module, name).unwrap();
            let row = DATA + rows.len() as u32 * 32;
            let output = OUT + rows.len() as u32 * 64;
            errno(&mut code, errno_api);
            esp(&mut code, row + 8);
            if va_list {
                call(&mut code, api, &[output, FORMAT, VA]);
            } else {
                call(
                    &mut code,
                    api,
                    &[
                        output, FORMAT, values[0], values[1], values[2], values[3], values[4],
                        values[5],
                    ],
                );
            }
            code.mov_mem_eax(row);
            esp(&mut code, row + 12);
            code.mov_eax_mem(ERRNO)
                .mov_eax_ptr_eax()
                .mov_mem_eax(row + 4);
            rows.push((row, output));
        }
    }
    // A buffer without declared capacity needs only its actual output+NUL to
    // be writable; the adjacent readonly page must not reject this exact fit.
    let tiny_format = FORMAT + 128;
    let tiny_text = TEXT + 128;
    m.cpu.write(tiny_format, b"%s\0").unwrap();
    m.cpu.write(tiny_text, b"x\0").unwrap();
    let exact = DATA + 8190;
    m.cpu.write(exact - 1, &[0x5a; 3]).unwrap();
    let sprintf = m.resolve_import("msvcr80.dll", "sprintf").unwrap();
    call(&mut code, sprintf, &[exact, tiny_format, tiny_text]);
    code.mov_mem_eax(DATA + 512);
    let result = run(&mut m, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (row, output) in rows {
        assert_eq!(m.cpu.read_u32(row).unwrap(), 9);
        assert_eq!(m.cpu.read_u32(row + 4).unwrap(), 79);
        assert_eq!(
            m.cpu.read_u32(row + 8).unwrap(),
            m.cpu.read_u32(row + 12).unwrap()
        );
        assert_eq!(read(&m, output, 10), b"3|1|-7|ab\0");
        assert_eq!(read(&m, output + 10, 22), [0x5a; 22]);
    }
    assert_eq!(m.cpu.read_u32(DATA + 512).unwrap(), 1);
    assert_eq!(read(&m, exact - 1, 3), [0x5a, b'x', 0]);
    assert_eq!(m.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
fn six_fronts_read_two_double_slots_and_only_write_actual_output_plus_nul() {
    success(UnicornCpu::new(PARK_ADDR).unwrap());
    success(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn failure<B: Backend>(mut cpu: impl FnMut() -> B) {
    for module in ["msvcr80.dll", "msvcr90.dll"] {
        let mut m = machine(cpu());
        m.cpu.write(OUT, &[0x5a; 32]).unwrap();
        m.cpu.write(FORMAT, b"AB%nCD\0%d\0").unwrap();
        m.cpu.write_u32(TEXT, 0xcccccccc).unwrap();
        m.cpu.write_u32(VA, TEXT).unwrap();
        let errno_api = m.resolve_import(module, "_errno").unwrap();
        let setter = m
            .resolve_import(module, "_set_invalid_parameter_handler")
            .unwrap();
        let mut handler = Asm::new(HANDLER);
        handler.bytes.extend_from_slice(&[0xff, 0x05]);
        handler.bytes.extend_from_slice(&COUNTER.to_le_bytes());
        handler
            .mov_eax_mem(ERRNO)
            .mov_eax_ptr_eax()
            .mov_mem_eax(SEEN_ERRNO);
        handler.mov_eax_mem(OUT).mov_mem_eax(SEEN_OUTPUT).ret();
        m.cpu.write(HANDLER, &handler.bytes).unwrap();
        let mut code = Asm::new(CODE);
        call(&mut code, setter, &[HANDLER]);
        let mut rows = Vec::new();
        for va_list in [false, true] {
            let api = m
                .resolve_import(module, if va_list { "vsprintf" } else { "sprintf" })
                .unwrap();
            for case in 0..3 {
                let row = DATA + rows.len() as u32 * 32;
                errno(&mut code, errno_api);
                code.mov_mem_imm(COUNTER, 0)
                    .mov_mem_imm(SEEN_ERRNO, 0)
                    .mov_mem_imm(SEEN_OUTPUT, 0);
                let (dest, format) = match case {
                    0 => (OUT, 0),
                    1 => (0, FORMAT + 7),
                    _ => (OUT, FORMAT),
                };
                esp(&mut code, row + 20);
                if va_list {
                    call(&mut code, api, &[dest, format, VA]);
                } else {
                    call(&mut code, api, &[dest, format, TEXT]);
                }
                code.mov_mem_eax(row);
                esp(&mut code, row + 24);
                for (source, offset) in [
                    (ERRNO, 4),
                    (COUNTER, 8),
                    (SEEN_ERRNO, 12),
                    (SEEN_OUTPUT, 16),
                ] {
                    code.mov_eax_mem(source);
                    if source == ERRNO {
                        code.mov_eax_ptr_eax();
                    }
                    code.mov_mem_eax(row + offset);
                }
                rows.push((row, case));
            }
        }
        let result = run(&mut m, code);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for (row, case) in rows {
            assert_eq!(m.cpu.read_u32(row).unwrap(), u32::MAX);
            let unsupported = case == 2;
            assert_eq!(
                m.cpu.read_u32(row + 4).unwrap(),
                if unsupported { 40 } else { 22 }
            );
            assert_eq!(m.cpu.read_u32(row + 8).unwrap(), u32::from(!unsupported));
            assert_eq!(
                m.cpu.read_u32(row + 12).unwrap(),
                if unsupported { 0 } else { 22 }
            );
            assert_eq!(
                m.cpu.read_u32(row + 16).unwrap(),
                if unsupported { 0 } else { 0x5a5a5a5a }
            );
            assert_eq!(
                m.cpu.read_u32(row + 20).unwrap(),
                m.cpu.read_u32(row + 24).unwrap()
            );
        }
        assert_eq!(read(&m, OUT, 32), [0x5a; 32]);
        assert_eq!(m.cpu.read_u32(TEXT).unwrap(), 0xcccccccc);
        assert!(
            m.ext
                .get::<adv_abi::ApiDiagnostics>()
                .warnings
                .keys()
                .any(|(api, _)| api == &format!("{module}!sprintf"))
        );
    }
    let mut m = machine(cpu());
    m.cpu.write(FORMAT, b"%d\0").unwrap();
    let api = m.resolve_import("msvcr80.dll", "sprintf").unwrap();
    let output = DATA + 8191;
    m.cpu.write(output, &[0x5a]).unwrap();
    let mut code = Asm::new(CODE);
    call(&mut code, api, &[output, FORMAT, 123]); // Actual four bytes cross into readonly.
    assert!(matches!(run(&mut m, code), RunResult::Failed(_)));
    assert_eq!(m.cpu.read_u8(output).unwrap(), 0x5a);
}

#[test]
fn null_invalid_handlers_and_explicit_unsupported_failures_preserve_the_output_atomically() {
    failure(|| UnicornCpu::new(PARK_ADDR).unwrap());
    failure(|| JitCpu::new(PARK_ADDR, false).unwrap());
}
