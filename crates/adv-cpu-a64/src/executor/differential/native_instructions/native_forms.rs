use super::*;

fn check(p: &Program, config: Config, permission: Option<u8>, native_only: bool) {
    let mut native = executor(p, config, false);
    let mut oracle = executor(p, config, true);
    if let Some(permission) = permission {
        for cpu in [&mut native, &mut oracle] {
            cpu.protect(DATA, DATA_LEN, permission).unwrap();
        }
    }
    let actual = native.run(p.stop, 100).unwrap();
    assert_eq!(actual, oracle.run(p.stop, 100).unwrap());
    assert_eq!(
        (native.state.regs, native.state.eflags),
        (oracle.state.regs, oracle.state.eflags),
        "code={:?} config={config:?} permission={permission:?}",
        p.code
    );
    same(&native, &oracle);
    if native_only && permission.is_none() && !config.1 {
        assert_eq!(actual, RunOutcome::ReachedStop);
        assert!(native.stats.native_entries > 0);
        assert_eq!(native.stats.interpreter_entries, 0, "code={:?}", p.code);
        assert_eq!(
            native.native.as_ref().unwrap().diagnostics.helpers,
            0,
            "code={:?}",
            p.code
        );
        if !config.0.flagm && config.2 == Checks::Adaptive {
            no_operation_calls(p);
        }
    }
}

fn no_operation_calls(p: &Program) {
    use crate::translate::{Stubs, Workspace, translate};
    let mut memory = Memory::new(BASE, LEN).unwrap();
    load(&mut memory, p);
    let mut work = Workspace::default();
    let at = 0x1000_0000usize;
    let stubs = Stubs {
        exit: at + 0x10000,
        dispatch: at + 0x10004,
        lookup: at + 0x10008,
        lookup_slow: at + 0x1000c,
        ret_miss: at + 0x10010,
        access: at + 0x10014,
        interpret: at + 0x10018,
        call: at + 0x10020,
        fault: at + 0x10024,
    };
    translate(
        &mut work,
        &mut memory,
        CODE,
        &[p.stop],
        &|_| false,
        0,
        Features { flagm: false },
        at,
        stubs,
        &|_, _| None,
        &|_| None,
        &|_, _| Vec::new(),
        0x037f,
    );
    let exceptional = iced_x86::Decoder::new(32, &p.code, 0).iter().any(|i| {
        matches!(
            i.mnemonic(),
            iced_x86::Mnemonic::Div | iced_x86::Mnemonic::Idiv
        ) || i.has_lock_prefix()
            || i.mnemonic() == iced_x86::Mnemonic::Xchg && i.op0_kind() == iced_x86::OpKind::Memory
    });
    for (n, &word) in work.words().iter().enumerate() {
        if !exceptional {
            assert_ne!(
                word,
                crate::encode::bl((stubs.interpret - at) as i32 - n as i32 * 4),
                "interpret call"
            );
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_bit_strings_match_offsets_flags_and_faults() {
    for kind in 0..4 {
        for width in [2, 4] {
            for memory in [false, true] {
                for immediate in [false, true] {
                    for offset in [0u32, 15, 16, 31, 32, 63, 255, 0xffff_ffff, 0xffff_ffdf] {
                        for value in [0u32, 0x8000_8000, u32::MAX] {
                            let mut p = fixture(|a| {
                                a.nop().unwrap();
                                macro_rules! bit {
                                    ($dst:expr, $src:expr) => {
                                        match kind {
                                            0 => a.bt($dst, $src),
                                            1 => a.bts($dst, $src),
                                            2 => a.btr($dst, $src),
                                            _ => a.btc($dst, $src),
                                        }
                                        .unwrap()
                                    };
                                }
                                macro_rules! source {
                                    ($dst:expr, $reg:expr) => {
                                        if immediate {
                                            bit!($dst, (offset & 255) as i32);
                                        } else {
                                            bit!($dst, $reg);
                                        }
                                    };
                                }
                                match (width, memory) {
                                    (2, false) => source!(bp, cx),
                                    (4, false) => source!(ebp, ecx),
                                    (2, true) => source!(word_ptr(ebx + 256), cx),
                                    _ => source!(dword_ptr(ebx + 256), ecx),
                                }
                            });
                            p.state.regs[1] = offset;
                            p.state.regs[5] = value;
                            for chunk in p.data.as_chunks_mut::<4>().0 {
                                chunk.copy_from_slice(&value.to_le_bytes());
                            }
                            for config in pixel_loops::configs().filter(|c| !c.1) {
                                check(&p, config, None, true);
                                if memory && offset == 32 && value == u32::MAX {
                                    check(&p, config, Some(0), false);
                                    if kind != 0 {
                                        check(&p, config, Some(adv_cpu_core::READ), false);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_multiply_matches_all_widths_sources_and_products() {
    for width in [1, 2, 4] {
        for signed in [false, true] {
            for count in 1..=3 {
                if count != 1 && (!signed || width == 1) {
                    continue;
                }
                for memory in [false, true] {
                    for value in [0u32, 1, 0x80, 0x0180, 0x8080, 0x8000, 0x8000_0000, u32::MAX] {
                        let mut p = fixture(|a| {
                            a.nop().unwrap();
                            macro_rules! multiply {
                                ($src:expr, $dst:expr) => {
                                    match (signed, count) {
                                        (false, _) => a.mul($src),
                                        (true, 1) => a.imul($src),
                                        (true, 2) => a.imul_2($dst, $src),
                                        _ => a.imul_3($dst, $src, -3),
                                    }
                                    .unwrap()
                                };
                            }
                            match (width, memory) {
                                (1, false) => if signed { a.imul(ah) } else { a.mul(ch) }.unwrap(),
                                (1, true) => if signed {
                                    a.imul(byte_ptr(ebx))
                                } else {
                                    a.mul(byte_ptr(ebx))
                                }
                                .unwrap(),
                                (2, false) => multiply!(bp, dx),
                                (2, true) => multiply!(word_ptr(ebx), dx),
                                (4, false) if count == 1 => multiply!(ebp, edx),
                                (4, false) => multiply!(ecx, ebp),
                                _ => multiply!(dword_ptr(ebx), ebp),
                            }
                        });
                        p.state.regs[0] = value;
                        p.state.regs[1] = value;
                        p.state.regs[2] = value;
                        p.state.regs[5] = value;
                        p.data[..4].copy_from_slice(&value.to_le_bytes());
                        for config in pixel_loops::configs() {
                            check(&p, config, None, true);
                            if memory && value == u32::MAX {
                                check(&p, config, Some(0), false);
                            }
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_divide_matches_quotients_remainders_and_de() {
    for width in [1, 2, 4] {
        for signed in [false, true] {
            for memory in [false, true] {
                for (lo, hi, divisor) in [
                    (0u32, 0u32, 1u32),
                    (17, 0, 3),
                    (127, 0, 1),
                    (0xff80, 0, 1),
                    (0x8000, 0, u32::MAX),
                    (0, 0x80000000, u32::MAX),
                    (u32::MAX, u32::MAX, u32::MAX),
                    (0x80008000, 0, 1),
                    (0, 1, 1),
                    (0, 0, 0),
                    (0x80000000, 0x80008000, u32::MAX),
                ] {
                    let mut p = fixture(|a| {
                        a.nop().unwrap();
                        macro_rules! divide {
                            ($src:expr) => {
                                if signed { a.idiv($src) } else { a.div($src) }.unwrap()
                            };
                        }
                        match (width, memory) {
                            (1, false) => divide!(ch),
                            (1, true) => divide!(byte_ptr(ebx)),
                            (2, false) => divide!(bp),
                            (2, true) => divide!(word_ptr(ebx)),
                            (4, false) => divide!(ebp),
                            _ => divide!(dword_ptr(ebx)),
                        }
                    });
                    p.state.regs[0] = lo;
                    p.state.regs[2] = hi;
                    p.state.regs[1] = divisor << 8;
                    p.state.regs[5] = divisor;
                    p.data[..4].copy_from_slice(&divisor.to_le_bytes());
                    for config in pixel_loops::configs() {
                        let safe = hi == 0 && lo < 128 && divisor != 0;
                        check(&p, config, None, safe);
                        if memory && divisor == 1 {
                            check(&p, config, Some(0), false);
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_narrow_exchanges_match_aliases_locks_and_refused_stores() {
    for kind in 0..3 {
        for width in [1, 2] {
            for form in 0..7 {
                for value in [0u32, 0x7f, 0x80, 0x8000, u32::MAX] {
                    for equal in [false, true] {
                        let mut p = fixture(|a| {
                            a.nop().unwrap();
                            macro_rules! exchange {
                                ($a:expr, $dst:expr, $src:expr) => {
                                    match kind {
                                        0 => $a.xchg($dst, $src),
                                        1 => $a.xadd($dst, $src),
                                        _ => $a.cmpxchg($dst, $src),
                                    }
                                    .unwrap()
                                };
                            }
                            match (width, form) {
                                (1, 0) => exchange!(a, ah, ch),
                                (1, 1) => exchange!(a, ah, al),
                                (1, 2) => exchange!(a, al, ah),
                                (1, 3) => exchange!(a, ah, ah),
                                (1, 4 | 5) => exchange!(a, byte_ptr(ebx + 1), ch),
                                (1, _) => exchange!(a.lock(), byte_ptr(ebx + 1), ch),
                                (2, 0) => exchange!(a, bp, cx),
                                (2, 1) => exchange!(a, ax, cx),
                                (2, 2) => exchange!(a, bp, ax),
                                (2, 3) => exchange!(a, bp, bp),
                                (2, 4 | 5) => exchange!(a, word_ptr(ebx + 1), cx),
                                _ => exchange!(a.lock(), word_ptr(ebx + 1), cx),
                            }
                        });
                        p.state.regs[0] = if equal { value } else { value ^ 0x8080 };
                        p.state.regs[1] = value.wrapping_add(1);
                        p.state.regs[5] = value;
                        p.data[1..5].copy_from_slice(&value.to_le_bytes());
                        for config in pixel_loops::configs().filter(|c| !c.1) {
                            check(&p, config, None, true);
                            if form >= 4 && value == u32::MAX {
                                check(&p, config, Some(0), false);
                                check(&p, config, Some(adv_cpu_core::READ), false);
                            }
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_checked_simd_and_half_moves_preserve_dirty_state() {
    for form in 0..5 {
        let mut p = fixture(|a| {
            a.pxor(xmm3, xmm2).unwrap();
            match form {
                0 => a.pinsrw(xmm3, word_ptr(ebx + 1), 7).unwrap(),
                1 => {
                    a.movd(mm3, eax).unwrap();
                    a.pinsrw(mm3, word_ptr(ebx + 1), 3).unwrap();
                }
                2 => a.stmxcsr(dword_ptr(ebx + 1)).unwrap(),
                3 => a.movlhps(xmm3, xmm2).unwrap(),
                _ => a.movhlps(xmm3, xmm2).unwrap(),
            }
        });
        p.state.xmm = std::array::from_fn(|n| std::array::from_fn(|i| (n * 19 + i) as u8));
        p.data.fill(0xa5);
        for config in pixel_loops::configs() {
            check(&p, config, None, true);
            if form < 3 {
                check(&p, config, Some(0), false);
                if form == 2 {
                    check(&p, config, Some(adv_cpu_core::READ), false);
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_packed_conversions_match_rounding_indefinite_and_exceptions() {
    for form in 0..3 {
        for memory in [false, true] {
            for values in [
                [0u32, 0x80000000, 0x3fc00000, 0xc0200000],
                [0x7fc01234, 0x7f801234, 0x7f800000, 0xff800000],
                [0x4effffff, 0x4f000000, 0xcf000000, 0xcf000001],
                [1, 0x80000001, 0x007fffff, 0x807fffff],
                [0x7fffffff, 0x80000000, 0xffffffff, 0x01000001],
            ] {
                let mut p = fixture(|a| {
                    a.pxor(xmm4, xmm2).unwrap();
                    macro_rules! convert {
                        ($src:expr) => {
                            match form {
                                0 => a.cvttps2dq(xmm3, $src),
                                1 => a.cvtps2dq(xmm3, $src),
                                _ => a.cvtdq2ps(xmm3, $src),
                            }
                            .unwrap()
                        };
                    }
                    if memory {
                        convert!(xmmword_ptr(ebx));
                    } else {
                        convert!(xmm3);
                    }
                });
                for (n, value) in values.iter().enumerate() {
                    p.state.xmm[3][4 * n..4 * n + 4].copy_from_slice(&value.to_le_bytes());
                }
                p.data[..16].copy_from_slice(&p.state.xmm[3]);
                for rc in 0..4 {
                    for control in [0x1f80, 0x1fc0, 0x9f80, 0x1f00, 0x0f80, 0x1e80] {
                        p.state.mxcsr = control | (rc << 13);
                        for config in pixel_loops::configs().filter(|c| !c.1) {
                            check(&p, config, None, form != 2 && control & 0x1f80 == 0x1f80);
                            if memory && rc == 0 && control == 0x1f80 {
                                check(&p, config, Some(0), false);
                            }
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_cwd_and_carry_controls_match_flags() {
    for value in [0u32, 0x7fff, 0x8000, u32::MAX] {
        let mut p = fixture(|a| {
            a.nop().unwrap();
            a.cwd().unwrap();
            a.clc().unwrap();
            a.cmc().unwrap();
            a.stc().unwrap();
            a.cmc().unwrap();
        });
        p.state.regs[0] = value;
        for config in pixel_loops::configs() {
            check(&p, config, None, true);
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_short_simd_sequences_match_memory_aliases_and_faults() {
    for form in 0..13 {
        for memory in [false, true] {
            for alias in [false, true] {
                for value in [0u8, 0x80, 0xff] {
                    let mut p = fixture(|a| {
                        a.add(ebp, eax).unwrap();
                        let src = if alias { xmm3 } else { xmm2 };
                        macro_rules! binary {
                            ($op:ident, $dst:expr, $src:expr, $mem:ident) => {
                                if memory {
                                    a.$op($dst, $mem(ebx))
                                } else {
                                    a.$op($dst, $src)
                                }
                                .unwrap()
                            };
                        }
                        macro_rules! shuffle {
                            ($op:ident) => {
                                if memory {
                                    a.$op(xmm3, xmmword_ptr(ebx), 0x1b)
                                } else {
                                    a.$op(xmm3, src, 0x1b)
                                }
                                .unwrap()
                            };
                        }
                        match form {
                            0 => shuffle!(pshufhw),
                            1 => shuffle!(pshuflw),
                            2 => shuffle!(shufpd),
                            3 => binary!(pmuludq, xmm3, src, xmmword_ptr),
                            4 => binary!(pmuludq, mm3, if alias { mm3 } else { mm2 }, qword_ptr),
                            5 => binary!(psadbw, xmm3, src, xmmword_ptr),
                            6 => binary!(psadbw, mm3, if alias { mm3 } else { mm2 }, qword_ptr),
                            7 => a.movmskps(ebp, src).unwrap(),
                            8 => a.movmskpd(esp, src).unwrap(),
                            9 => a.movq2dq(xmm3, mm2).unwrap(),
                            10 => a.movdq2q(mm3, xmm2).unwrap(),
                            11 => a.movnti(dword_ptr(ebx), eax).unwrap(),
                            _ => a.femms().unwrap(),
                        }
                    });
                    p.state.xmm = std::array::from_fn(|n| {
                        std::array::from_fn(|lane| value.wrapping_add((n * 71 + lane * 37) as u8))
                    });
                    p.state.mmx = [u64::from_le_bytes([value; 8]); 8];
                    p.data[..16].copy_from_slice(&p.state.xmm[2]);
                    for config in pixel_loops::configs() {
                        check(&p, config, None, true);
                        if (memory && form < 7 || form == 11) && value == 0xff {
                            check(&p, config, Some(0), false);
                            if form == 11 {
                                check(&p, config, Some(adv_cpu_core::READ), false);
                            }
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_short_integer_sequences_and_hints_match_flags_and_faults() {
    for form in 0..10 {
        for value in [0u32, 0x8000_0080, u32::MAX] {
            let mut p = fixture(|a| {
                a.nop().unwrap();
                match form {
                    0 => a.not(ah).unwrap(),
                    1 => a.not(bp).unwrap(),
                    2 => a.not(byte_ptr(ebx)).unwrap(),
                    3 => a.not(word_ptr(ebx)).unwrap(),
                    4 => a.lock().not(dword_ptr(ebx)).unwrap(),
                    5 => a.db(&[0x66, 0x0f, 0xcc]).unwrap(), // BSWAP SP clears its word.
                    6 => a.clflush(byte_ptr(ebx)).unwrap(),
                    9 => {
                        a.rol(ah, 0).unwrap();
                        a.ror(bp, 32).unwrap();
                        a.rcl(ebp, 0).unwrap();
                        a.rcr(ch, 0).unwrap();
                    }
                    8 => {
                        a.db(&[0x65]).unwrap();
                        a.lfence().unwrap();
                    }
                    _ => {
                        a.pause().unwrap();
                        a.lfence().unwrap();
                        a.sfence().unwrap();
                        a.mfence().unwrap();
                    }
                }
            });
            p.state.regs[0] = value;
            p.state.regs[5] = value;
            p.data[..4].copy_from_slice(&value.to_le_bytes());
            for config in pixel_loops::configs() {
                check(&p, config, None, form != 8);
                if matches!(form, 2 | 3 | 4 | 6) {
                    check(&p, config, Some(0), false);
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_resume_at_budget_cuts_and_crossing_accesses() {
    let p = fixture(|a| {
        a.add(eax, edx).unwrap();
        a.pxor(xmm3, xmm2).unwrap();
        a.pinsrw(xmm3, word_ptr(ebx + 1), 5).unwrap();
        a.movlhps(xmm3, xmm3).unwrap();
        a.bts(dword_ptr(ebx), 35).unwrap();
        a.lock().btc(word_ptr(ebx), cx).unwrap();
        a.xadd(ah, ch).unwrap();
        a.cmpxchg(word_ptr(ebx + 2), bp).unwrap();
        a.imul_3(bp, word_ptr(ebx + 4), -1).unwrap();
        a.stmxcsr(dword_ptr(ebx + 7)).unwrap();
        a.cvtps2dq(xmm1, xmm0).unwrap();
    });
    for config in pixel_loops::configs() {
        for budget in 1..=12 {
            let mut native = executor(&p, config, false);
            let mut oracle = executor(&p, config, true);
            assert_eq!(
                native.run(p.stop, budget).unwrap(),
                oracle.run(p.stop, budget).unwrap()
            );
            same(&native, &oracle);
            assert_eq!(
                native.run(p.stop, 100).unwrap(),
                oracle.run(p.stop, 100).unwrap()
            );
            same(&native, &oracle);
            assert_eq!(
                native.native.as_ref().unwrap().diagnostics.helpers,
                0,
                "code={:?}",
                p.code
            );
        }
    }
    for form in 0..7 {
        for complete in [false, true] {
            let mut p = fixture(|a| {
                a.pxor(xmm3, xmm2).unwrap();
                match form {
                    0 => a.pinsrw(xmm3, word_ptr(ebx), 0).unwrap(),
                    1 => a.stmxcsr(dword_ptr(ebx)).unwrap(),
                    2 => a.lock().xadd(word_ptr(ebx), bp).unwrap(),
                    3 => a.lock().cmpxchg(word_ptr(ebx), bp).unwrap(),
                    4 => a.lock().bts(word_ptr(ebx), cx).unwrap(),
                    5 => a.imul(word_ptr(ebx)).unwrap(),
                    _ => a.idiv(word_ptr(ebx)).unwrap(),
                }
            });
            p.state.regs[3] = DATA + 4095;
            p.state.regs[0] = 17;
            p.state.regs[2] = 0;
            p.state.regs[1] = 0;
            p.data[4095..4097].copy_from_slice(&3u16.to_le_bytes());
            for config in pixel_loops::configs().filter(|c| !c.1) {
                let mut native = executor(&p, config, false);
                let mut oracle = executor(&p, config, true);
                if !complete {
                    for cpu in [&mut native, &mut oracle] {
                        cpu.protect(DATA + 4096, 4096, 0).unwrap();
                    }
                }
                assert_eq!(
                    native.run(p.stop, 100).unwrap(),
                    oracle.run(p.stop, 100).unwrap()
                );
                same(&native, &oracle);
                assert_eq!(
                    native.native.as_ref().unwrap().diagnostics.helpers,
                    0,
                    "code={:?}",
                    p.code
                );
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_biased_exchanges_swaps_and_byte_tables_match() {
    for form in 0..10 {
        for value in [0u32, 0x8000_0080, u32::MAX] {
            let mut p = fixture(|a| {
                a.add(eax, edx).unwrap();
                match form {
                    0 => a.xchg(ebp, ecx).unwrap(),
                    1 => a.xadd(ecx, ebp).unwrap(),
                    2 => a.cmpxchg(ecx, esp).unwrap(),
                    3 => a.xchg(dword_ptr(ebx), esp).unwrap(),
                    4 => a.lock().xadd(dword_ptr(ebx), ebp).unwrap(),
                    5 => a.lock().cmpxchg(dword_ptr(ebx), esp).unwrap(),
                    6 => a.bswap(esp).unwrap(),
                    7 => a.bswap(ebp).unwrap(),
                    _ => a.xlatb().unwrap(),
                }
            });
            p.state.regs[0] = value;
            p.state.regs[1] = value;
            p.data.fill(0xa5);
            for config in pixel_loops::configs() {
                check(&p, config, None, true);
                if matches!(form, 3 | 4 | 5 | 8 | 9) {
                    check(&p, config, Some(0), false);
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_narrow_conditions_extensions_and_memory_negation_match() {
    for form in 0..13 {
        for memory in [false, true] {
            for value in [0u32, 0x7f, 0x80, 0x8000, 0xffff, u32::MAX] {
                let mut p = fixture(|a| {
                    a.add(eax, edx).unwrap();
                    macro_rules! cond {
                        ($op:ident, $reg:expr, $mem:expr) => {
                            if memory { a.$op($mem) } else { a.$op($reg) }.unwrap()
                        };
                    }
                    macro_rules! source {
                        ($op:ident, $dst:expr, $reg:expr, $mem:expr) => {
                            if memory {
                                a.$op($dst, $mem)
                            } else {
                                a.$op($dst, $reg)
                            }
                            .unwrap()
                        };
                    }
                    match form {
                        0 => cond!(sete, ah, byte_ptr(ebx)),
                        1 => cond!(setb, ch, byte_ptr(ebx)),
                        2 => cond!(setp, ah, byte_ptr(ebx)),
                        3 => cond!(setnp, ch, byte_ptr(ebx)),
                        4 => source!(cmovl, bp, cx, word_ptr(ebx)),
                        5 => source!(cmova, sp, ax, word_ptr(ebx)),
                        6 => source!(cmovp, sp, cx, word_ptr(ebx)),
                        7 => source!(cmovnp, ebp, ecx, dword_ptr(ebx)),
                        8 => source!(movzx, bp, ah, byte_ptr(ebx)),
                        9 => source!(movsx, sp, ch, byte_ptr(ebx)),
                        10 => source!(movsx, bp, ax, word_ptr(ebx)),
                        11 => a.neg(byte_ptr(ebx)).unwrap(),
                        _ => a.lock().neg(word_ptr(ebx)).unwrap(),
                    }
                });
                p.state.regs[0] = value;
                p.state.regs[1] = value;
                p.state.regs[2] = value;
                p.data[..4].copy_from_slice(&value.to_le_bytes());
                for config in pixel_loops::configs() {
                    check(&p, config, None, true);
                    if memory || form > 10 {
                        check(&p, config, Some(0), false);
                        if !(4..=10).contains(&form) {
                            check(&p, config, Some(adv_cpu_core::READ), false);
                        }
                    }
                }
            }
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_full_immediates_and_fs_accesses_match() {
    for immediate in [0i32, 1, -1, 127, -128, 128, 32767, -32768, 0xffff] {
        for memory in [false, true] {
            let mut p = fixture(|a| {
                a.add(eax, edx).unwrap();
                if memory {
                    a.imul_3(bp, word_ptr(ebx), immediate)
                } else {
                    a.imul_3(ax, ax, immediate)
                }
                .unwrap();
            });
            p.data[..2].copy_from_slice(&0x8000u16.to_le_bytes());
            for config in pixel_loops::configs() {
                check(&p, config, None, true);
            }
        }
    }
    for form in 0..9 {
        let mut p = fixture(|a| {
            a.add(edi, eax).unwrap();
            a.db(&[0x64]).unwrap();
            match form {
                0 => a.bt(dword_ptr(ebx), ecx).unwrap(),
                1 => a.btr(word_ptr(ebx), cx).unwrap(),
                2 => a.imul(word_ptr(ebx)).unwrap(),
                3 => a.div(byte_ptr(ebx)).unwrap(),
                4 => a.xadd(word_ptr(ebx), bp).unwrap(),
                5 => a.pinsrw(xmm3, word_ptr(ebx), 7).unwrap(),
                6 => a.stmxcsr(dword_ptr(ebx)).unwrap(),
                7 => a.cvtps2dq(xmm3, xmmword_ptr(ebx)).unwrap(),
                _ => a.xlatb().unwrap(),
            }
        });
        p.state.fs_base = DATA;
        p.state.regs[3] = 0;
        p.state.regs[0] = 17;
        p.state.regs[1] = 33;
        p.data.fill(3);
        for config in pixel_loops::configs() {
            check(&p, config, None, true);
            check(&p, config, Some(0), false);
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_mxcsr_loads_refresh_rounding_guards_and_fault_exactly() {
    for control in [0x1f80u32, 0x3f80, 0x5f80, 0x7f80] {
        let mut p = fixture(|a| {
            a.addps(xmm0, xmm1).unwrap();
            a.ldmxcsr(dword_ptr(ebx)).unwrap();
            a.addps(xmm0, xmm1).unwrap();
        });
        p.state.mxcsr |= 0x20;
        p.data[..4].copy_from_slice(&control.to_le_bytes());
        p.state.xmm[0] = [1.0f32.to_bits().to_le_bytes(); 4]
            .concat()
            .try_into()
            .unwrap();
        p.state.xmm[1] = [(2.0f32.powi(-24)).to_bits().to_le_bytes(); 4]
            .concat()
            .try_into()
            .unwrap();
        for config in pixel_loops::configs() {
            check(&p, config, None, control == 0x1f80);
        }
    }
    for control in [0x1f80u32, 0x3f80, 0x5fc0, 0x7f80, 0x10000, u32::MAX] {
        let mut p = fixture(|a| {
            a.addps(xmm0, xmm1).unwrap();
            a.ldmxcsr(dword_ptr(ebx)).unwrap();
            a.cvtps2dq(xmm3, xmm2).unwrap();
            a.stmxcsr(dword_ptr(ebx + 4)).unwrap();
        });
        p.data[..4].copy_from_slice(&control.to_le_bytes());
        p.state.xmm[2] = [1.5f32.to_bits().to_le_bytes(); 4]
            .concat()
            .try_into()
            .unwrap();
        for config in pixel_loops::configs() {
            check(&p, config, None, control <= 0xffff);
            check(&p, config, Some(0), false);
        }
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_long_conversion_runs_keep_guards_in_branch_range() {
    let mut p = fixture(|a| {
        a.movd(mm0, eax).unwrap();
        for _ in 0..90 {
            a.cvtps2dq(xmm3, xmm2).unwrap();
        }
    });
    p.state.xmm[2] = [1.5f32.to_bits().to_le_bytes(); 4]
        .concat()
        .try_into()
        .unwrap();
    for config in pixel_loops::configs() {
        check(&p, config, None, true);
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_simd_loops_preserve_cached_registers_across_backedges() {
    let mut p = fixture(|a| {
        let mut top = a.create_label();
        a.set_label(&mut top).unwrap();
        a.pinsrw(xmm3, word_ptr(ebx), 7).unwrap();
        a.movlhps(xmm3, xmm2).unwrap();
        a.movhlps(xmm4, xmm3).unwrap();
        a.cvtps2dq(xmm5, xmm2).unwrap();
        a.cvttps2dq(xmm6, xmm2).unwrap();
        a.pshuflw(xmm3, xmm3, 0x1b).unwrap();
        a.pshufhw(xmm4, xmm3, 0xe4).unwrap();
        a.shufpd(xmm5, xmm6, 1).unwrap();
        a.psadbw(xmm3, xmm4).unwrap();
        a.pmuludq(mm3, mm2).unwrap();
        a.movq2dq(xmm4, mm3).unwrap();
        a.movdq2q(mm4, xmm5).unwrap();
        a.ldmxcsr(dword_ptr(ebx + 16)).unwrap();
        a.stmxcsr(dword_ptr(ebx + 20)).unwrap();
        a.dec(ecx).unwrap();
        a.jnz(top).unwrap();
    });
    p.state.regs[1] = 3;
    p.state.xmm[2] = [1.5f32.to_bits().to_le_bytes(); 4]
        .concat()
        .try_into()
        .unwrap();
    p.data[16..20].copy_from_slice(&0x1f80u32.to_le_bytes());
    for config in pixel_loops::configs() {
        check(&p, config, None, true);
    }
}

// spec: A64-TR-4
#[test]
fn native_forms_word_addresses_wrap_before_segments_and_bit_string_offsets() {
    for form in 0..41 {
        let mut p = fixture(|a| {
            a.add(edi, eax).unwrap();
            a.db(&[0x64]).unwrap(); // FS applies after the 16-bit address wraps.
            match form {
                0 => a.bt(word_ptr(bx + si), cx).unwrap(),
                1 => a.bts(dword_ptr(bx + si), ecx).unwrap(),
                2 => a.btr(word_ptr(bx + si), 31).unwrap(),
                3 => a.lock().btc(dword_ptr(bx + si), 63).unwrap(),
                4 => a.mul(byte_ptr(bx + si)).unwrap(),
                5 => a.imul(word_ptr(bx + si)).unwrap(),
                6 => a.mul(dword_ptr(bx + si)).unwrap(),
                7 => a.imul_2(edx, dword_ptr(bx + si)).unwrap(),
                8 => a.imul_3(bp, word_ptr(bx + si), -3).unwrap(),
                9 => a.div(byte_ptr(bx + si)).unwrap(),
                10 => a.idiv(word_ptr(bx + si)).unwrap(),
                11 => a.div(dword_ptr(bx + si)).unwrap(),
                12 => a.xchg(byte_ptr(bx + si), ah).unwrap(),
                13 => a.lock().xadd(word_ptr(bx + si), bp).unwrap(),
                14 => a.lock().cmpxchg(byte_ptr(bx + si), ch).unwrap(),
                15 => a.cmpxchg(word_ptr(bx + si), bp).unwrap(),
                16 => a.pinsrw(xmm3, word_ptr(bx + si), 7).unwrap(),
                17 => a.pinsrw(mm3, word_ptr(bx + si), 3).unwrap(),
                18 => a.stmxcsr(dword_ptr(bx + si)).unwrap(),
                19 => a.cvtps2dq(xmm3, xmmword_ptr(bx + si)).unwrap(),
                20 => a.cvttps2dq(xmm3, xmmword_ptr(bx + si)).unwrap(),
                21 => a.cvtdq2ps(xmm3, xmmword_ptr(bx + si)).unwrap(),
                22 => a.imul_3(edx, dword_ptr(bx + si), -3).unwrap(),
                23 => a.pinsrw(xmm3, word_ptr(bp + di), 2).unwrap(),
                24 => a.btc(word_ptr(bp + di), cx).unwrap(),
                25 => a.movzx(ebp, byte_ptr(bp + di)).unwrap(),
                26 => a.movsx(esp, word_ptr(bx + si)).unwrap(),
                27 => a.neg(word_ptr(bx + si)).unwrap(),
                28 => a.not(dword_ptr(bx + si)).unwrap(),
                29 => a.setp(byte_ptr(bx + si)).unwrap(),
                30 => a.cmovp(ebp, dword_ptr(bx + si)).unwrap(),
                31 => a.cmove(bp, word_ptr(bx + si)).unwrap(),
                32 => a.pshufhw(xmm3, xmmword_ptr(bx + si), 0x1b).unwrap(),
                33 => a.pshuflw(xmm3, xmmword_ptr(bx + si), 0xe4).unwrap(),
                34 => a.shufpd(xmm3, xmmword_ptr(bx + si), 1).unwrap(),
                35 => a.pmuludq(mm3, qword_ptr(bx + si)).unwrap(),
                36 => a.psadbw(xmm3, xmmword_ptr(bx + si)).unwrap(),
                37 => a.ldmxcsr(dword_ptr(bx + si)).unwrap(),
                38 => a.movnti(dword_ptr(bp + di), eax).unwrap(),
                39 => a.clflush(byte_ptr(bx + si)).unwrap(),
                _ => {
                    a.db(&[0x67]).unwrap();
                    a.xlatb().unwrap();
                }
            }
        });
        p.state.fs_base = DATA;
        p.state.regs[0] = 17;
        p.state.regs[1] = 33;
        p.state.regs[2] = 0;
        p.state.regs[3] = 0xffff_ffff;
        p.state.regs[6] = 0x1234_0201;
        p.data.fill(0);
        for chunk in p.data[0x200..0x240].as_chunks_mut::<4>().0 {
            chunk.copy_from_slice(&3u32.to_le_bytes());
        }
        for config in pixel_loops::configs() {
            check(&p, config, None, true);
            check(&p, config, Some(0), false);
        }
    }
}
