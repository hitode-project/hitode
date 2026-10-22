use super::*;
use adv_abi::signature;
use adv_cpu::backend::Guest;

// spec: CRT-LIB-18
#[test]
fn quick_cleanup_preserves_callbacks_and_quick_exit_has_a_separate_array() {
    for jit in [false, true] {
        for module in ["ucrtbase.dll", RUNTIME] {
            for mode in ["exit", "quick_exit", "_Exit"] {
                let mut m = machine(JitCpu::new(PARK_ADDR, jit).unwrap());
                for (pointer, digit) in [(FIRST, 1), (SECOND, 2), (THIRD, 3)] {
                    callback(&mut m, pointer, digit);
                }
                let mut a = Asm::new(CODE);
                let normal = m.resolve_import(module, "_crt_atexit").unwrap();
                let quick = m.resolve_import(module, "_crt_at_quick_exit").unwrap();
                a.push_imm(FIRST).call(normal).add_esp(4);
                a.push_imm(SECOND).call(quick).add_esp(4);
                a.push_imm(THIRD).call(quick).add_esp(4);
                a.call(m.resolve_import(module, "_c_exit").unwrap());
                a.mov_eax_mem(DATA).mov_mem_eax(DATA + 4);
                a.push_imm(73).call(m.resolve_import(module, mode).unwrap());
                run(&mut m, a, 73);
                assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
                assert_eq!(
                    m.cpu.read_u32(DATA).unwrap(),
                    match mode {
                        "exit" => 1,
                        "quick_exit" => 32,
                        _ => 0,
                    },
                    "{module} {jit} {mode}"
                );
                if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
                    assert_eq!(d.stub_calls, 0);
                    assert!(d.warnings.is_empty(), "{:?}", d.warnings);
                }
            }
        }
    }
}

// spec: CRT-LIB-19
#[test]
fn initializer_versions_skip_nulls_and_only_error_initializers_stop_early() {
    let declarations: Vec<_> = signature::signatures()
        .iter()
        .filter(|s| matches!(s.name, "_initterm" | "_initterm_e"))
        .collect();
    assert!(declarations.len() >= 18);
    for jit in [false, true] {
        for s in &declarations {
            let mut m = machine(JitCpu::new(PARK_ADDR, jit).unwrap());
            for (pointer, digit) in [(FIRST, 1), (SECOND, 2), (THIRD, 3)] {
                callback(&mut m, pointer, digit);
            }
            for (i, pointer) in [0, FIRST, 0, SECOND, THIRD, 0].into_iter().enumerate() {
                m.cpu.write_u32(DATA + 32 + i as u32 * 4, pointer).unwrap();
            }
            let mut a = Asm::new(CODE);
            a.push_imm(DATA + 56)
                .push_imm(DATA + 32)
                .call(m.resolve_import(s.module, s.name).unwrap())
                .add_esp(8)
                .mov_mem_eax(DATA + 4);
            run(&mut m, a, 0);
            let checking = s.name == "_initterm_e";
            assert_eq!(
                m.cpu.read_u32(DATA).unwrap(),
                if checking { 1 } else { 123 },
                "{} {} {jit}",
                s.module,
                s.name
            );
            if checking {
                assert_eq!(m.cpu.read_u32(DATA + 4).unwrap(), u32::MAX);
            }
            if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
                assert_eq!(d.stub_calls, 0);
                assert!(d.warnings.is_empty(), "{:?}", d.warnings);
            }
        }
    }
}

// spec: CRT-LIB-18
#[test]
fn full_cleanup_closes_only_owned_streams_even_without_an_exit_table() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    for jit in [false, true] {
        for callbacks in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "hitode-crt-exit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("stream.bin"), b"file").unwrap();
            let mut m = machine_with(
                JitCpu::new(PARK_ADDR, jit).unwrap(),
                Win32Config::new(&root, "fixture.exe", root.join("data")),
            );
            m.cpu.write(DATA + 128, b"stream.bin\0").unwrap();
            m.cpu.write(DATA + 160, b"rb\0").unwrap();
            callback(&mut m, FIRST, 1);
            let mut a = Asm::new(CODE);
            for (module, output) in [("msvcr80.dll", DATA + 4), ("msvcr90.dll", DATA + 8)] {
                a.push_imm(DATA + 160)
                    .push_imm(DATA + 128)
                    .call(m.resolve_import(module, "fopen").unwrap())
                    .add_esp(8)
                    .mov_mem_eax(output);
            }
            if callbacks {
                a.push_imm(FIRST)
                    .call(m.resolve_import("msvcr80.dll", "atexit").unwrap())
                    .add_esp(4);
            }
            a.call(m.resolve_import("msvcr80.dll", "_c_exit").unwrap());
            a.mov_eax_mem(DATA + 4)
                .push_eax()
                .push_imm(1)
                .push_imm(1)
                .push_imm(DATA + 512)
                .call(m.resolve_import("msvcr80.dll", "fread").unwrap())
                .add_esp(16)
                .mov_mem_eax(DATA + 16);
            a.call(m.resolve_import("msvcr80.dll", "_cexit").unwrap());
            for (module, stream, output) in [
                ("msvcr80.dll", DATA + 4, DATA + 20),
                ("msvcr90.dll", DATA + 8, DATA + 24),
            ] {
                a.mov_eax_mem(stream)
                    .push_eax()
                    .call(m.resolve_import(module, "fclose").unwrap())
                    .add_esp(4)
                    .mov_mem_eax(output);
            }
            run(&mut m, a, 0);
            assert_ne!(m.cpu.read_u32(DATA + 4).unwrap(), 0);
            assert_ne!(m.cpu.read_u32(DATA + 8).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(DATA).unwrap(), u32::from(callbacks));
            assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 1);
            assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), u32::MAX);
            assert_eq!(m.cpu.read_u32(DATA + 24).unwrap(), 0);
            assert_eq!(m.cpu.read_u8(DATA + 512).unwrap(), b'f');
            drop(m);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
