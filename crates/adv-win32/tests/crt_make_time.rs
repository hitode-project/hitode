//! Authored calendars exercise all cataloged CRT projections and fault order.
use adv_abi::{Registry, signature};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
use std::collections::BTreeSet;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
fn machine(jit: bool) -> Machine<JitCpu> {
    let mut r = Registry::default();
    adv_win32::register(&mut r);
    adv_win32::msvcrt::register(&mut r);
    assert!(r.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        r,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut m,
        Win32Config::new(
            "/nonexistent/crt-calendar",
            "fixture.exe",
            "/nonexistent/crt-calendar-data",
        ),
    )
    .unwrap();
    m.cpu.map(CODE, 65536, Prot::RX).unwrap();
    m.cpu.map(DATA, 65536, Prot::RW).unwrap();
    m
}
fn invoke(
    m: &mut Machine<JitCpu>,
    a: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    output: u32,
) {
    for &arg in args.iter().rev() {
        a.push_imm(arg);
    }
    a.call(m.resolve_import(module, name).unwrap());
    if !args.is_empty() {
        a.add_esp(args.len() as u8 * 4);
    }
    a.mov_mem_eax(output);
    a.bytes.extend_from_slice(&[0x89, 0x15]);
    a.bytes.extend_from_slice(&(output + 4).to_le_bytes());
}
fn execute(m: &mut Machine<JitCpu>, mut a: Asm) -> RunResult {
    a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    m.cpu.write(CODE, &a.bytes).unwrap();
    m.spawn("CRT calendar", CODE, &[], 65536).unwrap();
    let r = m.run_virtual(1_000_000);
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
    r
}
fn put(m: &mut Machine<JitCpu>, pointer: u32, tm: [i32; 9]) {
    for (i, value) in tm.into_iter().enumerate() {
        m.cpu
            .write_u32(pointer + i as u32 * 4, value as u32)
            .unwrap();
    }
}
fn tm(m: &Machine<JitCpu>, pointer: u32) -> [i32; 9] {
    std::array::from_fn(|i| m.cpu.read_u32(pointer + i as u32 * 4).unwrap() as i32)
}
fn value(m: &Machine<JitCpu>, output: u32, name: &str) -> i64 {
    if name.ends_with("64") {
        let lo = u64::from(m.cpu.read_u32(output).unwrap());
        (lo | (u64::from(m.cpu.read_u32(output + 4).unwrap()) << 32)) as i64
    } else {
        i64::from(m.cpu.read_u32(output).unwrap() as i32)
    }
}
fn calendar(name: &str) -> bool {
    matches!(
        name.strip_prefix("_o_").unwrap_or(name),
        "mktime" | "_mktime32" | "_mktime64" | "_mkgmtime" | "_mkgmtime32" | "_mkgmtime64"
    )
}

// spec: CRT-LIB-28
#[test]
fn every_projection_normalizes_fields_and_uses_the_existing_timezone() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| calendar(s.name))
        .collect();
    assert_eq!(declarations.len(), 43);
    let modules: BTreeSet<_> = declarations.iter().map(|s| s.module).collect();
    for jit in [false, true] {
        for module in &modules {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            let owner = if module.starts_with("api-ms-win-crt-") {
                "ucrtbase.dll"
            } else {
                module
            };
            invoke(&mut m, &mut a, owner, "_errno", &[], DATA + 0xf000);
            a.bytes.extend_from_slice(&[0xc7, 0, 71, 0, 0, 0]);
            a.push_imm(73)
                .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
            let mut expected = vec![];
            for s in declarations.iter().filter(|s| s.module == *module) {
                for dst in [-1, 0, 1] {
                    let slot = expected.len() as u32;
                    let pointer = DATA + slot * 64;
                    put(&mut m, pointer, [61, -1, 24, 0, 14, 120, 99, 99, dst]);
                    invoke(
                        &mut m,
                        &mut a,
                        module,
                        s.name,
                        &[pointer],
                        DATA + 0x8000 + slot * 8,
                    );
                    expected.push((s.name, pointer, dst));
                }
            }
            a.mov_eax_mem(DATA + 0xf000)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 0xf010);
            a.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
                .mov_mem_eax(DATA + 0xf014);
            assert!(
                matches!(execute(&mut m, a), RunResult::Exited(0)),
                "{module} {jit}"
            );
            for (i, (name, pointer, dst)) in expected.into_iter().enumerate() {
                let local = !name.contains("mkgmtime");
                let forced = local && dst > 0;
                assert_eq!(
                    value(&m, DATA + 0x8000 + i as u32 * 8, name),
                    1_614_556_801 - if local { 32_400 } else { 0 } - if forced { 3600 } else { 0 },
                    "{module}!{name} {jit} {dst}"
                );
                assert_eq!(
                    tm(&m, pointer),
                    if forced {
                        [1, 0, 23, 28, 1, 121, 0, 58, 0]
                    } else {
                        [1, 0, 0, 1, 2, 121, 1, 59, 0]
                    }
                );
            }
            assert_eq!(m.cpu.read_u32(DATA + 0xf010).unwrap(), 71);
            assert_eq!(m.cpu.read_u32(DATA + 0xf014).unwrap(), 73);
        }
    }
}

// spec: CRT-LIB-28
#[test]
fn failure_keeps_only_prior_month_edits_and_does_not_invoke_invalid_handlers() {
    let cases = [
        ([0, 0, 0, 1, 12, 68, 91, 92, 0], 12), // year rejected before month
        ([0, 0, 0, 1, -1, 69, 91, 92, 0], 11), // normalized year becomes too old
        ([0, 0, 0, 1, 12, 139, 91, 92, 0], 0), // normalized year too large
        ([0, 0, 0, 1, 13, 138, 91, 92, 0], 1), // timestamp exceeds time32
        ([0, 0, -2_000_000_000, 100_000, 0, 120, 91, 92, 0], 0), // multiply overflow before cancellation
    ];
    for jit in [false, true] {
        for module in ["msvcr80.dll", "msvcr90.dll", "ucrtbase.dll"] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            // A handler that increments a cell makes unexpected validation visible.
            let mut handler = Asm::new(CODE + 0xf000);
            handler.bytes.extend_from_slice(&[0xff, 0x05]);
            handler
                .bytes
                .extend_from_slice(&(DATA + 0x7000).to_le_bytes());
            handler.ret();
            m.cpu.write(CODE + 0xf000, &handler.bytes).unwrap();
            invoke(
                &mut m,
                &mut a,
                module,
                "_set_invalid_parameter_handler",
                &[CODE + 0xf000],
                DATA + 0x7100,
            );
            for (i, (input, _)) in cases.iter().enumerate() {
                put(&mut m, DATA + i as u32 * 64, *input);
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_mktime32",
                    &[DATA + i as u32 * 64],
                    DATA + 0x8000 + i as u32 * 8,
                );
            }
            invoke(&mut m, &mut a, module, "_errno", &[], DATA + 0x7200);
            a.mov_eax_ptr_eax().mov_mem_eax(DATA + 0x7208);
            assert!(matches!(execute(&mut m, a), RunResult::Exited(0)));
            for (i, (mut input, month)) in cases.into_iter().enumerate() {
                input[4] = month;
                assert_eq!(value(&m, DATA + 0x8000 + i as u32 * 8, "_mktime32"), -1);
                assert_eq!(tm(&m, DATA + i as u32 * 64), input);
            }
            assert_eq!(m.cpu.read_u32(DATA + 0x7000).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA + 0x7208).unwrap(), 22);
        }
    }
}

// spec: CRT-LIB-28
#[test]
fn epoch_margins_and_signed_result_widths_follow_native_helpers() {
    let cases = [
        ("_mkgmtime32", [0, 0, 12, 31, 11, 69, 77, 78, 9], -43_200),
        ("_mkgmtime64", [59, 59, 11, 31, 11, 69, 77, 78, 9], -1),
        ("_mkgmtime64", [0, 0, 11, 31, 11, 69, 77, 78, 9], -1),
        ("_mktime64", [0, 0, 9, 1, 0, 70, 77, 78, -1], 0),
        ("_mktime64", [0, 0, 8, 1, 0, 70, 77, 78, 0], -1),
        (
            "_mkgmtime32",
            [7, 14, 3, 19, 0, 138, 77, 78, 0],
            i64::from(i32::MAX),
        ),
        ("_mkgmtime32", [8, 14, 3, 19, 0, 138, 77, 78, 0], -1),
        (
            "_mkgmtime64",
            [8, 14, 3, 19, 0, 138, 77, 78, 0],
            2_147_483_648,
        ),
    ];
    for jit in [false, true] {
        for module in ["msvcr80.dll", "msvcr90.dll", "ucrtbase.dll"] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            for (i, (name, input, _)) in cases.iter().enumerate() {
                put(&mut m, DATA + i as u32 * 64, *input);
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    name,
                    &[DATA + i as u32 * 64],
                    DATA + 0x8000 + i as u32 * 8,
                );
            }
            assert!(matches!(execute(&mut m, a), RunResult::Exited(0)));
            for (i, (name, _, expected)) in cases.iter().enumerate() {
                assert_eq!(
                    value(&m, DATA + 0x8000 + i as u32 * 8, name),
                    *expected,
                    "{module} {name} {i}"
                );
            }
        }
    }
}

// spec: CRT-LIB-28, PERS-ABI-1
#[test]
fn calendar_writes_stop_at_the_first_protected_field() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let p = DATA + 4096 - 24;
        put(&mut m, p, [61, -1, 24, 0, 2, 121, 99, 98, 97]);
        m.cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
        let mut a = Asm::new(CODE);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "_mkgmtime64",
            &[p],
            DATA + 0x8000,
        );
        assert!(matches!(execute(&mut m, a), RunResult::Failed(_)));
        assert_eq!(tm(&m, p), [1, 0, 0, 1, 2, 121, 99, 98, 97]);
    }
}

// spec: CRT-LIB-28, PERS-PROFILE-9
#[test]
fn null_validation_preserves_the_full_failure_width_after_returning_handler() {
    for jit in [false, true] {
        for returning in [false, true] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            if returning {
                m.cpu.write(CODE + 0xf000, &[0xc3]).unwrap();
                invoke(
                    &mut m,
                    &mut a,
                    "msvcr90.dll",
                    "_set_invalid_parameter_handler",
                    &[CODE + 0xf000],
                    DATA + 128,
                );
            }
            invoke(&mut m, &mut a, "msvcr90.dll", "_mktime64", &[0], DATA);
            let r = execute(&mut m, a);
            assert!(
                matches!(r, RunResult::Exited(code) if code == if returning {0} else {0xc0000417}),
                "{r:?}"
            );
            if returning {
                assert_eq!(value(&m, DATA, "_mktime64"), -1);
            }
        }
    }
}

// spec: CRT-LIB-28
#[test]
fn version_margins_and_year_before_month_validation_remain_observable() {
    for jit in [false, true] {
        for module in ["msvcr90.dll", "ucrtbase.dll"] {
            let modern = module == "ucrtbase.dll";
            let cases = [
                (
                    "_mkgmtime64",
                    [59, 59, 20, 1, 0, 1101, 9, 9, 0],
                    32_535_291_599,
                ),
                (
                    "_mkgmtime64",
                    [0, 0, 21, 1, 0, 1101, 9, 9, 0],
                    if modern { 32_535_291_600 } else { -1 },
                ),
                (
                    "_mktime64",
                    [59, 59, 16, 1, 0, 1101, 9, 9, 0],
                    32_535_244_799,
                ),
                (
                    "_mktime64",
                    [0, 0, 17, 1, 0, 1101, 9, 9, 0],
                    if modern { 32_535_244_800 } else { -1 },
                ),
                (
                    "_mkgmtime64",
                    [59, 59, 21, 19, 0, 1101, 9, 9, 0],
                    if modern { 32_536_850_399 } else { -1 },
                ),
                ("_mkgmtime64", [0, 0, 22, 19, 0, 1101, 9, 9, 0], -1),
                (
                    "_mktime64",
                    [59, 59, 16, 19, 0, 1101, 9, 9, 0],
                    if modern { 32_536_799_999 } else { -1 },
                ),
                ("_mktime64", [0, 0, 17, 19, 0, 1101, 9, 9, 0], -1),
                // A later negative month cannot rescue an initially invalid year.
                (
                    "_mkgmtime64",
                    [0, 0, 0, 1, -24, 1102, 9, 9, 0],
                    if modern { 32_503_680_000 } else { -1 },
                ),
                ("_mkgmtime64", [0, 0, 0, 1, -36, 1103, 9, 9, 0], -1),
            ];
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            for (i, (name, input, _)) in cases.iter().enumerate() {
                put(&mut m, DATA + i as u32 * 64, *input);
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    name,
                    &[DATA + i as u32 * 64],
                    DATA + 0x8000 + i as u32 * 8,
                );
            }
            assert!(matches!(execute(&mut m, a), RunResult::Exited(0)));
            for (i, (name, _, expected)) in cases.iter().enumerate() {
                assert_eq!(
                    value(&m, DATA + 0x8000 + i as u32 * 8, name),
                    *expected,
                    "{module} {i}"
                );
            }
            assert_eq!(tm(&m, DATA + 9 * 64)[4], -36);
            if !modern {
                assert_eq!(tm(&m, DATA + 8 * 64)[4], -24);
            }
        }
    }
}

// spec: CRT-LIB-28, PERS-ABI-1
#[test]
fn invalid_year_returns_without_reading_earlier_fields_or_output_tail() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let p = DATA + 65536 - 24;
        m.cpu.write_u32(p + 20, 68).unwrap();
        // The output tail is unmapped and the input itself is not writable.
        m.cpu.protect(DATA + 61440, 4096, Prot::READ).unwrap();
        // An isolated year page leaves the preceding fields unmapped too.
        let year = 0x800000;
        m.cpu.map(year, 4096, Prot::READ).unwrap();
        m.cpu.write_u32(year, 68).unwrap();
        let mut a = Asm::new(CODE);
        invoke(&mut m, &mut a, "msvcr90.dll", "_mkgmtime64", &[p], DATA);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "_mkgmtime64",
            &[year - 20],
            DATA + 16,
        );
        assert!(matches!(execute(&mut m, a), RunResult::Exited(0)));
        assert_eq!(value(&m, DATA, "_mkgmtime64"), -1);
        assert_eq!(value(&m, DATA + 16, "_mkgmtime64"), -1);
        assert_eq!(m.cpu.read_u32(p + 20).unwrap(), 68);
    }
}
