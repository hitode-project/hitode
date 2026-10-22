use super::*;
fn store(a: &mut Asm, slot: u32, value: u32) {
    a.mov_eax_mem(slot);
    a.bytes.extend_from_slice(&[0xc7, 0]);
    a.bytes.extend_from_slice(&value.to_le_bytes());
}
fn callback_counter(m: &mut Machine<JitCpu>) {
    let mut a = Asm::new(CALLBACK);
    a.mov_eax_esp(4).mov_mem_eax(DATA + 0x900);
    a.bytes.extend_from_slice(&[0xff, 0x05]);
    a.bytes.extend_from_slice(&(DATA + 0x904).to_le_bytes());
    a.ret();
    m.cpu.write(CALLBACK, &a.bytes).unwrap();
}

// spec: CRT-LIB-16, CRT-LIB-17
#[test]
fn signal_fronts_share_aliases_and_reset_handlers_before_explicit_delivery() {
    let modules: BTreeSet<_> = signature::signatures()
        .iter()
        .filter(|s| s.name == "signal")
        .map(|s| s.module)
        .collect();
    assert_eq!(modules.len(), 12);
    for jit in [false, true] {
        for module in &modules {
            let mut m = machine(jit);
            callback_counter(&mut m);
            let mut a = Asm::new(CODE);
            for (i, (name, args)) in [
                ("signal", vec![15, CALLBACK]),
                ("raise", vec![15]),
                ("signal", vec![15, 2]),
                ("signal", vec![6, CALLBACK]),
                ("signal", vec![22, 2]),
                ("raise", vec![6]),
                ("signal", vec![22, 2]),
                ("signal", vec![21, 1]),
                ("raise", vec![21]),
                ("__fpecode", vec![]),
                ("__pxcptinfoptrs", vec![]),
            ]
            .into_iter()
            .enumerate()
            {
                invoke(&mut m, &mut a, module, name, &args, RESULTS + i as u32 * 4);
            }
            a.mov_eax_mem(RESULTS + 36)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 48);
            a.mov_eax_mem(RESULTS + 40)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 52);
            let result = finish(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            for (i, expected) in [
                (0, 0),
                (1, 0),
                (2, 0),
                (3, 0),
                (4, CALLBACK),
                (5, 0),
                (6, 0),
                (7, 0),
                (8, 0),
                (12, 0),
                (13, 0),
            ] {
                assert_eq!(
                    m.cpu.read_u32(RESULTS + i * 4).unwrap(),
                    expected,
                    "{module} {jit} {i}"
                );
            }
            assert_ne!(m.cpu.read_u32(RESULTS + 36).unwrap(), 0);
            assert_ne!(m.cpu.read_u32(RESULTS + 40).unwrap(), 0);
            assert_ne!(
                m.cpu.read_u32(RESULTS + 36).unwrap(),
                m.cpu.read_u32(RESULTS + 40).unwrap()
            );
            assert_eq!(m.cpu.read_u32(DATA + 0x900).unwrap(), 6);
            assert_eq!(m.cpu.read_u32(DATA + 0x904).unwrap(), 2);
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-16, CRT-LIB-17
#[test]
fn explicit_fp_handlers_observe_reason_and_null_context_then_restore_thread_cells() {
    for jit in [false, true] {
        for module in [
            "msvcr80.dll",
            "msvcr90.dll",
            "ucrtbase.dll",
            "api-ms-win-crt-runtime-l1-1-0.dll",
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            invoke(&mut m, &mut a, module, "__fpecode", &[], DATA + 0x800);
            invoke(&mut m, &mut a, module, "__pxcptinfoptrs", &[], DATA + 0x804);
            store(&mut a, DATA + 0x800, 77);
            store(&mut a, DATA + 0x804, 0x12345678);
            let mut callback = Asm::new(CALLBACK);
            callback.mov_eax_esp(4).mov_mem_eax(DATA + 0x900);
            callback.mov_eax_esp(8).mov_mem_eax(DATA + 0x904);
            invoke(
                &mut m,
                &mut callback,
                module,
                "signal",
                &[8, 2],
                DATA + 0x908,
            );
            callback
                .mov_eax_mem(DATA + 0x800)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 0x90c);
            callback
                .mov_eax_mem(DATA + 0x804)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 0x910);
            store(&mut callback, DATA + 0x800, 99);
            store(&mut callback, DATA + 0x804, 0xdeadbeef);
            callback.ret();
            m.cpu.write(CALLBACK, &callback.bytes).unwrap();
            invoke(&mut m, &mut a, module, "signal", &[8, CALLBACK], RESULTS);
            invoke(&mut m, &mut a, module, "raise", &[8], RESULTS + 4);
            a.mov_eax_mem(DATA + 0x800)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 8);
            a.mov_eax_mem(DATA + 0x804)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 12);
            assert!(
                matches!(finish(&mut m, a), RunResult::Exited(0)),
                "{module} {jit}"
            );
            for (offset, expected) in [(0, 8), (4, 0x8c), (8, 0), (12, 0x8c), (16, 0)] {
                assert_eq!(
                    m.cpu.read_u32(DATA + 0x900 + offset).unwrap(),
                    expected,
                    "{module} {offset}"
                );
            }
            assert_eq!(m.cpu.read_u32(RESULTS + 4).unwrap(), 0);
            assert_eq!(m.cpu.read_u32(RESULTS + 8).unwrap(), 77);
            assert_eq!(m.cpu.read_u32(RESULTS + 12).unwrap(), 0x12345678);
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-16, CRT-LIB-17
#[test]
fn legacy_filters_use_real_actions_fault_reasons_and_cpp_status_gating() {
    for jit in [false, true] {
        for (status, reason) in [
            (0xc000008d, 0x82),
            (0xc000008e, 0x83),
            (0xc000008f, 0x86),
            (0xc0000090, 0x81),
            (0xc0000091, 0x84),
            (0xc0000092, 0x8a),
            (0xc0000093, 0x85),
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "__fpecode",
                &[],
                DATA + 0x800,
            );
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "__pxcptinfoptrs",
                &[],
                DATA + 0x804,
            );
            store(&mut a, DATA + 0x800, 77);
            store(&mut a, DATA + 0x804, 0x12345678);
            let mut callback = Asm::new(CALLBACK);
            callback.mov_eax_esp(8).mov_mem_eax(DATA + 0x900);
            callback
                .mov_eax_mem(DATA + 0x800)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 0x904);
            callback
                .mov_eax_mem(DATA + 0x804)
                .mov_eax_ptr_eax()
                .mov_mem_eax(DATA + 0x908);
            callback.ret();
            m.cpu.write(CALLBACK, &callback.bytes).unwrap();
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "_XcptFilter",
                &[status, DATA + 0xa00],
                RESULTS,
            );
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "signal",
                &[8, CALLBACK],
                RESULTS + 4,
            );
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "__CppXcptFilter",
                &[status, DATA + 0xa00],
                RESULTS + 8,
            );
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "__CppXcptFilter",
                &[0xe06d7363, DATA + 0xa00],
                RESULTS + 12,
            );
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "_XcptFilter",
                &[status, DATA + 0xa00],
                RESULTS + 16,
            );
            invoke(
                &mut m,
                &mut a,
                "msvcr90.dll",
                "_XcptFilter",
                &[status, DATA + 0xa00],
                RESULTS + 20,
            );
            a.mov_eax_mem(DATA + 0x800)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 24);
            a.mov_eax_mem(DATA + 0x804)
                .mov_eax_ptr_eax()
                .mov_mem_eax(RESULTS + 28);
            assert!(
                matches!(finish(&mut m, a), RunResult::Exited(0)),
                "{status:x} {jit}"
            );
            for n in [0, 4, 8, 12, 20] {
                assert_eq!(m.cpu.read_u32(RESULTS + n).unwrap(), 0);
            }
            assert_eq!(m.cpu.read_u32(RESULTS + 16).unwrap(), u32::MAX);
            assert_eq!(m.cpu.read_u32(DATA + 0x900).unwrap(), reason);
            assert_eq!(m.cpu.read_u32(DATA + 0x904).unwrap(), reason);
            assert_eq!(m.cpu.read_u32(DATA + 0x908).unwrap(), DATA + 0xa00);
            assert_eq!(m.cpu.read_u32(RESULTS + 24).unwrap(), 77);
            assert_eq!(m.cpu.read_u32(RESULTS + 28).unwrap(), 0x12345678);
            no_gaps(&m);
        }
    }
}

// spec: CRT-LIB-16
#[test]
fn process_signals_cross_threads_while_fault_signals_and_other_crts_remain_independent() {
    for jit in [false, true] {
        let mut m = machine(jit);
        callback_counter(&mut m);
        let mut main = Asm::new(CODE);
        invoke(
            &mut m,
            &mut main,
            "msvcr90.dll",
            "signal",
            &[15, CALLBACK],
            RESULTS,
        );
        invoke(
            &mut m,
            &mut main,
            "msvcr90.dll",
            "signal",
            &[8, CALLBACK],
            RESULTS + 4,
        );
        main.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        let worker_address = CODE + 0x1000;
        let mut worker = Asm::new(worker_address);
        invoke(
            &mut m,
            &mut worker,
            "msvcr90.dll",
            "signal",
            &[15, 2],
            RESULTS + 8,
        );
        invoke(
            &mut m,
            &mut worker,
            "msvcr90.dll",
            "signal",
            &[8, 2],
            RESULTS + 12,
        );
        invoke(
            &mut m,
            &mut worker,
            "msvcr80.dll",
            "signal",
            &[15, 2],
            RESULTS + 16,
        );
        worker.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        m.cpu.write(CODE, &main.bytes).unwrap();
        m.cpu.write(worker_address, &worker.bytes).unwrap();
        m.spawn("signal owner", CODE, &[], 65536).unwrap();
        m.spawn("signal observer", worker_address, &[], 65536)
            .unwrap();
        assert!(matches!(m.run_virtual(5_000_000), RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(RESULTS + 8).unwrap(), CALLBACK);
        assert_eq!(m.cpu.read_u32(RESULTS + 12).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(RESULTS + 16).unwrap(), 0);
        no_gaps(&m);
    }
}
