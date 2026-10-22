use super::*;

// spec: CRT-LIB-14
#[test]
fn set_searches_and_spans_share_unsigned_byte_and_raw_wide_membership_in_every_version() {
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
            "api-ms-win-crt-string-l1-1-0.dll",
        ] {
            let mut m = machine(jit);
            let mut a = Asm::new(CODE);
            for wide in [false, true] {
                let (first, second, set) = if wide {
                    (
                        vec![0x5000, 0xd800, 0xdc00, 0xd800],
                        vec![0xd800, 0xdc00, 0x5000],
                        vec![0xdc00, 0xd800],
                    )
                } else {
                    (vec![65, 255, 128, 255], vec![255, 128, 65], vec![128, 255])
                };
                let input = DATA + if wide { 128 } else { 0 };
                text(&mut m, input, &first, wide);
                text(&mut m, input + 32, &second, wide);
                text(&mut m, input + 64, &set, wide);
                text(&mut m, input + 96, &[], wide);
                let prefix = if wide { "wcs" } else { "str" };
                for (i, (suffix, source, control)) in [
                    ("pbrk", input, input + 64),
                    ("cspn", input, input + 64),
                    ("spn", input + 32, input + 64),
                    ("pbrk", input + 96, input + 64),
                    ("cspn", input, input + 96),
                    ("spn", input, input + 96),
                ]
                .into_iter()
                .enumerate()
                {
                    invoke(
                        &mut m,
                        &mut a,
                        module,
                        &format!("{prefix}{suffix}"),
                        &[source, control],
                        DATA + 512 + u32::from(wide) * 32 + i as u32 * 4,
                    );
                }
            }
            a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
            let result = run(&mut m, a);
            assert!(
                matches!(result, RunResult::Exited(0)),
                "{module} {jit}: {result:?}"
            );
            for (offset, expected) in [
                (0, DATA + 1),
                (4, 1),
                (8, 2),
                (12, 0),
                (16, 4),
                (20, 0),
                (32, DATA + 128 + 2),
                (36, 1),
                (40, 2),
                (44, 0),
                (48, 4),
                (52, 0),
            ] {
                assert_eq!(
                    m.cpu.read_u32(DATA + 512 + offset).unwrap(),
                    expected,
                    "{module} {jit} {offset}"
                );
            }
            if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
                assert_eq!(d.stub_calls, 0);
                assert!(d.warnings.is_empty());
            }
        }
    }
}

// spec: CRT-LIB-14
#[test]
fn unaligned_wide_matches_stop_before_an_inaccessible_following_unit() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let tail = DATA + 16384 - 4;
        m.cpu.write(tail, &0xd800u16.to_le_bytes()).unwrap();
        m.cpu.write(tail + 2, &33u16.to_le_bytes()).unwrap();
        text(&mut m, DATA, &[0xd800], true);
        text(&mut m, DATA + 32, &[33], true);
        // An odd address leaves a complete final UTF-16 unit before the guard.
        m.cpu.write(tail + 1, &0xdc00u16.to_le_bytes()).unwrap();
        text(&mut m, DATA + 64, &[0xdc00], true);
        let mut a = Asm::new(CODE);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "wcspbrk",
            &[tail + 1, DATA + 64],
            DATA + 128,
        );
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "wcscspn",
            &[tail + 1, DATA + 64],
            DATA + 132,
        );
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "wcsspn",
            &[tail + 1, DATA],
            DATA + 136,
        );
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        assert!(matches!(run(&mut m, a), RunResult::Exited(0)));
        assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), tail + 1);
        assert_eq!(m.cpu.read_u32(DATA + 132).unwrap(), 0);
        assert_eq!(m.cpu.read_u32(DATA + 136).unwrap(), 0);
    }
}

// spec: CRT-LIB-14
#[test]
fn terminated_set_scans_can_cross_the_old_sixteen_megabyte_limit() {
    for jit in [false, true] {
        let mut m = machine(jit);
        let source = 0x600000;
        let length = 16 * 1024 * 1024 + 17;
        m.cpu.map(source, 17 * 1024 * 1024, Prot::RW).unwrap();
        m.cpu.write(source, &vec![b'a'; length]).unwrap();
        m.cpu.write(source + length as u32, &[0]).unwrap();
        text(&mut m, DATA, &[97], false);
        let mut a = Asm::new(CODE);
        invoke(
            &mut m,
            &mut a,
            "msvcr90.dll",
            "strspn",
            &[source, DATA],
            DATA + 32,
        );
        a.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        let result = run(&mut m, a);
        assert!(matches!(result, RunResult::Exited(0)), "{jit}: {result:?}");
        assert_eq!(m.cpu.read_u32(DATA + 32).unwrap(), length as u32);
        if let Some(d) = m.ext.try_get::<adv_abi::ApiDiagnostics>() {
            assert!(d.warnings.is_empty());
        }
    }
}
