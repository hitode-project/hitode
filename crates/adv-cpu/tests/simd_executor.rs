use adv_cpu::interpreter::{Interpreter, Step};
use adv_cpu::{CpuState, EXEC, Memory, READ, WRITE};
use iced_x86::code_asm::*;
const CODE: u32 = 0x10000;
const DATA: u32 = 0x20000;
fn code(f: impl FnOnce(&mut CodeAssembler)) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a);
    a.assemble(CODE as u64).unwrap()
}
fn memory() -> Memory {
    let mut m = Memory::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m
}
fn step(bytes: &[u8], s: &mut CpuState, m: &mut Memory) -> Step {
    s.eip = CODE;
    m.write(CODE, bytes).unwrap();
    Interpreter::new().step(s, m)
}
fn singles(values: [f32; 4]) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}
fn doubles(values: [f64; 2]) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (chunk, value) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}
fn integers(values: [i32; 4]) -> [u8; 16] {
    let mut bytes = [0; 16];
    for (chunk, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[cfg(target_arch = "x86_64")]
fn host_convert(op: usize, source: [u8; 16], mxcsr: u32) -> ([u8; 16], u32) {
    #[repr(align(16))]
    struct Saved([u8; 512]);
    let mut saved = Saved([0; 512]);
    let mut out = [0; 16];
    let mut flags = 0u32;
    macro_rules! native {
        ($ins:literal)=>{{
            // SAFETY: preserve complete host FP/SIMD state, use live pointers
            // and aligned FXSAVE storage. MOVDQU allows ordinary array alignment.
            unsafe {std::arch::asm!("fxsave [{save}]","ldmxcsr [{control}]",
                "movdqu xmm1, [{input}]",$ins,"movdqu [{output}], xmm0","stmxcsr [{flags}]",
                "fxrstor [{save}]",save=in(reg)saved.0.as_mut_ptr(),control=in(reg)&mxcsr,
                input=in(reg)source.as_ptr(),output=in(reg)out.as_mut_ptr(),flags=in(reg)&mut flags,
                out("xmm0")_,out("xmm1")_,options(nostack));}
        }};
    }
    match op {
        0 => native!("cvtps2dq xmm0,xmm1"),
        1 => native!("cvttps2dq xmm0,xmm1"),
        2 => native!("cvtdq2ps xmm0,xmm1"),
        3 => native!("cvtpd2dq xmm0,xmm1"),
        4 => native!("cvttpd2dq xmm0,xmm1"),
        5 => native!("cvtdq2pd xmm0,xmm1"),
        6 => native!("cvtps2pd xmm0,xmm1"),
        7 => native!("cvtpd2ps xmm0,xmm1"),
        _ => unreachable!(),
    }
    (out, flags)
}

#[cfg(target_arch = "x86_64")]
#[test]
fn packed_sse2_conversions_match_native_results_flags_all_rc_and_special_lanes() {
    let bytes = [
        code(|a| a.cvtps2dq(xmm0, xmm1).unwrap()),
        code(|a| a.cvttps2dq(xmm0, xmm1).unwrap()),
        code(|a| a.cvtdq2ps(xmm0, xmm1).unwrap()),
        code(|a| a.cvtpd2dq(xmm0, xmm1).unwrap()),
        code(|a| a.cvttpd2dq(xmm0, xmm1).unwrap()),
        code(|a| a.cvtdq2pd(xmm0, xmm1).unwrap()),
        code(|a| a.cvtps2pd(xmm0, xmm1).unwrap()),
        code(|a| a.cvtpd2ps(xmm0, xmm1).unwrap()),
    ];
    for (op, bytes) in bytes.iter().enumerate() {
        for special in [false, true] {
            let source = match op {
                2 | 5 => integers([16_777_217, -16_777_217, i32::MAX, i32::MIN]),
                3 | 4 | 7 if special => doubles([f64::NAN, f64::INFINITY]),
                3 | 4 | 7 => doubles([1.5, -2.5]),
                _ if special => singles([f32::NAN, f32::INFINITY, 2147483648.0, -2147483648.0]),
                _ => singles([1.5, -2.5, 1.1, -1.1]),
            };
            for rc in 0..4 {
                let mxcsr = 0x1f80 | (rc << 13);
                let (expected, flags) = host_convert(op, source, mxcsr);
                let mut s = CpuState {
                    mxcsr,
                    ..CpuState::default()
                };
                s.xmm[0] = [0x5a; 16];
                s.xmm[1] = source;
                assert_eq!(step(bytes, &mut s, &mut memory()), Step::Executed);
                assert_eq!(s.xmm[0], expected, "op {op} RC {rc} special {special}");
                assert_eq!(s.mxcsr, flags, "op {op} RC {rc} special {special}");
            }
        }
    }
}

#[test]
fn packed_sse_exception_commits_sticky_flags_and_suppresses_all_lanes() {
    let bytes = code(|a| a.cvtps2dq(xmm0, xmm1).unwrap());
    let mut s = CpuState {
        mxcsr: 0x1f00,
        ..CpuState::default()
    };
    s.xmm[0] = [0xaa; 16];
    s.xmm[1] = singles([1.5, 2.0, f32::NAN, 4.0]);
    assert_eq!(
        step(&bytes, &mut s, &mut memory()),
        Step::Trap {
            pc: CODE,
            vector: 19
        }
    );
    assert_eq!(s.xmm[0], [0xaa; 16]);
    assert_eq!(s.mxcsr & 0x21, 0x21);
    assert_eq!(s.eip, CODE);
    // A preexisting unmasked sticky bit does not retrigger on an exact operation.
    s.xmm[1] = singles([1.0, 2.0, 3.0, 4.0]);
    assert_eq!(step(&bytes, &mut s, &mut memory()), Step::Executed);
    assert_eq!(s.xmm[0], integers([1, 2, 3, 4]));
}

#[test]
fn legacy_packed_mmx_conversions_preserve_xmm_high_half_and_enter_alias_state() {
    for (bytes, double) in [
        (code(|a| a.cvtpi2ps(xmm0, mm1).unwrap()), false),
        (code(|a| a.cvtpi2pd(xmm0, mm1).unwrap()), true),
    ] {
        let mut s = CpuState::default();
        s.xmm[0] = [0xaa; 16];
        s.mmx[1] = 2u64 << 32 | 1;
        s.mmx_mode = 1;
        assert_eq!(step(&bytes, &mut s, &mut memory()), Step::Executed);
        if double {
            assert_eq!(s.xmm[0], doubles([1.0, 2.0]));
        } else {
            assert_eq!(&s.xmm[0][..8], &singles([1.0, 2.0, 0.0, 0.0])[..8]);
            assert_eq!(&s.xmm[0][8..], &[0xaa; 8]);
        }
        assert_eq!(s.x87.tag, 0);
        assert_eq!(s.x87.top(), 0);
    }
    for (bytes, double, truncation) in [
        (code(|a| a.cvtps2pi(mm0, xmm1).unwrap()), false, false),
        (code(|a| a.cvttps2pi(mm0, xmm1).unwrap()), false, true),
        (code(|a| a.cvtpd2pi(mm0, xmm1).unwrap()), true, false),
        (code(|a| a.cvttpd2pi(mm0, xmm1).unwrap()), true, true),
    ] {
        let mut s = CpuState::default();
        s.xmm[1] = if double {
            doubles([1.5, -2.5])
        } else {
            singles([1.5, -2.5, 0.0, 0.0])
        };
        assert_eq!(step(&bytes, &mut s, &mut memory()), Step::Executed);
        assert_eq!(
            s.mmx[0],
            u64::from((-2i32) as u32) << 32 | if truncation { 1 } else { 2 }
        );
        assert_eq!(s.x87.regs[0].sign_exponent(), 0xffff);
        assert_eq!(s.x87.tag, 0);
        assert_eq!(s.mxcsr & 0x20, 0x20);
    }
}

#[test]
fn sqrt_ignores_old_destination_and_new_denormal_exception_is_precise() {
    let bytes = code(|a| a.sqrtss(xmm0, xmm1).unwrap());
    let mut s = CpuState {
        mxcsr: 0x1e80,
        ..CpuState::default()
    };
    s.xmm[0][..4].copy_from_slice(&1u32.to_le_bytes());
    s.xmm[1] = singles([4.0, 0.0, 0.0, 0.0]);
    assert_eq!(step(&bytes, &mut s, &mut memory()), Step::Executed);
    assert_eq!(&s.xmm[0][..4], &2f32.to_le_bytes());
    assert_eq!(s.mxcsr & 2, 0);
    s.xmm[1][..4].copy_from_slice(&1u32.to_le_bytes());
    let before = s.xmm;
    assert_eq!(
        step(&bytes, &mut s, &mut memory()),
        Step::Trap {
            pc: CODE,
            vector: 19
        }
    );
    assert_eq!(s.xmm, before);
    assert_eq!(s.mxcsr & 2, 2);
}

#[test]
fn packed_compare_all_predicates_preserve_flags_and_report_nan_invalid() {
    for double in [false, true] {
        for predicate in 0..8 {
            let bytes = code(|a| {
                if double {
                    a.cmppd(xmm0, xmm1, predicate).unwrap()
                } else {
                    a.cmpps(xmm0, xmm1, predicate).unwrap()
                }
            });
            let mut s = CpuState {
                eflags: 0x8d7,
                ..CpuState::default()
            };
            s.xmm[0] = if double {
                doubles([1.0, f64::NAN])
            } else {
                singles([1.0, 2.0, 3.0, f32::NAN])
            };
            s.xmm[1] = if double {
                doubles([2.0, 1.0])
            } else {
                singles([2.0, 2.0, 1.0, 0.0])
            };
            assert_eq!(step(&bytes, &mut s, &mut memory()), Step::Executed);
            let relations = if double { vec![1, 3] } else { vec![1, 0, 2, 3] };
            for (chunk, c) in s.xmm[0]
                .chunks_exact(if double { 8 } else { 4 })
                .zip(relations)
            {
                let yes = match predicate {
                    0 => c == 0,
                    1 => c == 1,
                    2 => c <= 1,
                    3 => c == 3,
                    4 => c != 0,
                    5 => c != 1,
                    6 => c > 1,
                    _ => c != 3,
                };
                assert!(chunk.iter().all(|&byte| byte == if yes { 255 } else { 0 }));
            }
            assert_eq!(s.eflags, 0x8d7);
            assert_eq!(s.mxcsr & 1, u32::from(matches!(predicate, 1 | 2 | 5 | 6)));
        }
    }
}

#[test]
fn packed_unsigned_multiply_sad_and_mmx_xmm_transfer_preserve_alias_contract() {
    let mul = code(|a| a.pmuludq(xmm0, xmm1).unwrap());
    let mut s = CpuState::default();
    s.xmm[0] = integers([-1, 7, 3, 9]);
    s.xmm[1] = integers([2, 5, 4, 6]);
    assert_eq!(step(&mul, &mut s, &mut memory()), Step::Executed);
    assert_eq!(
        u64::from_le_bytes(s.xmm[0][..8].try_into().unwrap()),
        u64::from(u32::MAX) * 2
    );
    assert_eq!(u64::from_le_bytes(s.xmm[0][8..].try_into().unwrap()), 12);
    let sad = code(|a| a.psadbw(xmm0, xmm1).unwrap());
    s.xmm[0] = [255; 16];
    s.xmm[1] = [0; 16];
    assert_eq!(step(&sad, &mut s, &mut memory()), Step::Executed);
    assert_eq!(
        s.xmm[0],
        [248, 7, 0, 0, 0, 0, 0, 0, 248, 7, 0, 0, 0, 0, 0, 0]
    );
    let q = 0x0123_4567_89ab_cdefu64;
    s.mmx[3] = q;
    s.mmx_mode = 1;
    let to_xmm = code(|a| a.movq2dq(xmm2, mm3).unwrap());
    assert_eq!(step(&to_xmm, &mut s, &mut memory()), Step::Executed);
    assert_eq!(&s.xmm[2][..8], &q.to_le_bytes());
    assert_eq!(&s.xmm[2][8..], &[0; 8]);
    assert_eq!(s.x87.top(), 0);
    assert_eq!(s.x87.tag, 0);
    let to_mmx = code(|a| a.movdq2q(mm5, xmm2).unwrap());
    assert_eq!(step(&to_mmx, &mut s, &mut memory()), Step::Executed);
    assert_eq!(s.mmx[5], q);
    assert_eq!(s.x87.regs[5].sign_exponent(), 0xffff);
    s.x87.status |= 0x80;
    let before = s.x87.clone();
    let emms = code(|a| a.emms().unwrap());
    assert_eq!(
        step(&emms, &mut s, &mut memory()),
        Step::Trap {
            pc: CODE,
            vector: 16
        }
    );
    assert_eq!(s.x87, before);
}

#[test]
fn masked_store_checks_only_selected_bytes_and_never_partially_commits_on_fault() {
    let bytes = code(|a| a.maskmovdqu(xmm0, xmm1).unwrap());
    let mut m = memory();
    m.protect(DATA + 4096, 4096, READ).unwrap();
    let mut s = CpuState::default();
    s.regs[7] = DATA + 4092;
    s.xmm[0] = [0x7b; 16];
    s.xmm[1][..4].fill(0x80);
    assert_eq!(step(&bytes, &mut s, &mut m), Step::Executed);
    assert_eq!(m.read(DATA + 4092, 4).unwrap(), [0x7b; 4]);
    s.xmm[0] = [0x33; 16];
    s.xmm[1][4] = 0x80;
    assert_eq!(
        step(&bytes, &mut s, &mut m),
        Step::WriteFault {
            pc: CODE,
            address: DATA + 4096
        }
    );
    assert_eq!(m.read(DATA + 4092, 4).unwrap(), [0x7b; 4]);
    s.xmm[1] = [0; 16];
    s.regs[7] = u32::MAX;
    assert_eq!(step(&bytes, &mut s, &mut m), Step::Executed);
}

#[test]
fn packed_alignment_faults_precede_write_and_unaligned_movdqu_is_valid() {
    let bytes = code(|a| a.addps(xmm0, xmmword_ptr(DATA as i32 + 1)).unwrap());
    let mut s = CpuState::default();
    s.xmm[0] = [0xaa; 16];
    assert_eq!(
        step(&bytes, &mut s, &mut memory()),
        Step::Trap {
            pc: CODE,
            vector: 13
        }
    );
    assert_eq!(s.xmm[0], [0xaa; 16]);
    let bytes = code(|a| a.movdqu(xmm0, xmmword_ptr(DATA as i32 + 1)).unwrap());
    assert_eq!(step(&bytes, &mut s, &mut memory()), Step::Executed);
    assert_eq!(s.xmm[0], [0; 16]);
}

#[test]
fn reciprocal_approximations_obey_error_bound_ignore_rc_and_leave_flags_unchanged() {
    for sqrt in [false, true] {
        let bytes = code(|a| {
            if sqrt {
                a.rsqrtps(xmm0, xmm1).unwrap()
            } else {
                a.rcpps(xmm0, xmm1).unwrap()
            }
        });
        for rc in 0..4 {
            let mut s = CpuState {
                mxcsr: 0x1f81 | (rc << 13),
                ..CpuState::default()
            };
            s.xmm[1] = singles([0.3, 1.0, 2.0, 1234.5]);
            let before = s.mxcsr;
            assert_eq!(step(&bytes, &mut s, &mut memory()), Step::Executed);
            for (out, source) in s.xmm[0]
                .as_chunks::<4>()
                .0
                .iter()
                .zip([0.3f64, 1.0, 2.0, 1234.5])
            {
                let result = f32::from_le_bytes(*out) as f64;
                let exact = 1.0 / if sqrt { source.sqrt() } else { source };
                assert!((result - exact).abs() / exact <= 1.5 / 4096.0);
            }
            assert_eq!(s.mxcsr, before);
        }
    }
}
