//! Authored x86 calls. VC90 native evidence lives under target/crt-format-native.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = 0x420000;
const FORMAT: u32 = DATA + 0x800;
const VA: u32 = DATA + 0x1000;
const TEXT: u32 = DATA + 0x1400;
const WTEXT: u32 = DATA + 0x1500;
const CALLBACK: u32 = CODE + 0x800;

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in ["msvcr80.dll", "msvcr90.dll"] {
        for front in FRONTS {
            let signature = adv_abi::signature::lookup(module, front.name).unwrap();
            assert_eq!(
                registry.get(module, front.name).unwrap().conv,
                signature.conv.unwrap()
            );
            assert!(registry.dynamic_available(module, front.name));
        }
    }
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-format-tests");
    crate::Win32::install(&mut m, crate::Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.map(OUT, 4096, Prot::RW).unwrap();
    m.cpu.map(OUT + 4096, 4096, Prot::READ).unwrap();
    let errno = m.resolve_import("msvcr90.dll", "_errno").unwrap();
    let mut handler = Asm::new(CALLBACK);
    handler.bytes.extend_from_slice(&[0xff, 0x05]);
    handler.bytes.extend_from_slice(&(DATA + 16).to_le_bytes());
    for i in 0..5u8 {
        handler
            .bytes
            .extend_from_slice(&[0x8b, 0x44, 0x24, 4 + i * 4]);
        handler.mov_mem_eax(DATA + 32 + u32::from(i) * 4);
    }
    handler.call(errno);
    handler.bytes.extend_from_slice(&[0x8b, 0x00]);
    handler.mov_mem_eax(DATA + 20);
    handler.bytes.extend_from_slice(&[0x0f, 0xb6, 0x05]);
    handler.bytes.extend_from_slice(&OUT.to_le_bytes());
    handler.mov_mem_eax(DATA + 24);
    handler.ret();
    m.cpu.write(CALLBACK, &handler.bytes).unwrap();
    m
}
fn wide<B: Backend>(m: &mut Machine<B>, pointer: u32, text: &[u16]) {
    let bytes: Vec<_> = text
        .iter()
        .copied()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(pointer, &bytes).unwrap();
}
fn format<B: Backend>(m: &mut Machine<B>, text: &str, unicode: bool) {
    if unicode {
        wide(m, FORMAT, &text.encode_utf16().collect::<Vec<_>>());
    } else {
        m.cpu
            .write(FORMAT, &[text.as_bytes(), &[0]].concat())
            .unwrap();
    }
}
fn seed<B: Backend>(m: &mut Machine<B>) {
    m.cpu.write(DATA, &[0; 64]).unwrap();
    m.cpu.write(OUT, &[0x5a; 256]).unwrap();
}
fn push_call(code: &mut Asm, api: u32, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(api).add_esp((args.len() * 4) as u8);
}
fn invoke<B: Backend>(
    m: &mut Machine<B>,
    name: &str,
    args: &[u32],
    locale: Option<&[u8]>,
) -> RunResult {
    let mut code = Asm::new(CODE);
    let setter = m
        .resolve_import("msvcr90.dll", "_set_invalid_parameter_handler")
        .unwrap();
    let errno = m.resolve_import("msvcr90.dll", "_errno").unwrap();
    let set_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
    let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
    let api = m.resolve_import("msvcr90.dll", name).unwrap();
    push_call(&mut code, setter, &[CALLBACK]);
    if let Some(locale) = locale {
        m.cpu
            .write(DATA + 0x1800, &[locale, &[0]].concat())
            .unwrap();
        let setlocale = m.resolve_import("msvcr90.dll", "setlocale").unwrap();
        push_call(&mut code, setlocale, &[2, DATA + 0x1800]);
    }
    code.call(errno).mov_mem_eax(DATA + 12);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
    code.push_imm(73).call(set_error);
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&(DATA + 56).to_le_bytes());
    push_call(&mut code, api, args);
    code.mov_mem_eax(DATA);
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&(DATA + 60).to_le_bytes());
    code.call(get_error).mov_mem_eax(DATA + 4);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("authored CRT format", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn exited(result: RunResult) {
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}
fn errno<B: Backend>(m: &Machine<B>) -> u32 {
    m.cpu.read_u32(m.cpu.read_u32(DATA + 12).unwrap()).unwrap()
}
fn output<B: Backend>(m: &Machine<B>, count: usize, unicode: bool) -> String {
    if unicode {
        String::from_utf16(&crate::util::wstr(&m.cpu, OUT)[..count]).unwrap()
    } else {
        String::from_utf8(crate::util::cstr(&m.cpu, OUT)[..count].to_vec()).unwrap()
    }
}
fn families<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    let expected = "+31:0X00002A:-4294967297:007   :ab:cd:0012ABCD:Z";
    let long = (-4294967297i64) as u64;
    for front in FRONTS {
        seed(&mut m);
        format(&mut m, "%+ld:%#08X:%I64d:%*.*d:%s:%S:%p:%c", front.wide);
        m.cpu.write(TEXT, b"cd\0ab\0").unwrap();
        wide(&mut m, WTEXT, &"ab\0cd".encode_utf16().collect::<Vec<_>>());
        let values = [
            31,
            42,
            long as u32,
            (long >> 32) as u32,
            (-6i32) as u32,
            3,
            7,
            if front.wide { WTEXT } else { TEXT + 3 },
            if front.wide { TEXT } else { WTEXT + 6 },
            0x12abcd,
            u32::from(b'Z'),
        ];
        let bytes: Vec<_> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        m.cpu.write(VA, &bytes).unwrap();
        let mut args = if front.mode == Mode::Measure {
            vec![FORMAT]
        } else if front.mode == Mode::Unbounded {
            vec![OUT]
        } else {
            vec![OUT, 256]
        };
        if front.mode == Mode::Bounded {
            args.push(u32::MAX);
        }
        if front.mode != Mode::Measure {
            args.push(FORMAT);
        }
        if front.va_list {
            args.push(VA);
        } else {
            args.extend(values);
        }
        exited(invoke(&mut m, front.name, &args, None));
        assert_eq!(
            m.cpu.read_u32(DATA).unwrap(),
            expected.len() as u32,
            "{}",
            front.name
        );
        assert_eq!(
            m.cpu.read_u32(DATA + 56).unwrap(),
            m.cpu.read_u32(DATA + 60).unwrap(),
            "cdecl caller stack"
        );
        assert_eq!(errno(&m), 79);
        assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 73);
        assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
        if front.mode == Mode::Measure {
            assert_eq!(m.cpu.read_u32(OUT).unwrap(), 0x5a5a5a5a);
        } else {
            assert_eq!(output(&m, expected.len(), front.wide), expected);
        }
        let mut after = vec![0; bytes.len()];
        m.cpu.read(VA, &mut after).unwrap();
        assert_eq!(after, bytes);
    }
}
#[test]
fn all_fronts_share_checked_x86_va_transport_integer_widths_and_measurement() {
    families(UnicornCpu::new(PARK_ADDR).unwrap());
    families(JitCpu::new(PARK_ADDR, false).unwrap());
}
fn boundaries<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    // Result, errno, callback count, callback-observed errno, output prefix.
    for (name, capacity, count, text, result, error, callbacks, seen, prefix) in [
        ("sprintf_s", 4, 0, "abc", 3, 79, 0, 0, "abc"),
        ("sprintf_s", 3, 0, "abc", -1, 34, 1, 34, ""),
        ("_snprintf_s", 4, u32::MAX, "abcdef", -1, 79, 0, 0, "abc"),
        ("_snprintf_s", 8, 3, "abcdef", -1, 79, 0, 0, "abc"),
        ("_snprintf_s", 4, 5, "abcdef", -1, 34, 1, 34, ""),
        ("_snprintf_s", 4, 0, "abcdef", -1, 79, 0, 0, ""),
        ("_snprintf_s", 4, 0, "", 0, 79, 0, 0, ""),
        ("swprintf_s", 4, 0, "abc", 3, 79, 0, 0, "abc"),
        ("swprintf_s", 3, 0, "abc", -1, 34, 1, 34, ""),
        ("_snwprintf_s", 4, u32::MAX, "abcdef", -1, 79, 0, 0, "abc"),
    ] {
        seed(&mut m);
        let unicode = name.contains('w');
        format(&mut m, "%s", unicode);
        if unicode {
            wide(&mut m, TEXT, &text.encode_utf16().collect::<Vec<_>>());
        } else {
            m.cpu
                .write(TEXT, &[text.as_bytes(), &[0]].concat())
                .unwrap();
        }
        let mut args = vec![OUT, capacity];
        if name.starts_with('_') {
            args.push(count);
        }
        args.extend([FORMAT, TEXT]);
        exited(invoke(&mut m, name, &args, None));
        assert_eq!(
            m.cpu.read_u32(DATA).unwrap() as i32,
            result,
            "{name}, cap {capacity}, count {count}"
        );
        assert_eq!(errno(&m), error);
        assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), callbacks);
        assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), seen);
        assert_eq!(output(&m, prefix.len(), unicode), prefix);
        if callbacks != 0 {
            assert_eq!(
                m.cpu.read_u32(DATA + 24).unwrap(),
                0,
                "empty output visible before callback"
            );
            for i in 0..5 {
                assert_eq!(m.cpu.read_u32(DATA + 32 + i * 4).unwrap(), 0);
            }
        }
    }
    seed(&mut m);
    exited(invoke(&mut m, "sprintf_s", &[OUT, 16, 0], None));
    assert_eq!(errno(&m), 22);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 1);
    assert_eq!(
        m.cpu.read_u32(OUT).unwrap(),
        0x5a5a5a5a,
        "NULL format does not reset output"
    );
    seed(&mut m);
    format(&mut m, "abc", false);
    exited(invoke(&mut m, "_snprintf_s", &[0, 0, 0, FORMAT], None));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(errno(&m), 79);
}
#[test]
fn vc90_native_capacity_truncation_errno_and_returning_invalid_callback_contracts() {
    boundaries(UnicornCpu::new(PARK_ADDR).unwrap());
    boundaries(JitCpu::new(PARK_ADDR, false).unwrap());
}
fn locale_cases<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    assert_eq!(m.ext.get::<crate::Win32>().codec.acp, 932);
    seed(&mut m);
    format(&mut m, "%.3ls", false);
    wide(&mut m, WTEXT, &[0x3042, 0x3044]);
    exited(invoke(
        &mut m,
        "sprintf_s",
        &[OUT, 16, FORMAT, WTEXT],
        Some(b"Japanese_Japan.932"),
    ));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 4);
    assert_eq!(&crate::util::cstr(&m.cpu, OUT), &[0x82, 0xa0, 0x82, 0xa2]);
    seed(&mut m);
    format(&mut m, "%.1S", true);
    m.cpu.write(TEXT, &[0x82, 0xa0, 0x82, 0xa2, 0]).unwrap();
    exited(invoke(&mut m, "swprintf_s", &[OUT, 16, FORMAT, TEXT], None));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
    assert_eq!(&crate::util::wstr(&m.cpu, OUT), &[0x3042]);
    seed(&mut m);
    format(&mut m, "%S", true);
    m.cpu.write(TEXT, &[0xe9, 0]).unwrap();
    exited(invoke(
        &mut m,
        "swprintf_s",
        &[OUT, 16, FORMAT, TEXT],
        Some(b"English_United States.1252"),
    ));
    assert_eq!(&crate::util::wstr(&m.cpu, OUT), &[0xe9]);
    seed(&mut m);
    m.cpu.write(FORMAT, &[0x81, b'%', b'd', 0]).unwrap();
    exited(invoke(&mut m, "sprintf_s", &[OUT, 16, FORMAT, 7], None));
    assert_eq!(
        &crate::util::cstr(&m.cpu, OUT),
        &[0x81, b'7'],
        "CRT1252 format scanner ignores Win32 ACP932 lead ranges"
    );
    seed(&mut m);
    format(&mut m, "%ls", false);
    exited(invoke(
        &mut m,
        "sprintf_s",
        &[OUT, 16, FORMAT, WTEXT],
        Some(b"C"),
    ));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), u32::MAX);
    assert_eq!(errno(&m), 42);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    seed(&mut m);
    format(&mut m, "%S", true);
    m.cpu.write(TEXT, &[0xe9, 0]).unwrap();
    exited(invoke(&mut m, "swprintf_s", &[OUT, 16, FORMAT, TEXT], None));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
    assert_eq!(
        &crate::util::wstr(&m.cpu, OUT),
        &[0xe9],
        "C byte mapping includes 0x80..0xff"
    );
    seed(&mut m);
    format(&mut m, "%ls", false);
    wide(&mut m, WTEXT, &[0xe9]);
    exited(invoke(&mut m, "sprintf_s", &[OUT, 16, FORMAT, WTEXT], None));
    assert_eq!(&crate::util::cstr(&m.cpu, OUT), &[0xe9]);
    seed(&mut m);
    format(&mut m, "A%lcB", false);
    exited(invoke(
        &mut m,
        "sprintf_s",
        &[OUT, 16, FORMAT, 0x3042],
        None,
    ));
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 2);
    assert_eq!(&crate::util::cstr(&m.cpu, OUT), b"AB");
    assert_eq!(errno(&m), 42);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    seed(&mut m);
    format(&mut m, "%0*.*d", false);
    exited(invoke(
        &mut m,
        "sprintf_s",
        &[OUT, 16, FORMAT, 6, u32::MAX, 7],
        None,
    ));
    assert_eq!(
        &crate::util::cstr(&m.cpu, OUT),
        b"000007",
        "negative precision is omitted, zero-padding remains active"
    );
    seed(&mut m);
    format(&mut m, "%.0d|%#.0o|%hhd", false);
    exited(invoke(
        &mut m,
        "sprintf_s",
        &[OUT, 16, FORMAT, 0, 0, 255],
        None,
    ));
    assert_eq!(
        &crate::util::cstr(&m.cpu, OUT),
        b"|0|255",
        "VC90 hh uses the short-width contract"
    );
}
#[test]
fn crt_locale_and_dbcs_precision_remain_independent_of_win32_acp() {
    locale_cases(UnicornCpu::new(PARK_ADDR).unwrap());
    locale_cases(JitCpu::new(PARK_ADDR, false).unwrap());
}
fn safety<B: Backend>(cpu: B) {
    let mut m = machine(cpu);
    seed(&mut m);
    // Hexadecimal floats remain unsupported and must not touch the null va_list.
    format(&mut m, "%a", false);
    exited(invoke(&mut m, "vsprintf_s", &[OUT, 16, FORMAT, 0], None));
    assert_eq!(errno(&m), 40);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), u32::MAX);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    assert!(
        m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(_, reason)| reason.contains("outside the VC80 profile"))
    );
    seed(&mut m);
    format(&mut m, "%*d", false);
    m.cpu.write_u32(VA, i32::MIN as u32).unwrap();
    exited(invoke(&mut m, "vsprintf_s", &[OUT, 16, FORMAT, VA], None));
    assert_eq!(errno(&m), 40);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    seed(&mut m);
    format(&mut m, "%.0ls", false);
    exited(invoke(
        &mut m,
        "sprintf_s",
        &[OUT, 16, FORMAT, 0x12345000],
        None,
    ));
    assert_eq!(
        m.cpu.read_u32(DATA).unwrap(),
        0,
        "precision zero never dereferences source"
    );
    seed(&mut m);
    format(&mut m, "%n", false);
    m.cpu.write_u32(VA, 0x87654321).unwrap();
    exited(invoke(&mut m, "sprintf_s", &[OUT, 16, FORMAT, VA], None));
    assert_eq!(errno(&m), 22);
    assert_eq!(m.cpu.read_u32(VA).unwrap(), 0x87654321);
}
#[test]
fn rejected_formats_and_large_fields_never_read_guessed_value_slots_or_allocate_unbounded_padding()
{
    safety(UnicornCpu::new(PARK_ADDR).unwrap());
    safety(JitCpu::new(PARK_ADDR, false).unwrap());
}
#[test]
fn protected_outputs_and_unreadable_va_slots_fail_without_partial_publication() {
    let mut m = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    seed(&mut m);
    format(&mut m, "ABC", false);
    let result = invoke(&mut m, "sprintf_s", &[OUT + 4094, 16, FORMAT], None);
    assert!(matches!(result, RunResult::Failed(_)));
    let mut tail = [0; 2];
    m.cpu.read(OUT + 4094, &mut tail).unwrap();
    assert_eq!(tail, [0; 2]);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    let mut m = machine(UnicornCpu::new(PARK_ADDR).unwrap());
    seed(&mut m);
    format(&mut m, "%d", false);
    let result = invoke(&mut m, "vsprintf_s", &[OUT, 16, FORMAT, 0x12345000], None);
    assert!(matches!(result, RunResult::Failed(_)));
    assert_eq!(m.cpu.read_u32(OUT).unwrap(), 0x5a5a5a5a);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
}
