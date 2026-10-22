use super::*;
use adv_abi::signature;

fn finish(m: &mut Machine<JitCpu>, mut a: Asm) -> RunResult {
    a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
    run(m, a)
}
fn no_gaps(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}
fn edit(name: &str) -> bool {
    let name = name.strip_prefix("_o_").unwrap_or(name);
    [
        "_strrev",
        "_wcsrev",
        "_strset",
        "_wcsset",
        "_strnset",
        "_wcsnset",
        "_strset_s",
        "_wcsset_s",
        "_strnset_s",
        "_wcsnset_s",
    ]
    .contains(&name)
}

// spec: CRT-LIB-20, CRT-LIB-21
#[test]
fn every_edit_projection_uses_code_units_and_preserves_errors() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| edit(s.name))
        .collect();
    assert_eq!(declarations.len(), 110);
    let modules: std::collections::BTreeSet<_> = declarations.iter().map(|s| s.module).collect();
    for jit in [false, true] {
        for module in &modules {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            let mut expectations = Vec::new();
            for (i, s) in declarations
                .iter()
                .filter(|s| s.module == *module)
                .enumerate()
            {
                let name = s.name.strip_prefix("_o_").unwrap_or(s.name);
                let wide = name.starts_with("_wcs");
                let secure = name.ends_with("_s");
                let bounded = name.contains("nset");
                let mut expected = if wide {
                    vec![0xd800, 0x3042, 0xdc00, 0x41]
                } else {
                    vec![0x41, 0x81, 0x61, 0x42]
                };
                let destination = DATA + 128 + i as u32 * 64;
                let output = DATA + 0x2000 + i as u32 * 16;
                text(&mut m, destination, &expected, wide);
                let value = if wide { 0x12345 } else { 0x1fe };
                let mut args = vec![destination];
                if secure {
                    args.push(5);
                }
                if !name.ends_with("rev") {
                    args.push(value);
                }
                if bounded {
                    args.push(2);
                }
                let owner = if s.module.starts_with("api-ms-win-crt-") {
                    "ucrtbase.dll"
                } else {
                    s.module
                };
                invoke(&mut m, &mut a, owner, "_errno", &[], DATA + 0x2f00);
                a.bytes.extend_from_slice(&[0xc7, 0, 71, 0, 0, 0]);
                a.push_imm(73)
                    .call(m.resolve_import("kernel32.dll", "SetLastError").unwrap());
                invoke(&mut m, &mut a, s.module, s.name, &args, output);
                a.mov_eax_mem(DATA + 0x2f00)
                    .mov_eax_ptr_eax()
                    .mov_mem_eax(output + 4);
                a.call(m.resolve_import("kernel32.dll", "GetLastError").unwrap())
                    .mov_mem_eax(output + 8);
                if name.ends_with("rev") {
                    expected.reverse();
                } else {
                    for unit in expected.iter_mut().take(if bounded { 2 } else { 4 }) {
                        *unit = if wide {
                            value as u16
                        } else {
                            u16::from(value as u8)
                        };
                    }
                }
                expected.push(0);
                expectations.push((s.name, destination, output, wide, secure, expected));
            }
            let result = finish(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            for (name, destination, output, wide, secure, expected) in expectations {
                assert_eq!(
                    read(&m, destination, 5, wide),
                    expected,
                    "{module}!{name} {jit}"
                );
                assert_eq!(
                    m.cpu.read_u32(output).unwrap(),
                    if secure { 0 } else { destination }
                );
                assert_eq!(m.cpu.read_u32(output + 4).unwrap(), 71);
                assert_eq!(m.cpu.read_u32(output + 8).unwrap(), 73);
            }
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-21
#[test]
fn secure_edits_keep_native_partial_writes_and_validate_the_original_suffix() {
    for jit in [false, true] {
        for module in [
            "msvcr80.dll",
            "msvcr90.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-string-l1-1-0.dll",
        ] {
            for wide in [false, true] {
                let mut m = machine(jit);
                let owner = if module.starts_with("api-ms-win-crt-") {
                    "ucrtbase.dll"
                } else {
                    module
                };
                let full = if wide { "_wcsset_s" } else { "_strset_s" };
                let bounded = if wide { "_wcsnset_s" } else { "_strnset_s" };
                let mut a = Asm::new(CODE);
                let callback = CODE + 3072;
                let mut handler = Asm::new(callback);
                handler.bytes.extend_from_slice(&[0xff, 0x05]);
                handler
                    .bytes
                    .extend_from_slice(&(DATA + 0x3000).to_le_bytes());
                handler.ret();
                m.cpu.write(callback, &handler.bytes).unwrap();
                invoke(
                    &mut m,
                    &mut a,
                    owner,
                    "_set_invalid_parameter_handler",
                    &[callback],
                    DATA + 0x2000,
                );
                for (i, (name, capacity, value, count, expected_result)) in [
                    (bounded, 3, 88, Some(1), 22),
                    (full, 3, 88, None, 22),
                    (bounded, 3, 88, Some(0), 22),
                    (full, 3, 0, None, 22),
                    (bounded, 5, 88, Some(100), 0),
                    (bounded, 0, 88, Some(0), 22),
                ]
                .into_iter()
                .enumerate()
                {
                    let destination = DATA + 128 + i as u32 * 64;
                    text(&mut m, destination, &[97, 98, 99, 100], wide);
                    let mut args = vec![destination, capacity, value];
                    if let Some(count) = count {
                        args.push(count);
                    }
                    invoke(
                        &mut m,
                        &mut a,
                        module,
                        name,
                        &args,
                        DATA + 0x2004 + i as u32 * 8,
                    );
                    invoke(&mut m, &mut a, owner, "_errno", &[], DATA + 0x2100);
                    a.mov_eax_ptr_eax()
                        .mov_mem_eax(DATA + 0x2008 + i as u32 * 8);
                    m.cpu
                        .write_u32(DATA + 0x2200 + i as u32 * 4, expected_result)
                        .unwrap();
                }
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    bounded,
                    &[0, 0, 88, 0],
                    DATA + 0x2050,
                );
                assert!(
                    matches!(finish(&mut m, a), RunResult::Exited(0)),
                    "{module} {jit} {wide}"
                );
                for (i, expected) in [
                    [0, 98, 99, 100, 0],
                    [0, 88, 99, 100, 0],
                    [0, 98, 99, 100, 0],
                    [0, 0, 99, 100, 0],
                    [88, 88, 88, 88, 0],
                    [97, 98, 99, 100, 0],
                ]
                .into_iter()
                .enumerate()
                {
                    assert_eq!(
                        read(&m, DATA + 128 + i as u32 * 64, 5, wide),
                        expected,
                        "{module} {jit} {wide} {i}"
                    );
                    assert_eq!(
                        m.cpu.read_u32(DATA + 0x2004 + i as u32 * 8).unwrap(),
                        m.cpu.read_u32(DATA + 0x2200 + i as u32 * 4).unwrap()
                    );
                    assert_eq!(m.cpu.read_u32(DATA + 0x2008 + i as u32 * 8).unwrap(), 22);
                }
                assert_eq!(m.cpu.read_u32(DATA + 0x2050).unwrap(), 0);
                assert_eq!(m.cpu.read_u32(DATA + 0x3000).unwrap(), 5);
                no_gaps(&m);
            }
        }
    }
}

// spec: CRT-LIB-20, CRT-LIB-21
#[test]
fn edits_stop_at_guarded_counts_and_terminators_and_report_inaccessible_writes() {
    for jit in [false, true] {
        for wide in [false, true] {
            for operation in [
                "bounded",
                "bounded_zero",
                "secure_empty",
                "reverse",
                "read_only",
            ] {
                let mut m = machine(jit);
                let width = if wide { 2 } else { 1 };
                let destination = DATA + 4096 - width;
                m.cpu.write(destination, &0u16.to_le_bytes()).unwrap();
                m.cpu.unmap(DATA + 4096, 4096).unwrap();
                let (name, args) = match operation {
                    "bounded_zero" => (if wide { "_wcsnset" } else { "_strnset" }, vec![0, 88, 0]),
                    "bounded" => {
                        if wide {
                            m.cpu.write(destination, &97u16.to_le_bytes()).unwrap();
                        } else {
                            m.cpu.write(destination, &[97]).unwrap();
                        }
                        (
                            if wide { "_wcsnset" } else { "_strnset" },
                            vec![destination, 88, 1],
                        )
                    }
                    "secure_empty" => (
                        if wide { "_wcsnset_s" } else { "_strnset_s" },
                        vec![destination, 100, 88, 0],
                    ),
                    "reverse" => (if wide { "_wcsrev" } else { "_strrev" }, vec![destination]),
                    _ => {
                        if wide {
                            m.cpu
                                .write(destination - width, &97u16.to_le_bytes())
                                .unwrap();
                        } else {
                            m.cpu.write(destination - width, &[97]).unwrap();
                        }
                        m.cpu.protect(DATA, 4096, Prot::READ).unwrap();
                        (
                            if wide { "_wcsset" } else { "_strset" },
                            vec![destination - width, 88],
                        )
                    }
                };
                let mut a = Asm::new(CODE);
                invoke(&mut m, &mut a, "msvcr90.dll", name, &args, DATA + 0x2000);
                let result = finish(&mut m, a);
                if operation == "read_only" {
                    assert!(
                        matches!(result, RunResult::Failed(_)),
                        "{jit} {wide}: {result:?}"
                    );
                } else {
                    assert!(
                        matches!(result, RunResult::Exited(0)),
                        "{jit} {wide} {operation}: {result:?}"
                    );
                }
                no_gaps(&m);
            }
        }
    }
}

// spec: CRT-LIB-21, PERS-PROFILE-9
#[test]
fn secure_edits_without_a_validation_handler_use_crt_parameter_termination() {
    for jit in [false, true] {
        for (module, name, args) in [
            ("msvcr90.dll", "_strset_s", vec![0, 1, 88]),
            ("ucrtbase.dll", "_wcsnset_s", vec![0, 1, 88, 0]),
            ("ucrtbase.dll", "_o__strset_s", vec![0, 1, 88]),
            ("ucrtbase.dll", "_o__wcsnset_s", vec![0, 1, 88, 0]),
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            invoke(&mut m, &mut a, module, name, &args, DATA + 0x2000);
            assert!(
                matches!(finish(&mut m, a), RunResult::Exited(0xc0000417)),
                "{module}!{name} {jit}"
            );
            no_gaps(&m);
        }
    }
}
