//! Authored x86 calls exercise every cataloged public process form and the
//! shared error cells on both the interpreter and JIT.
use adv_abi::{Registry, signature};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};
use std::collections::BTreeSet;

const CODE: u32 = 0x400000;
const CALLBACK: u32 = CODE + 0xf000;
const DATA: u32 = 0x410000;
const RESULTS: u32 = DATA + 1024;

fn process(name: &str) -> bool {
    matches!(
        name,
        "system" | "_wsystem" | "_popen" | "_wpopen" | "_pclose"
    ) || ["_exec", "_wexec", "_spawn", "_wspawn"]
        .iter()
        .any(|prefix| name.starts_with(prefix) && !name.ends_with("table"))
}

fn machine(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/crt-process",
            "fixture.exe",
            "/nonexistent/crt-process-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 65536, Prot::RX).unwrap();
    machine.cpu.map(DATA, 65536, Prot::RW).unwrap();
    machine.cpu.write(DATA, b"missing-command\0").unwrap();
    machine
        .cpu
        .write(
            DATA + 64,
            &"missing-command\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    machine.cpu.write(DATA + 128, b"rb\0").unwrap();
    machine
        .cpu
        .write(DATA + 160, &[b'r', 0, b'b', 0, 0, 0])
        .unwrap();
    machine.cpu.write_u32(DATA + 192, DATA).unwrap();
    machine.cpu.write_u32(DATA + 200, DATA + 64).unwrap();
    machine
}

fn invoke(
    m: &mut Machine<JitCpu>,
    code: &mut Asm,
    module: &str,
    name: &str,
    args: &[u32],
    output: u32,
) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(m.resolve_import(module, name).unwrap());
    if !args.is_empty() {
        code.add_esp(args.len() as u8 * 4);
    }
    code.mov_mem_eax(output);
}

fn finish(m: &mut Machine<JitCpu>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("CRT process fixture", CODE, &[], 65536).unwrap();
    m.run_virtual(5_000_000)
}

fn no_gaps(m: &Machine<JitCpu>) {
    if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
        assert_eq!(d.stub_calls, 0);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }
}

// spec: PERS-PROFILE-8, PERS-PROFILE-9, PERS-ABI-1
#[test]
fn all_public_process_exports_fail_normally_in_both_engines() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| process(s.name))
        .collect();
    let modules: BTreeSet<_> = declarations.iter().map(|s| s.module).collect();
    assert!(modules.len() >= 13);
    for jit in [false, true] {
        for module in &modules {
            let mut m = machine(jit);
            let mut code = Asm::new(CODE);
            let owner = if module.starts_with("api-ms-win-crt-") {
                "ucrtbase.dll"
            } else {
                module
            };
            let errno = m.resolve_import(owner, "_errno").unwrap();
            let doserrno = m.resolve_import(owner, "__doserrno").unwrap();
            let last_error = m.resolve_import("kernel32.dll", "SetLastError").unwrap();
            code.push_imm(73).call(last_error);
            let mut expected = Vec::new();
            for declaration in declarations.iter().filter(|s| s.module == *module) {
                let name = declaration.name;
                let wide = name.starts_with("_w");
                let path = DATA + if wide { 64 } else { 0 };
                let vector = DATA + if wide { 200 } else { 192 };
                let mut args = if matches!(name, "system" | "_wsystem") {
                    vec![path]
                } else if name == "_pclose" {
                    vec![0x12345678]
                } else if name.ends_with("popen") {
                    vec![path, DATA + if wide { 160 } else { 128 }]
                } else {
                    let mut args = if name.contains("spawn") {
                        vec![0]
                    } else {
                        vec![]
                    };
                    args.extend([
                        path,
                        if name.contains("execv") || name.contains("spawnv") {
                            vector
                        } else {
                            path
                        },
                    ]);
                    // Both list forms (NULL sentinel) and vector e forms (NULL
                    // environment) include their caller-owned trailing word.
                    args.push(0);
                    if name.contains('l') && name.ends_with('e') {
                        args.push(0); // environment after the list sentinel
                    }
                    args
                };
                let at = RESULTS + expected.len() as u32 * 12;
                // Seed both cells before each call, including pclose which only
                // assigns errno. This also detects accidental module aliasing.
                code.call(errno)
                    .bytes
                    .extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
                code.call(doserrno)
                    .bytes
                    .extend_from_slice(&[0xc7, 0x00, 83, 0, 0, 0]);
                invoke(&mut m, &mut code, module, name, &args, at);
                code.call(errno).mov_eax_ptr_eax().mov_mem_eax(at + 4);
                code.call(doserrno).mov_eax_ptr_eax().mov_mem_eax(at + 8);
                expected.push((
                    name,
                    if name.ends_with("popen") { 0 } else { u32::MAX },
                    if name == "_pclose" { 9 } else { 2 },
                    if name == "_pclose" { 83 } else { 2 },
                ));
                if matches!(name, "system" | "_wsystem") {
                    args[0] = 0;
                    let at = RESULTS + expected.len() as u32 * 12;
                    invoke(&mut m, &mut code, module, name, &args, at);
                    code.call(errno).mov_eax_ptr_eax().mov_mem_eax(at + 4);
                    code.call(doserrno).mov_eax_ptr_eax().mov_mem_eax(at + 8);
                    expected.push((name, 0, 2, 2));
                }
            }
            let get_error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
            code.call(get_error).mov_mem_eax(DATA + 512);
            let result = finish(&mut m, code);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module}, jit={jit}: {result:?}"
            );
            for (i, (name, result, errno, dos)) in expected.iter().enumerate() {
                let at = RESULTS + i as u32 * 12;
                assert_eq!(m.cpu.read_u32(at).unwrap(), *result, "{module}!{name}");
                assert_eq!(m.cpu.read_u32(at + 4).unwrap(), *errno, "{module}!{name}");
                assert_eq!(m.cpu.read_u32(at + 8).unwrap(), *dos, "{module}!{name}");
            }
            assert_eq!(m.cpu.read_u32(DATA + 512).unwrap(), 73);
            no_gaps(&m);
        }
    }
}

// spec: PERS-PROFILE-8
#[test]
fn legacy_validation_and_modern_returning_handlers_preserve_failure_values() {
    for jit in [false, true] {
        for module in [
            "msvcrt20.dll",
            "msvcrt40.dll",
            "msvcr70.dll",
            "msvcr71.dll",
            "msvcr80.dll",
            "msvcr90.dll",
            "msvcr100.dll",
            "msvcr110.dll",
            "msvcr120.dll",
            "ucrtbase.dll",
        ] {
            let mut m = machine(jit);
            let mut code = Asm::new(CODE);
            let modern =
                !["msvcrt20.dll", "msvcrt40.dll", "msvcr70.dll", "msvcr71.dll"].contains(&module);
            let callback = [0xff, 0x05, 0x00, 0x03, 0x41, 0x00, 0xc3]; // inc [DATA+0x300]; ret
            m.cpu.write(CALLBACK, &callback).unwrap();
            if modern {
                invoke(
                    &mut m,
                    &mut code,
                    module,
                    "_set_invalid_parameter_handler",
                    &[CALLBACK],
                    DATA + 520,
                );
            }
            for (i, (name, args, expected)) in [
                ("_spawnv", vec![5, DATA, DATA + 192], u32::MAX),
                ("_execv", vec![0, DATA + 192], u32::MAX),
                ("_execv", vec![0, 0x12345678], u32::MAX),
                ("_execv", vec![DATA + 15, 0x12345678], u32::MAX),
                ("_execl", vec![DATA, 0, 0], u32::MAX),
                ("_popen", vec![0, DATA + 128], 0),
                ("_popen", vec![DATA, DATA + 64], 0),
                ("_pclose", vec![0], u32::MAX),
            ]
            .into_iter()
            .enumerate()
            {
                invoke(
                    &mut m,
                    &mut code,
                    module,
                    name,
                    &args,
                    RESULTS + i as u32 * 8,
                );
                let errno = m.resolve_import(module, "_errno").unwrap();
                code.call(errno)
                    .mov_eax_ptr_eax()
                    .mov_mem_eax(RESULTS + i as u32 * 8 + 4);
                m.cpu
                    .write_u32(DATA + 600 + i as u32 * 4, expected)
                    .unwrap();
            }
            let result = finish(&mut m, code);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module}: {result:?}"
            );
            for i in 0..8 {
                assert_eq!(
                    m.cpu.read_u32(RESULTS + i * 8).unwrap(),
                    m.cpu.read_u32(DATA + 600 + i * 4).unwrap()
                );
                assert_eq!(m.cpu.read_u32(RESULTS + i * 8 + 4).unwrap(), 22);
            }
            assert_eq!(
                m.cpu.read_u32(DATA + 0x300).unwrap(),
                if modern { 8 } else { 0 }
            );
            no_gaps(&m);
        }
    }
}

// spec: PERS-PROFILE-9
#[test]
fn null_error_outputs_invoke_the_module_handler_without_writing_a_pointer() {
    for jit in [false, true] {
        for module in [
            "msvcr80.dll",
            "msvcr90.dll",
            "msvcr100.dll",
            "msvcr110.dll",
            "msvcr120.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
        ] {
            let mut m = machine(jit);
            let mut code = Asm::new(CODE);
            m.cpu
                .write(CALLBACK, &[0xff, 0x05, 0x00, 0x03, 0x41, 0x00, 0xc3])
                .unwrap();
            invoke(
                &mut m,
                &mut code,
                module,
                "_set_invalid_parameter_handler",
                &[CALLBACK],
                RESULTS,
            );
            invoke(
                &mut m,
                &mut code,
                module,
                "_get_invalid_parameter_handler",
                &[],
                RESULTS + 4,
            );
            invoke(&mut m, &mut code, module, "_get_errno", &[0], RESULTS + 8);
            invoke(
                &mut m,
                &mut code,
                module,
                "_get_doserrno",
                &[0],
                RESULTS + 12,
            );
            let errno = m.resolve_import(module, "_errno").unwrap();
            code.call(errno).mov_eax_ptr_eax().mov_mem_eax(RESULTS + 16);
            assert!(matches!(finish(&mut m, code), RunResult::Exited(0)));
            assert_eq!(m.cpu.read_u32(RESULTS).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(RESULTS + 4).unwrap(), CALLBACK);
            for offset in [8, 12, 16] {
                assert_eq!(m.cpu.read_u32(RESULTS + offset).unwrap(), 22);
            }
            assert_eq!(m.cpu.read_u32(DATA + 0x300).unwrap(), 2);
            no_gaps(&m);
        }
    }
}

// spec: PERS-PROFILE-8, PERS-PROFILE-9
#[test]
fn modern_invalid_parameters_without_handler_terminate_with_crt_parameter_status() {
    for jit in [false, true] {
        for (name, args) in [
            ("_spawnv", vec![5, DATA, DATA + 192]),
            ("_popen", vec![0, DATA + 128]),
            ("_pclose", vec![0]),
            ("_get_errno", vec![0]),
            ("_get_doserrno", vec![0]),
        ] {
            let mut m = machine(jit);
            let mut code = Asm::new(CODE);
            invoke(&mut m, &mut code, "ucrtbase.dll", name, &args, RESULTS);
            assert!(
                matches!(finish(&mut m, code), RunResult::Exited(0xc0000417)),
                "{name}, jit={jit}"
            );
            no_gaps(&m);
        }
    }
}

// spec: PERS-PROFILE-9
#[test]
fn error_accessors_share_cells_and_api_sets_without_sharing_other_crts() {
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
        "api-ms-win-crt-runtime-l1-1-0.dll",
    ];
    for jit in [false, true] {
        let mut m = machine(jit);
        let mut code = Asm::new(CODE);
        for (i, module) in modules.iter().enumerate() {
            let at = RESULTS + i as u32 * 32;
            invoke(&mut m, &mut code, module, "_errno", &[], at);
            code.bytes.extend_from_slice(&[0xc7, 0x00, 71, 0, 0, 0]);
            invoke(&mut m, &mut code, module, "__doserrno", &[], at + 4);
            code.bytes.extend_from_slice(&[0xc7, 0x00, 83, 0, 0, 0]);
            if signature::lookup(module, "_get_errno").is_some() {
                invoke(&mut m, &mut code, module, "_get_errno", &[at + 8], at + 16);
                invoke(
                    &mut m,
                    &mut code,
                    module,
                    "_get_doserrno",
                    &[at + 12],
                    at + 20,
                );
                invoke(
                    &mut m,
                    &mut code,
                    module,
                    "_set_errno",
                    &[0x80000001],
                    at + 24,
                );
                invoke(
                    &mut m,
                    &mut code,
                    module,
                    "_set_doserrno",
                    &[0xfedcba98],
                    at + 28,
                );
            }
        }
        assert!(matches!(finish(&mut m, code), RunResult::Exited(0)));
        let mut pointers = BTreeSet::new();
        for (i, module) in modules.iter().enumerate() {
            let at = RESULTS + i as u32 * 32;
            let pointer = m.cpu.read_u32(at).unwrap();
            assert_eq!(m.cpu.read_u32(at + 4).unwrap(), pointer + 4);
            if module.starts_with("api-ms-win-crt-") {
                assert_eq!(pointer, m.cpu.read_u32(RESULTS + 10 * 32).unwrap());
            } else {
                assert!(pointers.insert(pointer), "distinct CRT module: {module}");
            }
            if signature::lookup(module, "_get_errno").is_some() {
                assert_eq!(m.cpu.read_u32(at + 8).unwrap(), 71);
                assert_eq!(m.cpu.read_u32(at + 12).unwrap(), 83);
                for offset in [16, 20, 24, 28] {
                    assert_eq!(m.cpu.read_u32(at + offset).unwrap(), 0);
                }
                assert_eq!(m.cpu.read_u32(pointer).unwrap(), 0x80000001);
                assert_eq!(m.cpu.read_u32(pointer + 4).unwrap(), 0xfedcba98);
            }
        }
        no_gaps(&m);
    }
}

// spec: PERS-PROFILE-9
#[test]
fn error_cells_remain_independent_while_another_guest_thread_runs() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let mut entry = Asm::new(CODE);
        let mut worker = Asm::new(CODE + 0x1000);
        for (i, code) in [&mut entry, &mut worker].into_iter().enumerate() {
            let pointer = m.resolve_import("ucrtbase.dll", "_errno").unwrap();
            code.call(pointer).mov_mem_eax(RESULTS + i as u32 * 4);
            code.bytes.extend_from_slice(&[0xc7, 0x00]);
            code.bytes
                .extend_from_slice(&(71u32 + i as u32).to_le_bytes());
            let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
            code.push_imm(0).call(sleep);
            code.call(pointer)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 16 + i as u32 * 4);
            code.bytes.extend_from_slice(&[0x31, 0xc0]);
            code.ret();
        }
        m.cpu.write(CODE, &entry.bytes).unwrap();
        m.cpu.write(CODE + 0x1000, &worker.bytes).unwrap();
        m.spawn("CRT errno first thread", CODE, &[], 65536).unwrap();
        m.spawn("CRT errno second thread", CODE + 0x1000, &[], 65536)
            .unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert_ne!(
            m.cpu.read_u32(RESULTS).unwrap(),
            m.cpu.read_u32(RESULTS + 4).unwrap()
        );
        assert_eq!(m.cpu.read_u32(RESULTS + 16).unwrap(), 71);
        assert_eq!(m.cpu.read_u32(RESULTS + 20).unwrap(), 72);
        no_gaps(&m);
    }
}

// spec: PERS-EXPORT-2
#[test]
fn nonexistent_legacy_time_and_string_contract_memchr_stay_hidden_from_guest_lookup() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let load = m.resolve_import("kernel32.dll", "LoadLibraryA").unwrap();
        let lookup = m.resolve_import("kernel32.dll", "GetProcAddress").unwrap();
        let error = m.resolve_import("kernel32.dll", "GetLastError").unwrap();
        let mut code = Asm::new(CODE);
        for (i, (module, name)) in [
            ("msvcr80.dll", "time"),
            ("msvcr90.dll", "time"),
            ("api-ms-win-crt-string-l1-1-0.dll", "memchr"),
        ]
        .into_iter()
        .enumerate()
        {
            let input = DATA + 4096 + i as u32 * 128;
            m.cpu
                .write(input, format!("{module}\0").as_bytes())
                .unwrap();
            m.cpu
                .write(input + 64, format!("{name}\0").as_bytes())
                .unwrap();
            code.push_imm(input)
                .call(load)
                .mov_mem_eax(RESULTS + i as u32 * 12);
            code.push_imm(input + 64)
                .push_eax()
                .call(lookup)
                .mov_mem_eax(RESULTS + i as u32 * 12 + 4);
            code.call(error).mov_mem_eax(RESULTS + i as u32 * 12 + 8);
        }
        code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        m.cpu.write(CODE, &code.bytes).unwrap();
        m.spawn("absent CRT exports", CODE, &[], 65536).unwrap();
        let result = m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for i in 0..3 {
            assert_ne!(m.cpu.read_u32(RESULTS + i * 12).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(RESULTS + i * 12 + 4).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(RESULTS + i * 12 + 8).unwrap(), 127);
        }
    }
}

// spec: PERS-CRT-5
#[test]
fn identity_and_sleep_fronts_share_windows_processes_and_virtual_scheduling() {
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
            "api-ms-win-crt-runtime-l1-1-0.dll",
        ] {
            let mut m = machine(jit);
            let tick = m.resolve_import("kernel32.dll", "GetTickCount").unwrap();
            let pid = m
                .resolve_import("kernel32.dll", "GetCurrentProcessId")
                .unwrap();
            let mut a = Asm::new(CODE);
            a.call(tick).mov_mem_eax(RESULTS);
            invoke(&mut m, &mut a, module, "_getpid", &[], RESULTS + 4);
            a.call(pid).mov_mem_eax(RESULTS + 8);
            invoke(&mut m, &mut a, module, "_sleep", &[0], RESULTS + 12);
            a.call(tick).mov_mem_eax(RESULTS + 16);
            invoke(&mut m, &mut a, module, "_sleep", &[5], RESULTS + 20);
            a.call(tick).mov_mem_eax(RESULTS + 24);
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            m.cpu.write(CODE, &a.bytes).unwrap();
            m.spawn("CRT process services", CODE, &[], 65536).unwrap();
            let result = m.run_virtual(1_000_000);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module}: {result:?}"
            );
            assert_ne!(m.cpu.read_u32(RESULTS + 4).unwrap(), 0);
            assert_eq!(
                m.cpu.read_u32(RESULTS + 4).unwrap(),
                m.cpu.read_u32(RESULTS + 8).unwrap()
            );
            assert_eq!(
                m.cpu.read_u32(RESULTS + 16).unwrap() - m.cpu.read_u32(RESULTS).unwrap(),
                1
            );
            assert_eq!(
                m.cpu.read_u32(RESULTS + 24).unwrap() - m.cpu.read_u32(RESULTS + 16).unwrap(),
                5
            );
        }
    }
}
