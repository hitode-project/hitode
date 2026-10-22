use super::*;

// spec: CRT-LIB-16
#[test]
fn signal_validation_preserves_reserved_errors_and_uses_version_policy() {
    for jit in [false, true] {
        for module in [
            "msvcr70.dll",
            "msvcr80.dll",
            "msvcr90.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            m.cpu
                .write(CALLBACK, &[0xff, 0x05, 0x00, 0x09, 0x41, 0x00, 0xc3])
                .unwrap();
            if signature::lookup(module, "_set_invalid_parameter_handler").is_some() {
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "_set_invalid_parameter_handler",
                    &[CALLBACK],
                    DATA + 0x800,
                );
            }
            invoke(&mut m, &mut a, module, "signal", &[15, 1], DATA + 0x804);
            for (i, (signal, action)) in [
                (99, 0),
                (15, 3),
                (15, 4),
                (1, 0),
                (3, 0),
                (13, 0),
                (16, 0),
                (17, 0),
            ]
            .into_iter()
            .enumerate()
            {
                invoke(&mut m, &mut a, module, "_errno", &[], DATA + 0x808);
                a.bytes.extend_from_slice(&[0xc7, 0, 71, 0, 0, 0]);
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    "signal",
                    &[signal, action],
                    RESULTS + i as u32 * 8,
                );
                a.mov_eax_mem(DATA + 0x808)
                    .mov_eax_ptr_eax()
                    .mov_mem_eax(RESULTS + i as u32 * 8 + 4);
            }
            invoke(&mut m, &mut a, module, "signal", &[15, 2], RESULTS + 64);
            invoke(&mut m, &mut a, module, "raise", &[99], RESULTS + 68);
            a.mov_eax_mem(DATA + 0x808)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 72);
            let result = finish(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            for i in 0..8 {
                assert_eq!(m.cpu.read_u32(RESULTS + i * 8).unwrap(), u32::MAX);
                assert_eq!(
                    m.cpu.read_u32(RESULTS + i * 8 + 4).unwrap(),
                    if i < 3 { 22 } else { 71 }
                );
            }
            assert_eq!(m.cpu.read_u32(RESULTS + 64).unwrap(), 1);
            assert_eq!(m.cpu.read_u32(RESULTS + 68).unwrap(), u32::MAX);
            assert_eq!(m.cpu.read_u32(RESULTS + 72).unwrap(), 22);
            let count = match module {
                "msvcr80.dll" | "msvcr90.dll" => 4,
                "msvcr70.dll" => 0,
                _ => 1,
            };
            assert_eq!(
                m.cpu.read_u32(DATA + 0x900).unwrap(),
                count,
                "{module} {jit}"
            );
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-15, CRT-LIB-16
#[test]
fn abort_delivers_sigabrt_then_terminates_unless_the_handler_exits() {
    for jit in [false, true] {
        for module in [
            "msvcr80.dll",
            "msvcr90.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
            "vcruntime140.dll",
        ] {
            for action in [0, 1, 2] {
                let mut m = machine(jit);
                let owner = if module == "vcruntime140.dll" {
                    "ucrtbase.dll"
                } else {
                    module
                };
                let mut callback = Asm::new(CALLBACK);
                callback.mov_eax_esp(4).mov_mem_eax(DATA + 0x900);
                if action == 2 {
                    callback
                        .push_imm(77)
                        .call(m.resolve_import("kernel32.dll", "ExitProcess").unwrap());
                }
                callback.ret();
                m.cpu.write(CALLBACK, &callback.bytes).unwrap();
                let mut a = Asm::new(CODE);
                invoke(
                    &mut m,
                    &mut a,
                    owner,
                    "signal",
                    &[22, if action == 1 { 1 } else { CALLBACK }],
                    RESULTS,
                );
                invoke(
                    &mut m,
                    &mut a,
                    module,
                    if module == "vcruntime140.dll" {
                        "__std_terminate"
                    } else {
                        "abort"
                    },
                    &[],
                    RESULTS + 4,
                );
                let result = finish(&mut m, a);
                assert!(
                    matches!(result, RunResult::Exited(status) if status == if action == 2 {77} else {3}),
                    "{module} {jit} {action}: {result:?}"
                );
                assert_eq!(
                    m.cpu.read_u32(DATA + 0x900).unwrap(),
                    if action == 1 { 0 } else { 22 }
                );
                no_gaps(&m);
            }
        }
    }
}

// spec: CRT-LIB-16, CRT-LIB-17, EXC-CRT-3
#[test]
fn signal_callbacks_can_longjmp_without_running_abandoned_continuations() {
    for jit in [false, true] {
        for fp in [false, true] {
            let mut m = machine(jit);
            let module = "msvcr90.dll";
            let buffer = DATA + 0x1000;
            let resume = CODE + 0x1000;
            let mut callback = Asm::new(CALLBACK);
            callback
                .push_imm(9)
                .push_imm(buffer)
                .call(m.resolve_import(module, "longjmp").unwrap())
                .bytes
                .push(0xcc);
            m.cpu.write(CALLBACK, &callback.bytes).unwrap();
            let mut a = Asm::new(CODE);
            invoke(&mut m, &mut a, module, "__fpecode", &[], DATA + 0x800);
            a.bytes.extend_from_slice(&[0xc7, 0, 77, 0, 0, 0]);
            invoke(&mut m, &mut a, module, "__pxcptinfoptrs", &[], DATA + 0x804);
            a.bytes
                .extend_from_slice(&[0xc7, 0, 0x78, 0x56, 0x34, 0x12]);
            a.push_imm(0)
                .push_imm(buffer)
                .call(m.resolve_import(module, "_setjmp3").unwrap())
                .add_esp(8)
                .mov_mem_eax(RESULTS);
            a.bytes.extend_from_slice(&[0x85, 0xc0, 0x0f, 0x85]);
            let relative = resume.wrapping_sub(CODE + a.bytes.len() as u32 + 4);
            a.bytes.extend_from_slice(&relative.to_le_bytes());
            invoke(
                &mut m,
                &mut a,
                module,
                "signal",
                &[if fp { 8 } else { 22 }, CALLBACK],
                RESULTS + 4,
            );
            if fp {
                invoke(&mut m, &mut a, module, "raise", &[8], RESULTS + 8);
            } else {
                invoke(&mut m, &mut a, module, "abort", &[], RESULTS + 8);
            }
            a.bytes.push(0xcc);
            let mut resumed = Asm::new(resume);
            invoke(
                &mut m,
                &mut resumed,
                module,
                "signal",
                &[if fp { 8 } else { 22 }, 2],
                RESULTS + 12,
            );
            resumed
                .mov_eax_mem(DATA + 0x800)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 16);
            resumed
                .mov_eax_mem(DATA + 0x804)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 20);
            resumed.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            m.cpu.write(resume, &resumed.bytes).unwrap();
            let result = finish(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{jit} {fp}: {result:?}"
            );
            assert_eq!(m.cpu.read_u32(RESULTS).unwrap(), 9);
            assert_eq!(m.cpu.read_u32(RESULTS + 12).unwrap(), 0);
            assert_eq!(
                m.cpu.read_u32(RESULTS + 16).unwrap(),
                if fp { 0x8c } else { 77 }
            );
            assert_eq!(
                m.cpu.read_u32(RESULTS + 20).unwrap(),
                if fp { 0 } else { 0x12345678 }
            );
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-16, CRT-LIB-17
#[test]
fn filters_keep_per_fault_actions_and_ucrt_sse_entries() {
    for jit in [false, true] {
        for (module, filter, dll_filter) in [
            ("msvcr90.dll", "_XcptFilter", "__CppXcptFilter"),
            ("ucrtbase.dll", "_seh_filter_exe", "_seh_filter_dll"),
            (
                "api-ms-win-crt-runtime-l1-1-0.dll",
                "_seh_filter_exe",
                "_seh_filter_dll",
            ),
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            let mut callback = Asm::new(CALLBACK);
            callback.mov_eax_esp(4).mov_mem_eax(DATA + 0x900);
            callback.mov_eax_esp(8).mov_mem_eax(DATA + 0x904);
            callback.ret();
            m.cpu.write(CALLBACK, &callback.bytes).unwrap();
            let mut expected = Vec::new();
            let operations = [
                ("signal", vec![4, CALLBACK], 0),
                (filter, vec![0xc000001d, DATA], u32::MAX),
                (filter, vec![0xc000001d, DATA], 0),
                (filter, vec![0xc0000096, DATA], u32::MAX),
                ("signal", vec![11, 5], 0),
                (filter, vec![0xc0000005, DATA], 1),
                (filter, vec![0xc0000005, DATA], 0),
                ("signal", vec![11, 1], 0),
                (filter, vec![0xc0000005, DATA], u32::MAX),
                (filter, vec![0xc0000005, DATA], u32::MAX),
                (filter, vec![0xc0000094, DATA], 0),
                ("signal", vec![8, CALLBACK], 0),
                (dll_filter, vec![0xc00002b4, DATA], 0),
                (
                    filter,
                    vec![0xc00002b4, DATA],
                    if module == "msvcr90.dll" { 0 } else { u32::MAX },
                ),
                (
                    "signal",
                    vec![8, CALLBACK],
                    if module == "msvcr90.dll" { CALLBACK } else { 0 },
                ),
                (
                    filter,
                    vec![0xc00002b5, DATA],
                    if module == "msvcr90.dll" { 0 } else { u32::MAX },
                ),
            ];
            for (i, (name, args, result)) in operations.into_iter().enumerate() {
                invoke(&mut m, &mut a, module, name, &args, RESULTS + i as u32 * 4);
                expected.push(result);
            }
            assert!(
                matches!(finish(&mut m, a), RunResult::Exited(0)),
                "{module} {jit}"
            );
            for (i, result) in expected.into_iter().enumerate() {
                assert_eq!(
                    m.cpu.read_u32(RESULTS + i as u32 * 4).unwrap(),
                    result,
                    "{module} {jit} {i}"
                );
            }
            assert_eq!(
                m.cpu.read_u32(DATA + 0x900).unwrap(),
                if module == "msvcr90.dll" { 4 } else { 8 }
            );
            if module != "msvcr90.dll" {
                assert_eq!(m.cpu.read_u32(DATA + 0x904).unwrap(), 0x8d);
            }
            no_gaps(&m);
        }
    }
}
