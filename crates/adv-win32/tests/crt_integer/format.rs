use super::*;
const FORMATS: &[(&str, bool, bool, bool)] = &[
    ("_itoa", false, false, false),
    ("_ltoa", false, false, false),
    ("_ultoa", false, false, true),
    ("_i64toa", false, true, false),
    ("_ui64toa", false, true, true),
    ("_itow", true, false, false),
    ("_ltow", true, false, false),
    ("_ultow", true, false, true),
    ("_i64tow", true, true, false),
    ("_ui64tow", true, true, true),
];
fn arguments(
    value: u64,
    full: bool,
    secure: bool,
    pointer: u32,
    capacity: u32,
    radix: u32,
) -> Vec<u32> {
    let mut args = vec![value as u32];
    if full {
        args.push((value >> 32) as u32);
    }
    args.push(pointer);
    if secure {
        args.push(capacity);
    }
    args.push(radix);
    args
}
fn string(m: &Machine<JitCpu>, pointer: u32, wide: bool) -> String {
    if wide {
        String::from_utf16(
            &adv_abi::WideStr(adv_abi::GuestPtr::new(pointer))
                .read(&m.cpu, 128)
                .unwrap(),
        )
        .unwrap()
    } else {
        String::from_utf8(m.cpu.read_cstr(pointer, 128).unwrap()).unwrap()
    }
}

// spec: CRT-LIB-13
#[test]
fn all_cataloged_radix_fronts_share_x86_signedness_and_code_unit_outputs() {
    let catalog: std::collections::HashSet<_> =
        include_str!("../../../../data/abi/crt-exports.tsv")
            .lines()
            .filter(|r| !r.starts_with('#'))
            .map(|r| {
                let mut f = r.split('\t');
                (f.next().unwrap(), f.next().unwrap())
            })
            .collect();
    for jit in [false, true] {
        for module in [
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
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            errno(&mut m, &mut a, module);
            let mut checked = Vec::new();
            for (i, &(base, wide, full, unsigned)) in FORMATS.iter().enumerate() {
                for secure in [false, true] {
                    let name = format!("{base}{}", if secure { "_s" } else { "" });
                    if !catalog.contains(&(module, name.as_str())) {
                        continue;
                    }
                    let index = i as u32 * 2 + u32::from(secure);
                    let pointer = DATA + index * 64;
                    let out = DATA + 0x500 + index * 8;
                    let (value, expected) = match (full, unsigned) {
                        (false, false) => (0x80000000, "-2147483648"),
                        (false, true) => (0xffffffff, "4294967295"),
                        (true, false) => (0x8000000000000000, "-9223372036854775808"),
                        (true, true) => (u64::MAX, "18446744073709551615"),
                    };
                    invoke(
                        &mut m,
                        &mut a,
                        module,
                        &name,
                        &arguments(value, full, secure, pointer, 32, 10),
                        out,
                    );
                    checked.push((name, pointer, out, wide, secure, expected));
                }
            }
            sample_errno(&mut a, DATA + 0x650);
            let result = run(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            for (name, pointer, out, wide, secure, expected) in checked {
                assert_eq!(
                    m.cpu.read_u32(out).unwrap(),
                    if secure { 0 } else { pointer },
                    "{module}!{name}"
                );
                assert_eq!(string(&m, pointer, wide), expected, "{module}!{name}");
            }
            assert_eq!(m.cpu.read_u32(DATA + 0x650).unwrap(), 79);
            clean(&m);
        }
    }
}

// spec: CRT-LIB-13
#[test]
fn radix_boundaries_use_unsigned_patterns_outside_decimal_and_terminate_exact_buffers() {
    for jit in [false, true] {
        for (name, full, wide, secure, value, radix, expected) in [
            (
                "_itoa",
                false,
                false,
                false,
                0xffffffff,
                16,
                "ffffffff".to_owned(),
            ),
            (
                "_itow_s",
                false,
                true,
                true,
                0xffffffff,
                16,
                "ffffffff".to_owned(),
            ),
            (
                "_i64toa_s",
                true,
                false,
                true,
                u64::MAX,
                16,
                "ffffffffffffffff".to_owned(),
            ),
            (
                "_i64tow",
                true,
                true,
                false,
                u64::MAX,
                16,
                "ffffffffffffffff".to_owned(),
            ),
            ("_ui64toa_s", true, false, true, u64::MAX, 2, "1".repeat(64)),
            ("_ui64tow_s", true, true, true, u64::MAX, 2, "1".repeat(64)),
            ("_ultoa_s", false, false, true, 35, 36, "z".to_owned()),
            ("_ultow_s", false, true, true, 63, 8, "77".to_owned()),
            ("_itoa_s", false, false, true, 0, 10, "0".to_owned()),
            ("_itow_s", false, true, true, 0, 10, "0".to_owned()),
        ] {
            let mut m = machine(jit);
            m.cpu.write(TEXT, &[0xcc; 256]).unwrap();
            let mut a = Asm::new(CODE);
            errno(&mut m, &mut a, "msvcr90.dll");
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                name,
                &arguments(value, full, secure, TEXT, expected.len() as u32 + 1, radix),
                DATA,
            );
            assert!(
                matches!(run(&mut m, a), RunResult::Exited(0)),
                "{name} {jit}"
            );
            assert_eq!(string(&m, TEXT, wide), expected);
            let end = TEXT + (expected.len() as u32 + 1) * if wide { 2 } else { 1 };
            assert_eq!(m.cpu.read_u8(end).unwrap(), 0xcc);
            clean(&m);
        }
    }
}

// spec: CRT-LIB-13
#[test]
fn secure_radix_failures_preserve_native_partial_digits_and_callback_error_changes() {
    for jit in [false, true] {
        for (name, wide, full) in [
            ("_itoa_s", false, false),
            ("_itow_s", true, false),
            ("_ui64toa_s", false, true),
            ("_ui64tow_s", true, true),
        ] {
            for (value, capacity, radix, error, expected) in [
                (123456u64, 4, 10, 34, vec![0u16, 53, 52, 51]),
                (123456, 1, 1, 34, vec![0]),
                (123456, 8, 1, 22, vec![0]),
                (123456, 0, 10, 22, Vec::new()),
            ] {
                let mut m = machine(jit);
                m.cpu.write(TEXT, &[0xcc; 64]).unwrap();
                let mut a = Asm::new(CODE);
                errno(&mut m, &mut a, "msvcr90.dll");
                let mut callback = Asm::new(CALLBACK);
                sample_errno(&mut callback, DATA + 0x680);
                callback.mov_eax_mem(ERRNO);
                callback
                    .bytes
                    .extend_from_slice(&[0xc7, 0x00, 123, 0, 0, 0]);
                callback.ret();
                m.cpu.write(CALLBACK, &callback.bytes).unwrap();
                invoke(
                    &mut m,
                    &mut a,
                    "msvcr90.dll",
                    "_set_invalid_parameter_handler",
                    &[CALLBACK],
                    DATA + 0x600,
                );
                invoke(
                    &mut m,
                    &mut a,
                    "msvcr90.dll",
                    name,
                    &arguments(value, full, true, TEXT, capacity, radix),
                    DATA,
                );
                sample_errno(&mut a, DATA + 0x650);
                assert!(
                    matches!(run(&mut m, a), RunResult::Exited(0)),
                    "{name} {capacity} {radix} {jit}"
                );
                assert_eq!(m.cpu.read_u32(DATA).unwrap(), error);
                assert_eq!(m.cpu.read_u32(DATA + 0x680).unwrap(), error);
                assert_eq!(m.cpu.read_u32(DATA + 0x650).unwrap(), 123);
                for (i, unit) in expected.iter().enumerate() {
                    let address = TEXT + i as u32 * if wide { 2 } else { 1 };
                    let actual = if wide {
                        m.cpu.read_u16(address).unwrap()
                    } else {
                        u16::from(m.cpu.read_u8(address).unwrap())
                    };
                    assert_eq!(actual, *unit, "{name} {capacity} {radix}");
                }
                let next = TEXT + expected.len() as u32 * if wide { 2 } else { 1 };
                assert_eq!(m.cpu.read_u8(next).unwrap(), 0xcc);
                clean(&m);
            }
        }
    }
}
