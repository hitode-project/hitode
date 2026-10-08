//! Authored three-way SSE fixtures. Tier 1 must match every MXCSR bit;
//! Cranelift's existing arithmetic sticky-flag allowance is checked separately.
use super::*;
use iced_x86::{Code, Instruction, MemoryOperand, Register};

fn program(
    code: Code,
    memory: bool,
    integer_source: bool,
    integer_dest: bool,
    predicate: Option<u8>,
) -> Vec<u8> {
    let dst = if integer_dest {
        Register::EAX
    } else {
        Register::XMM0
    };
    let src = if integer_source {
        Register::ECX
    } else {
        Register::XMM1
    };
    let operand = MemoryOperand::with_base(Register::ESI);
    let i = match (memory, predicate) {
        (true, Some(p)) => Instruction::with3(code, dst, operand, u32::from(p)),
        (false, Some(p)) => Instruction::with3(code, dst, src, u32::from(p)),
        (true, None) => Instruction::with2(code, dst, operand),
        (false, None) => Instruction::with2(code, dst, src),
    }
    .unwrap();
    assemble(|a| {
        a.add_instruction(i).unwrap();
        a.ret().unwrap();
    })
}

fn three(code: &[u8]) -> [Executor; 3] {
    [machine(code, 0), machine(code, 1), machine(code, 2)]
}
fn seed(m: &mut Executor, a: u64, b: u64, width: usize, mxcsr: u32) {
    reset(m, [0x5555, b as u32, 7, 11, 0, 13, DATA, 17], 0x8d7);
    m.state.xmm = [[0xa5; 16]; 8];
    for lane in 0..16 / width {
        m.state.xmm[0][lane * width..(lane + 1) * width].copy_from_slice(&a.to_le_bytes()[..width]);
        m.state.xmm[1][lane * width..(lane + 1) * width].copy_from_slice(&b.to_le_bytes()[..width]);
    }
    m.state.mxcsr = mxcsr;
    let source = m.state.xmm[1];
    m.write(DATA, &source).unwrap();
}
fn check(machines: &mut [Executor; 3], exact_cranelift_flags: bool) -> RunOutcome {
    machines[2].set_tier_verification(Some(true));
    let outcomes: Vec<_> = machines
        .iter_mut()
        .map(|m| m.run(STOP, 100).unwrap())
        .collect();
    assert_eq!(outcomes[0], outcomes[1]);
    assert_eq!(outcomes[0], outcomes[2]);
    equivalent(&machines[0], &machines[2], u32::MAX);
    // Native Cranelift arithmetic intentionally diagnoses missing sticky bits.
    // Never apply that allowance to Tier 1 or hide control-word differences.
    let original = machines[1].state.mxcsr;
    if !exact_cranelift_flags {
        assert_eq!(original & !0x3f, machines[0].state.mxcsr & !0x3f);
        machines[1].state.mxcsr = machines[0].state.mxcsr;
    }
    equivalent(&machines[0], &machines[1], u32::MAX);
    machines[1].state.mxcsr = original;
    assert_eq!(machines[2].stats.verification.mismatches, 0);
    assert_eq!(machines[2].stats.verification.mxcsr_flags, 0);
    outcomes.into_iter().next().unwrap()
}

fn arithmetic() -> Vec<(Code, usize)> {
    use Code::*;
    vec![
        (Addss_xmm_xmmm32, 4),
        (Subss_xmm_xmmm32, 4),
        (Mulss_xmm_xmmm32, 4),
        (Divss_xmm_xmmm32, 4),
        (Sqrtss_xmm_xmmm32, 4),
        (Minss_xmm_xmmm32, 4),
        (Maxss_xmm_xmmm32, 4),
        (Addps_xmm_xmmm128, 4),
        (Subps_xmm_xmmm128, 4),
        (Mulps_xmm_xmmm128, 4),
        (Divps_xmm_xmmm128, 4),
        (Sqrtps_xmm_xmmm128, 4),
        (Minps_xmm_xmmm128, 4),
        (Maxps_xmm_xmmm128, 4),
        (Addsd_xmm_xmmm64, 8),
        (Subsd_xmm_xmmm64, 8),
        (Mulsd_xmm_xmmm64, 8),
        (Divsd_xmm_xmmm64, 8),
        (Sqrtsd_xmm_xmmm64, 8),
        (Minsd_xmm_xmmm64, 8),
        (Maxsd_xmm_xmmm64, 8),
        (Addpd_xmm_xmmm128, 8),
        (Subpd_xmm_xmmm128, 8),
        (Mulpd_xmm_xmmm128, 8),
        (Divpd_xmm_xmmm128, 8),
        (Sqrtpd_xmm_xmmm128, 8),
        (Minpd_xmm_xmmm128, 8),
        (Maxpd_xmm_xmmm128, 8),
    ]
}
fn values(width: usize) -> Vec<u64> {
    if width == 4 {
        vec![
            0, 0x80000000, 0x3f800000, 0xbf800000, 0x40400000, 0x3eaaaaab, 0x7f800000, 0xff800000,
            0x7fc12345, 0xffc54321, 0x7f812345, 1, 0x807fffff, 0x00800000, 0x7f7fffff, 0x33800000,
        ]
    } else {
        vec![
            0,
            1 << 63,
            1f64.to_bits(),
            (-1f64).to_bits(),
            3f64.to_bits(),
            (1f64 / 3.).to_bits(),
            0x7ff0000000000000,
            0xfff0000000000000,
            0x7ff8123456789abc,
            0xfff8abcdef012345,
            0x7ff0123456789abc,
            1,
            0x800fffffffffffff,
            0x0010000000000000,
            0x7fefffffffffffff,
            0x3ca0000000000000,
        ]
    }
}

#[test]
fn sse_arithmetic_special_values_and_all_lanes_match_three_tiers() {
    for (code, width) in arithmetic() {
        for memory in [false, true] {
            let bytes = program(code, memory, false, false, None);
            let mut machines = three(&bytes);
            for a in values(width) {
                for b in values(width) {
                    for m in &mut machines {
                        seed(m, a, b, width, 0x1f80 | 4);
                    }
                    check(&mut machines, false);
                }
            }
            assert!(
                machines[2].tier1_stats().native_instructions > 0,
                "{code:?}"
            );
            assert!(
                machines[2].tier1_stats().fallback_instructions > 0,
                "{code:?}"
            );
        }
    }
}

#[test]
fn sse_comparisons_predicates_nan_order_and_signed_zero() {
    use Code::*;
    for (code, width, predicate) in [
        (Comiss_xmm_xmmm32, 4, false),
        (Ucomiss_xmm_xmmm32, 4, false),
        (Comisd_xmm_xmmm64, 8, false),
        (Ucomisd_xmm_xmmm64, 8, false),
        (Cmpss_xmm_xmmm32_imm8, 4, true),
        (Cmpsd_xmm_xmmm64_imm8, 8, true),
        (Cmpps_xmm_xmmm128_imm8, 4, true),
        (Cmppd_xmm_xmmm128_imm8, 8, true),
    ] {
        for p in 0..if predicate { 16 } else { 1 } {
            for memory in [false, true] {
                let mut machines =
                    three(&program(code, memory, false, false, predicate.then_some(p)));
                for a in values(width) {
                    for b in values(width) {
                        for m in &mut machines {
                            seed(m, a, b, width, 0x1f80);
                        }
                        check(&mut machines, false);
                    }
                }
            }
        }
    }
}

#[test]
fn sse_conversions_rounding_indefinite_precision_and_upper_lanes() {
    use Code::*;
    for (code, from, to) in [
        (Cvtss2sd_xmm_xmmm32, 0, 1),
        (Cvtsd2ss_xmm_xmmm64, 1, 0),
        (Cvtsi2ss_xmm_rm32, 2, 0),
        (Cvtsi2sd_xmm_rm32, 2, 1),
        (Cvtss2si_r32_xmmm32, 0, 2),
        (Cvttss2si_r32_xmmm32, 0, 2),
        (Cvtsd2si_r32_xmmm64, 1, 2),
        (Cvttsd2si_r32_xmmm64, 1, 2),
        (Cvtdq2ps_xmm_xmmm128, 2, 0),
        (Cvtdq2pd_xmm_xmmm64, 2, 1),
        (Cvtps2dq_xmm_xmmm128, 0, 2),
        (Cvttps2dq_xmm_xmmm128, 0, 2),
        (Cvtpd2dq_xmm_xmmm128, 1, 2),
        (Cvttpd2dq_xmm_xmmm128, 1, 2),
        (Cvtps2pd_xmm_xmmm64, 0, 1),
        (Cvtpd2ps_xmm_xmmm128, 1, 0),
    ] {
        let integer_source = matches!(code, Cvtsi2ss_xmm_rm32 | Cvtsi2sd_xmm_rm32);
        let integer_dest = matches!(
            code,
            Cvtss2si_r32_xmmm32 | Cvttss2si_r32_xmmm32 | Cvtsd2si_r32_xmmm64 | Cvttsd2si_r32_xmmm64
        );
        let width = if from == 1 { 8 } else { 4 };
        let mut cases = if from == 2 {
            vec![0, 1, 0xffffffff, 0x7fffffff, 0x80000000, 16777217]
        } else {
            values(width)
        };
        if from != 2 {
            cases.extend(
                [
                    -2147483649.,
                    -2147483648.,
                    -2147483647.5,
                    -1.5,
                    -0.5,
                    0.5,
                    1.5,
                    16777217.,
                    2147483520.,
                    2147483647.,
                    2147483647.5,
                    2147483648.,
                ]
                .map(|v: f64| {
                    if width == 4 {
                        u64::from((v as f32).to_bits())
                    } else {
                        v.to_bits()
                    }
                }),
            );
        }
        for memory in [false, true] {
            let mut machines = three(&program(code, memory, integer_source, integer_dest, None));
            for mxcsr in [
                0x1f80, 0x1f95, 0x3f80, 0x5f80, 0x7f80, 0x9f80, 0x1fc0, 0x1f00, 0x0f80,
            ] {
                for &b in &cases {
                    for m in &mut machines {
                        seed(m, 0x5555, b, width, mxcsr);
                    }
                    check(&mut machines, from == 2 || to == 2);
                }
            }
        }
    }
}

#[test]
fn sse_controls_refuse_before_commit_and_faults_preserve_prefix() {
    for (code, width) in arithmetic() {
        let mut machines = three(&program(code, true, false, false, None));
        for control in [0x3f80, 0x5f80, 0x7f80, 0x9f80, 0x1fc0, 0x1f00, 0x0f80] {
            let before = machines[2].tier1_stats().fallback_instructions;
            let one = if width == 4 {
                u64::from(1f32.to_bits())
            } else {
                1f64.to_bits()
            };
            for m in &mut machines {
                seed(m, one, one, width, control);
            }
            check(&mut machines, false);
            assert_eq!(machines[2].tier1_stats().fallback_instructions, before + 1);
        }
        for address in [DATA + 8192, DATA + 8193] {
            for m in &mut machines {
                seed(m, 0, 0, width, 0x1f80);
                m.state.regs[6] = address;
                m.protect(DATA + 8192, 4096, 0).unwrap();
            }
            assert!(!matches!(
                check(&mut machines, false),
                RunOutcome::ReachedStop
            ));
        }
    }
}

#[test]
fn sse_precision_is_sticky_and_packed_refusal_discards_earlier_lanes() {
    let code = assemble(|a| {
        a.addps(xmm0, xmm1).unwrap();
        a.stmxcsr(dword_ptr(esi)).unwrap();
        a.ret().unwrap();
    });
    for special_lane in [None, Some(0), Some(1), Some(2), Some(3)] {
        let mut machines = three(&code);
        for m in &mut machines {
            seed(
                m,
                u64::from(1f32.to_bits()),
                u64::from((f32::EPSILON / 2.).to_bits()),
                4,
                0x1f84,
            );
            if let Some(lane) = special_lane {
                m.state.xmm[1][lane * 4..lane * 4 + 4]
                    .copy_from_slice(&0x7f812345u32.to_le_bytes());
            }
        }
        // STMXCSR makes Cranelift's documented missing arithmetic PE observable
        // in memory as well. Compare that path separately from the exact pair.
        for index in [0, 2] {
            if index == 2 {
                machines[index].set_tier_verification(Some(true));
            }
            machines[index].run(STOP, 100).unwrap();
        }
        equivalent(&machines[0], &machines[2], u32::MAX);
        assert_ne!(machines[2].state.mxcsr & 0x20, 0);
        assert_eq!(
            machines[2].tier1_stats().fallback_instructions,
            u64::from(special_lane.is_some())
        );
    }
}

#[test]
fn sse_bitwise_unpack_and_double_moves_preserve_raw_bits_under_any_control() {
    use Code::*;
    for code in [
        Andps_xmm_xmmm128,
        Andpd_xmm_xmmm128,
        Andnps_xmm_xmmm128,
        Andnpd_xmm_xmmm128,
        Orps_xmm_xmmm128,
        Orpd_xmm_xmmm128,
        Xorps_xmm_xmmm128,
        Xorpd_xmm_xmmm128,
        Unpcklps_xmm_xmmm128,
        Unpckhps_xmm_xmmm128,
        Unpcklpd_xmm_xmmm128,
        Unpckhpd_xmm_xmmm128,
        Movsd_xmm_xmmm64,
        Movapd_xmm_xmmm128,
        Movupd_xmm_xmmm128,
    ] {
        for memory in [false, true] {
            let mut machines = three(&program(code, memory, false, false, None));
            for control in [0, 0x1f80, 0xffbf] {
                for m in &mut machines {
                    seed(m, 0, 0, 4, control);
                    for n in 0..16 {
                        m.state.xmm[0][n] = (n * 17) as u8;
                        m.state.xmm[1][n] = (255 - n * 13) as u8;
                    }
                    let src = m.state.xmm[1];
                    m.write(DATA, &src).unwrap();
                }
                check(&mut machines, true);
            }
            assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
        }
    }
}

#[test]
fn sse_mxcsr_memory_changes_and_cross_page_faults_are_transactional() {
    for store in [false, true] {
        let bytes = assemble(|a| {
            a.inc(ebx).unwrap();
            if store {
                a.stmxcsr(dword_ptr(esi)).unwrap();
            } else {
                a.ldmxcsr(dword_ptr(esi)).unwrap();
            }
            a.addss(xmm0, xmm1).unwrap();
            a.stmxcsr(dword_ptr(esi + 32)).unwrap();
            a.ret().unwrap();
        });
        for value in [0x1f80u32, 0x1f95, 0x3f80, 0x5fc0, 0xffbf, 0x10000] {
            let mut machines = three(&bytes);
            for m in &mut machines {
                seed(
                    m,
                    u64::from(1f32.to_bits()),
                    u64::from(1f32.to_bits()),
                    4,
                    0x1f84,
                );
                m.write(DATA, &value.to_le_bytes()).unwrap();
            }
            check(&mut machines, true);
        }
        // Cross-page faults are compared at every byte against Tier 0. The
        // exact prefix remains committed and the failing transfer writes none.
        for tail in 1..4 {
            let mut machines = three(&bytes);
            for m in &mut machines {
                seed(m, 0, 0, 4, 0x1f84);
                m.state.regs[6] = DATA + 4096 - tail;
                m.protect(DATA + 4096, 4096, 0).unwrap();
            }
            assert!(!matches!(
                check(&mut machines, true),
                RunOutcome::ReachedStop
            ));
            assert_eq!(machines[2].state.regs[3], 12);
        }
    }
}

#[test]
fn sse_streaming_stores_keep_alignment_and_page_fault_order() {
    for code in [
        Code::Movntps_m128_xmm,
        Code::Movntpd_m128_xmm,
        Code::Movntdq_m128_xmm,
    ] {
        let bytes = assemble(|a| {
            a.add_instruction(
                Instruction::with2(
                    code,
                    MemoryOperand::with_base(Register::ESI),
                    Register::XMM0,
                )
                .unwrap(),
            )
            .unwrap();
            a.ret().unwrap();
        });
        for address in [DATA, DATA + 1, DATA + 8192, DATA + 8193] {
            let mut machines = three(&bytes);
            for m in &mut machines {
                seed(m, 0x7ff0123456789abc, 0, 8, 0);
                m.state.regs[6] = address;
                m.protect(DATA + 8192, 4096, 0).unwrap();
            }
            check(&mut machines, true);
            if address == DATA {
                assert_eq!(machines[2].tier1_stats().fallback_instructions, 0);
            }
        }
    }
}

#[test]
fn sse_each_budget_prefix_and_fault_resume_match_the_interpreter() {
    let bytes = assemble(|a| {
        a.addss(xmm0, xmm1).unwrap();
        a.cvttss2si(eax, xmm0).unwrap();
        a.comiss(xmm0, dword_ptr(esi)).unwrap();
        a.setb(cl).unwrap();
        a.mulps(xmm0, xmm1).unwrap();
        a.ret().unwrap();
    });
    let mut machines = [machine(&bytes, 0), machine(&bytes, 2)];
    for m in &mut machines {
        seed(
            m,
            u64::from(1.5f32.to_bits()),
            u64::from(1.25f32.to_bits()),
            4,
            0x1f80,
        );
    }
    machines[1].set_tier_verification(Some(true));
    for _ in 0..2 {
        for m in &mut machines {
            assert_eq!(m.run(STOP, 1).unwrap(), RunOutcome::Budget);
        }
        equivalent(&machines[0], &machines[1], u32::MAX);
    }
    for m in &mut machines {
        m.protect(DATA, 4096, 0).unwrap();
    }
    let a = machines[0].run(STOP, 1).unwrap();
    let b = machines[1].run(STOP, 1).unwrap();
    assert_eq!(a, b);
    equivalent(&machines[0], &machines[1], u32::MAX);
    for m in &mut machines {
        m.protect(DATA, 4096, READ | WRITE).unwrap();
    }
    for _ in 0..4 {
        assert_eq!(
            machines[0].run(STOP, 1).unwrap(),
            machines[1].run(STOP, 1).unwrap()
        );
        equivalent(&machines[0], &machines[1], u32::MAX);
    }
}

#[test]
fn sse_host_fp_environment_survives_success_and_refusal() {
    unsafe fn environment() -> (u64, u64) {
        let (control, status);
        unsafe {
            core::arch::asm!("mrs {0}, fpcr", "mrs {1}, fpsr", out(reg) control, out(reg) status, options(nostack));
        }
        (control, status)
    }
    unsafe fn restore((control, status): (u64, u64)) {
        unsafe {
            core::arch::asm!("msr fpcr, {0}", "msr fpsr, {1}", in(reg) control, in(reg) status, options(nostack));
        }
    }
    let bytes = program(Code::Divps_xmm_xmmm128, false, false, false, None);
    for input in [u64::from(3f32.to_bits()), 0x7f812345, 0] {
        let mut machines = [machine(&bytes, 0), machine(&bytes, 2)];
        for m in &mut machines {
            seed(m, u64::from(1f32.to_bits()), input, 4, 0x1f80);
        }
        machines[0].run(STOP, 100).unwrap();
        // SAFETY: this test thread restores both host registers before asserts.
        let (result, observed) = unsafe {
            let saved = environment();
            restore((0x03400000, 0x0800009f));
            let result = machines[1].run(STOP, 100);
            let observed = environment();
            restore(saved);
            (result, observed)
        };
        result.unwrap();
        assert_eq!(observed, (0x03400000, 0x0800009f));
        equivalent(&machines[0], &machines[1], u32::MAX);
    }
}

#[test]
fn sse_tininess_before_rounding_keeps_underflow_on_normal_results() {
    for (code, width, minimum, almost_one) in [
        (Code::Mulss_xmm_xmmm32, 4, 0x0080_0000, 0x3f7f_ffff),
        (Code::Mulps_xmm_xmmm128, 4, 0x0080_0000, 0x3f7f_ffff),
        (
            Code::Mulsd_xmm_xmmm64,
            8,
            0x0010_0000_0000_0000,
            0x3fef_ffff_ffff_ffff,
        ),
        (
            Code::Mulpd_xmm_xmmm128,
            8,
            0x0010_0000_0000_0000,
            0x3fef_ffff_ffff_ffff,
        ),
    ] {
        for negative in [false, true] {
            let sign = if negative {
                if width == 4 { 1 << 31 } else { 1 << 63 }
            } else {
                0
            };
            let one = if width == 4 {
                u64::from(1f32.to_bits())
            } else {
                1f64.to_bits()
            };
            let mut machines = three(&program(code, false, false, false, None));
            for b in [one, almost_one] {
                for m in &mut machines {
                    seed(m, minimum | sign, b, width, 0x1f84);
                }
                let before = machines[2].tier1_stats().fallback_instructions;
                check(&mut machines, false);
                let inexact = b == almost_one;
                assert_eq!(
                    machines[2].state.mxcsr & 0x30,
                    if inexact { 0x30 } else { 0 }
                );
                assert_eq!(
                    machines[2].tier1_stats().fallback_instructions,
                    before + u64::from(inexact)
                );
                assert_eq!(
                    &machines[2].state.xmm[0][..width],
                    &(minimum | sign).to_le_bytes()[..width]
                );
            }
        }
    }
    // A normal binary64 value just below the binary32 normal boundary rounds
    // to a normal single but still needs both guest UE and PE.
    for code in [Code::Cvtsd2ss_xmm_xmmm64, Code::Cvtpd2ps_xmm_xmmm128] {
        for sign in [1., -1.] {
            let source = (sign * f64::from(f32::MIN_POSITIVE) * (1. - 2f64.powi(-25))).to_bits();
            let mut machines = three(&program(code, false, false, false, None));
            for m in &mut machines {
                seed(m, 0, source, 8, 0x1f80);
            }
            check(&mut machines, false);
            assert_eq!(machines[2].state.mxcsr & 0x30, 0x30);
            assert_eq!(machines[2].tier1_stats().fallback_instructions, 1);
        }
    }
}
