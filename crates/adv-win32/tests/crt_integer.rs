//! Integer conversions run through generated cdecl fronts in both backends.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TEXT: u32 = DATA + 0x800;
const WIDE: u32 = DATA + 0xa00;
const ERRNO: u32 = DATA + 0x700;
const CALLBACK: u32 = CODE + 0x3000;
const FORMATS: &[(&str, bool, bool, bool)] = &[
    ("atoi", false, false, true),
    ("atol", false, false, true),
    ("atoll", false, true, true),
    ("_atoi64", false, true, true),
    ("_wtoi", true, false, true),
    ("_wtol", true, false, true),
    ("_wtoll", true, true, true),
    ("_wtoi64", true, true, true),
    ("strtol", false, false, false),
    ("strtoul", false, false, false),
    ("strtoll", false, true, false),
    ("strtoull", false, true, false),
    ("_strtoi64", false, true, false),
    ("_strtoui64", false, true, false),
    ("wcstol", true, false, false),
    ("wcstoul", true, false, false),
    ("wcstoll", true, true, false),
    ("wcstoull", true, true, false),
    ("_wcstoi64", true, true, false),
    ("_wcstoui64", true, true, false),
];
fn machine(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-integer",
            "fixture.exe",
            "/nonexistent/crt-integer-profile",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m
}
fn invoke(m: &mut Machine<JitCpu>, a: &mut Asm, module: &str, name: &str, args: &[u32], out: u32) {
    for &v in args.iter().rev() {
        a.push_imm(v);
    }
    a.call(m.resolve_import(module, name).unwrap())
        .add_esp(args.len() as u8 * 4)
        .mov_mem_eax(out);
    a.bytes.extend_from_slice(&[0x89, 0x15]); // mov [out+4], edx
    a.bytes.extend_from_slice(&(out + 4).to_le_bytes());
}
fn errno(m: &mut Machine<JitCpu>, a: &mut Asm, module: &str) {
    let owner = if module.starts_with("api-ms-win-crt-") {
        "ucrtbase.dll"
    } else {
        module
    };
    invoke(m, a, owner, "_errno", &[], ERRNO);
    a.bytes.extend_from_slice(&[0xc7, 0x00]);
    a.bytes.extend_from_slice(&79u32.to_le_bytes());
}
fn sample_errno(a: &mut Asm, out: u32) {
    a.mov_eax_mem(ERRNO).mov_eax_ptr_eax().mov_mem_eax(out);
}
fn run(m: &mut Machine<JitCpu>, mut a: Asm) -> RunResult {
    a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("CRT integer fixture", CODE, &[], 65536).unwrap();
    m.run_virtual(1_000_000)
}
fn clean(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}
fn text(m: &mut Machine<JitCpu>, value: &str, wide: bool, address: u32) {
    let bytes: Vec<u8> = if wide {
        value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        value.bytes().chain([0]).collect()
    };
    m.cpu.write(address, &bytes).unwrap();
}

// spec: CRT-LIB-11, PERS-ABI-1
#[test]
fn every_cataloged_conversion_front_uses_real_width_and_runtime_errno() {
    let modules = [
        "msvcrt20.dll",
        "msvcrt40.dll",
        "msvcrt.dll",
        "msvcr70.dll",
        "msvcr71.dll",
        "msvcr80.dll",
        "msvcr90.dll",
        "msvcr100.dll",
        "msvcr110.dll",
        "msvcr120.dll",
        "ucrtbase.dll",
        "api-ms-win-crt-convert-l1-1-0.dll",
    ];
    let catalog: std::collections::HashSet<_> = include_str!("../../../data/abi/crt-exports.tsv")
        .lines()
        .filter(|r| !r.starts_with('#'))
        .map(|r| {
            let mut f = r.split('\t');
            (f.next().unwrap(), f.next().unwrap())
        })
        .collect();
    for jit in [false, true] {
        for module in modules {
            let mut m = machine(jit);
            text(&mut m, " \t-42!", false, TEXT);
            text(&mut m, " \t-42!", true, WIDE);
            let mut a = Asm::new(CODE);
            errno(&mut m, &mut a, module);
            let mut checked = Vec::new();
            for (index, &(name, wide, full, decimal)) in FORMATS.iter().enumerate() {
                if !catalog.contains(&(module, name)) {
                    continue;
                }
                let out = DATA + index as u32 * 16;
                let source = if wide { WIDE } else { TEXT };
                let args = if decimal {
                    vec![source]
                } else {
                    vec![source, out + 8, 10]
                };
                invoke(&mut m, &mut a, module, name, &args, out);
                checked.push((name, source, out, wide, full, decimal));
            }
            sample_errno(&mut a, DATA + 0x600);
            let result = run(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            for (name, source, out, wide, full, decimal) in checked {
                assert_eq!(
                    m.cpu.read_u32(out).unwrap(),
                    (-42i32) as u32,
                    "{module}!{name}"
                );
                if full {
                    assert_eq!(
                        m.cpu.read_u32(out + 4).unwrap(),
                        u32::MAX,
                        "{module}!{name}"
                    );
                }
                if !decimal {
                    assert_eq!(
                        m.cpu.read_u32(out + 8).unwrap(),
                        source + if wide { 10 } else { 5 },
                        "{module}!{name}"
                    );
                }
            }
            assert_eq!(m.cpu.read_u32(DATA + 0x600).unwrap(), 79);
            clean(&m);
        }
    }
}

// spec: CRT-LIB-11
#[test]
fn signed_unsigned_limits_radices_and_unicode_digits_preserve_exact_end_positions() {
    for jit in [false, true] {
        for module in [
            "msvcr90.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-convert-l1-1-0.dll",
        ] {
            for (name, input, radix, expected, consumed, error) in [
                ("strtol", "2147483647!", 10, 0x7fffffff, 10, 79),
                ("strtol", "2147483648!", 10, 0x7fffffff, 10, 34),
                ("strtol", "-2147483648!", 10, 0x80000000, 11, 79),
                ("strtol", "-2147483649!", 10, 0x80000000, 11, 34),
                ("strtoul", "4294967295!", 10, 0xffffffff, 10, 79),
                ("strtoul", "4294967296!", 10, 0xffffffff, 10, 34),
                ("strtoul", "-4294967295!", 10, 1, 11, 79),
                ("strtoul", "-4294967296!", 10, 0xffffffff, 11, 34),
                (
                    "_strtoi64",
                    "9223372036854775807!",
                    10,
                    0x7fffffffffffffff,
                    19,
                    79,
                ),
                (
                    "_strtoi64",
                    "9223372036854775808!",
                    10,
                    0x7fffffffffffffff,
                    19,
                    34,
                ),
                (
                    "_strtoi64",
                    "-9223372036854775808!",
                    10,
                    0x8000000000000000,
                    20,
                    79,
                ),
                (
                    "_strtoi64",
                    "-9223372036854775809!",
                    10,
                    0x8000000000000000,
                    20,
                    34,
                ),
                ("_strtoui64", "18446744073709551615!", 10, u64::MAX, 20, 79),
                ("_strtoui64", "18446744073709551616!", 10, u64::MAX, 20, 34),
                ("_strtoui64", "-18446744073709551615!", 10, 1, 21, 79),
                ("_strtoui64", "-18446744073709551616!", 10, u64::MAX, 21, 34),
                ("strtol", "  -0x2aG", 0, (-42i32) as u32 as u64, 7, 79),
                ("strtoul", "0779", 0, 63, 3, 79),
                ("strtol", "1012", 2, 5, 3, 79),
                ("strtol", "z!", 36, 35, 1, 79),
                ("strtol", "  +x", 10, 0, 0, 79),
                ("strtol", "0xG", 0, 0, 0, 79),
                ("_wcstoi64", "１２٣!", 10, 123, 3, 79),
                ("_wcstoi64", "０xF!", 0, 15, 3, 79),
            ] {
                let wide = name.starts_with("_w");
                let full = name.contains("64");
                let mut m = machine(jit);
                text(&mut m, input, wide, TEXT);
                let mut a = Asm::new(CODE);
                errno(&mut m, &mut a, module);
                invoke(&mut m, &mut a, module, name, &[TEXT, DATA + 8, radix], DATA);
                sample_errno(&mut a, DATA + 12);
                assert!(
                    matches!(run(&mut m, a), RunResult::Exited(0)),
                    "{module}!{name}: {input}"
                );
                let actual = u64::from(m.cpu.read_u32(DATA).unwrap())
                    | if full {
                        u64::from(m.cpu.read_u32(DATA + 4).unwrap()) << 32
                    } else {
                        0
                    };
                assert_eq!(actual, expected, "{module}!{name}: {input}");
                assert_eq!(
                    m.cpu.read_u32(DATA + 8).unwrap(),
                    TEXT + consumed * if wide { 2 } else { 1 },
                    "{module}!{name}: {input}"
                );
                assert_eq!(
                    m.cpu.read_u32(DATA + 12).unwrap(),
                    error,
                    "{module}!{name}: {input}"
                );
                clean(&m);
            }
        }
    }
}

// spec: CRT-LIB-12
#[test]
fn conversion_access_stops_at_a_nondigit_or_known_overflow_without_an_end_output() {
    for jit in [false, true] {
        for (name, bytes, expected) in [
            ("strtol", b"7!".to_vec(), 7),
            ("wcstol", vec![b'7', 0, b'!', 0], 7),
            ("strtol", b"2147483648".to_vec(), 0x7fffffff),
            (
                "wcstol",
                "2147483648"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                0x7fffffff,
            ),
        ] {
            let mut m = machine(jit);
            let pointer = DATA + 4096 - bytes.len() as u32;
            m.cpu.write(pointer, &bytes).unwrap();
            let mut a = Asm::new(CODE);
            errno(&mut m, &mut a, "msvcr90.dll");
            invoke(&mut m, &mut a, "msvcr90.dll", name, &[pointer, 0, 10], DATA);
            assert!(
                matches!(run(&mut m, a), RunResult::Exited(0)),
                "{name} {jit}"
            );
            assert_eq!(m.cpu.read_u32(DATA).unwrap(), expected);
            clean(&m);
        }
    }
}

// spec: CRT-LIB-12
#[test]
fn invalid_parameter_callbacks_observe_initialized_end_and_full_width_zero_returns() {
    for jit in [false, true] {
        for (module, modern) in [
            ("msvcr70.dll", false),
            ("msvcr90.dll", true),
            ("ucrtbase.dll", true),
            ("api-ms-win-crt-convert-l1-1-0.dll", true),
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            errno(&mut m, &mut a, module);
            if modern {
                let mut callback = Asm::new(CALLBACK);
                callback.mov_eax_mem(DATA + 8).mov_mem_eax(DATA + 0x300);
                sample_errno(&mut callback, DATA + 0x304);
                callback.bytes.extend_from_slice(&[0xff, 0x05]);
                callback
                    .bytes
                    .extend_from_slice(&(DATA + 0x30c).to_le_bytes());
                callback
                    .bytes
                    .extend_from_slice(&[0xba, 0xbe, 0xba, 0xfe, 0xca]);
                callback.bytes.extend_from_slice(&[0xb8, 0x99, 0x99, 0, 0]);
                callback.ret();
                m.cpu.write(CALLBACK, &callback.bytes).unwrap();
                let owner = if module.starts_with("api-ms-win-crt-") {
                    "ucrtbase.dll"
                } else {
                    module
                };
                invoke(
                    &mut m,
                    &mut a,
                    owner,
                    "_set_invalid_parameter_handler",
                    &[CALLBACK],
                    DATA + 0x380,
                );
            }
            invoke(
                &mut m,
                &mut a,
                module,
                "_strtoi64",
                &[TEXT, DATA + 8, 1],
                DATA,
            );
            invoke(
                &mut m,
                &mut a,
                module,
                "_strtoi64",
                &[0, DATA + 0x108, 10],
                DATA + 0x100,
            );
            assert!(
                matches!(run(&mut m, a), RunResult::Exited(0)),
                "{module} {jit}"
            );
            for p in [DATA, DATA + 4, DATA + 0x100, DATA + 0x104, DATA + 0x108] {
                assert_eq!(m.cpu.read_u32(p).unwrap(), 0);
            }
            assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), TEXT);
            if modern {
                assert_eq!(m.cpu.read_u32(DATA + 0x300).unwrap(), TEXT);
                assert_eq!(m.cpu.read_u32(DATA + 0x304).unwrap(), 22);
                assert_eq!(m.cpu.read_u32(DATA + 0x30c).unwrap(), 2);
            }
            clean(&m);
        }
    }
}
