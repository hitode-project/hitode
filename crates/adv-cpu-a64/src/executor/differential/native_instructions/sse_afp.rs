//! Differential and codegen tests for Tier-1 SSE optimizations under
//! `FEAT_AFP`, `FEAT_RPRES`, `FEAT_FRINTTS`, `FEAT_FlagM2`, fast NEON shuffles,
//! and constant-definition idioms, with ARMv8.0 fallbacks (A64-ISA-4,
//! A64-FLAG-6, A64F-FP-5, A64F-FP-9).

use super::*;
use crate::facts;
use crate::native::compile::Workspace;
use crate::translate::{Features, Stubs, jit};
use adv_cpu_core::HostFeatures;

fn host_fpcr() -> u32 {
    let value: u64;
    // SAFETY: reading FPCR on AArch64 EL0 is always valid.
    unsafe {
        std::arch::asm!("mrs {}, fpcr", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value as u32
}

fn host_features() -> Features {
    let hw = HostFeatures::get();
    Features {
        flagm: hw.contains(HostFeatures::FLAGM),
        flagm2: hw.contains(HostFeatures::FLAGM2),
        frintts: hw.contains(HostFeatures::FRINTTS),
        afp: hw.contains(HostFeatures::AFP),
        rpres: hw.contains(HostFeatures::RPRES),
    }
}

/// Feature configurations supported on this host, covering default plus
/// individually disabled extensions and pure ARMv8.0.
fn feature_modes() -> Vec<Features> {
    let full = host_features();
    let mut modes = vec![full];
    for candidate in [
        Features {
            afp: false,
            rpres: false,
            ..full
        },
        Features {
            rpres: false,
            ..full
        },
        Features {
            frintts: false,
            ..full
        },
        Features {
            flagm2: false,
            ..full
        },
        Features {
            flagm: false,
            flagm2: false,
            frintts: false,
            afp: false,
            rpres: false,
        },
    ] {
        if !modes.contains(&candidate) {
            modes.push(candidate);
        }
    }
    modes
}

fn run_diff(program: &Program, features: Features, checks: Checks) -> Executor {
    let before_fpcr = host_fpcr();
    let config = (features, false, checks);
    let mut native = executor(program, config, false);
    let mut oracle = executor(program, config, true);
    let actual = native.run(program.stop, 10_000).unwrap();
    let expected = oracle.run(program.stop, 10_000).unwrap();
    assert_eq!(actual, expected, "outcome mismatch under {features:?}");
    same(&native, &oracle);
    assert_eq!(
        host_fpcr(),
        before_fpcr,
        "host FPCR leaked after run under {features:?}"
    );
    native
}

fn xmm_f32(lanes: [u32; 4]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, v) in lanes.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn xmm_f64(lanes: [u64; 2]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, v) in lanes.into_iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    out
}

fn tier1_words(features: Features, mxcsr: u32, build: impl FnOnce(&mut CodeAssembler)) -> Vec<u32> {
    let mut a = CodeAssembler::new(32).unwrap();
    build(&mut a);
    let code = a.assemble(0x10000).unwrap();
    let mut memory = adv_cpu_core::Memory::new(0x10000, 4096).unwrap();
    memory.protect(0x10000, 4096, 7).unwrap();
    memory.write(0x10000, &code).unwrap();
    let cpu = CpuState {
        mxcsr,
        ..Default::default()
    };
    let facts = facts::Key::read(&cpu, facts::MXCSR);
    let mut work = Workspace::default();
    work.unit.build(
        &mut memory,
        0x10000,
        &[0x10000 + code.len() as u32],
        &|_, _| None,
        &|_| None,
        true,
    );
    jit::lower_tier1(
        &mut work.code,
        &work.unit,
        0x10000,
        &|_| false,
        0,
        features,
        0,
        Stubs {
            call: 0,
            exit: 0,
            dispatch: 0,
            lookup: 0,
            lookup_slow: 0,
            ret_miss: 0,
            access: 0,
            fault: 0,
            opt_exit: 0,
            x87: None,
        },
        false,
        facts,
        0,
    );
    work.words().to_vec()
}

// spec: A64-ISA-4, A64F-FP-5
#[test]
fn afp_arithmetic_all_rounding_modes_daz_ftz_denormals_and_nans() {
    let mxcsr_modes = [
        0x1f80u32, // default RN
        0x3f80,    // RD (-Inf)
        0x5f80,    // RU (+Inf)
        0x7f80,    // RZ (0)
        0x1fc0,    // DAZ
        0x9f80,    // FTZ
        0x9fc0,    // DAZ + FTZ
        0xbfc0,    // DAZ + FTZ + RD
    ];
    let f32_pairs: [([u32; 4], [u32; 4]); 10] = [
        (
            [
                1.5f32.to_bits(),
                (-2.25f32).to_bits(),
                0.0f32.to_bits(),
                (-0.0f32).to_bits(),
            ],
            [
                0.3f32.to_bits(),
                3.7f32.to_bits(),
                (-0.0f32).to_bits(),
                0.0f32.to_bits(),
            ],
        ),
        // Denormals + normals + tiny products
        (
            [0x0040_0000, 0x0080_0000, 0x0080_0001, 0x3f7f_fffe],
            [0x3f80_0000, 0x3f00_0000, 0x3f00_0000, 0x0080_0001],
        ),
        // Subnormal + 0, subnormal * 1
        (
            [0x0040_0000, 0x0000_0001, 0x8000_0001, 0x0080_0000],
            [0x0000_0000, 0x3f80_0000, 0x3f80_0000, 0x3e80_0000],
        ),
        // 0/0, 1/0, overflow, underflow
        (
            [0x0000_0000, 0x3f80_0000, 0x7f7f_ffff, 0x0080_0000],
            [0x0000_0000, 0x0000_0000, 0x4000_0000, 0x3eaa_aaab],
        ),
        // QNaN + QNaN, QNaN + SNaN, SNaN + QNaN, 1 + SNaN
        (
            [0x7fc0_0001, 0x7fc0_0003, 0x7f80_0005, 0x3f80_0000],
            [0x7fc0_0002, 0x7f80_0004, 0x7fc0_0006, 0x7f80_0007],
        ),
        // Positive & negative values for sqrt (including -0 and -1 -> default NaN)
        (
            [
                4.0f32.to_bits(),
                2.0f32.to_bits(),
                0x8000_0000,
                (-1.0f32).to_bits(),
            ],
            [
                9.0f32.to_bits(),
                0x0040_0000,
                0x8000_0000,
                (-4.0f32).to_bits(),
            ],
        ),
        // Infinities
        (
            [0x7f80_0000, 0xff80_0000, 0x7f80_0000, 1.0f32.to_bits()],
            [0xff80_0000, 0x7f80_0000, 0x0000_0000, 0x7f80_0000],
        ),
        // Rounding tie cases (1.5, 2.5, -1.5, -2.5)
        (
            [0x3fc0_0000, 0x4020_0000, 0xbfc0_0000, 0xc020_0000],
            [0x3380_0000, 0x3380_0000, 0xb380_0000, 0xb380_0000],
        ),
        // Exact integer conversions and narrowing inputs
        (
            [0x3f80_0001, 0xbf80_0001, 0x4b00_0001, 0xcb00_0001],
            [0x3f80_0000, 0xbf80_0000, 0x3f80_0000, 0xbf80_0000],
        ),
        // Very small normals
        (
            [0x0080_0000, 0x8080_0000, 0x0100_0000, 0x8100_0000],
            [0x3f40_0000, 0x3f40_0000, 0x3f00_0000, 0x3f00_0000],
        ),
    ];

    let f64_pairs: [([u64; 2], [u64; 2]); 5] = [
        (
            [1.5f64.to_bits(), (-2.25f64).to_bits()],
            [0.3f64.to_bits(), 3.7f64.to_bits()],
        ),
        // Tiny-before-only product & subnormal + normal
        (
            [0x3fef_ffff_ffff_fffe, 0x0008_0000_0000_0000],
            [0x0010_0000_0000_0001, 0x3ff0_0000_0000_0000],
        ),
        // 0/0 and overflow
        (
            [0x0000_0000_0000_0000, 0x7fef_ffff_ffff_ffff],
            [0x0000_0000_0000_0000, 0x4000_0000_0000_0000],
        ),
        // QNaN + SNaN and SNaN + QNaN
        (
            [0x7ff8_0000_0000_0001, 0x7ff0_0000_0000_0003],
            [0x7ff0_0000_0000_0002, 0x7ff8_0000_0000_0004],
        ),
        // Denormal double narrowing to single
        (
            [0x0008_0000_0000_0000, 0x380f_ffff_f000_0000],
            [0x3810_0000_0000_0000, 0xb808_0000_0000_0000],
        ),
    ];

    for features in feature_modes() {
        for &mxcsr in &mxcsr_modes {
            // Single-precision scalar and packed arithmetic + sqrt + conversions.
            for (a, b) in f32_pairs {
                let mut p = fixture(|asm| {
                    asm.addss(xmm0, xmm1).unwrap();
                    asm.subss(xmm2, xmm1).unwrap();
                    asm.mulss(xmm3, xmm1).unwrap();
                    asm.divss(xmm4, xmm1).unwrap();
                    asm.sqrtss(xmm5, xmm1).unwrap();
                    asm.addps(xmm6, xmm1).unwrap();
                    asm.mulps(xmm7, xmm1).unwrap();
                    asm.subps(xmm6, xmm7).unwrap();
                    asm.divps(xmm6, xmm1).unwrap();
                    asm.sqrtps(xmm7, xmm1).unwrap();
                    asm.cvtss2sd(xmm2, xmm0).unwrap();
                    asm.cvtsd2ss(xmm3, xmm2).unwrap();
                    asm.cvtps2pd(xmm4, xmm0).unwrap();
                    asm.cvtpd2ps(xmm5, xmm4).unwrap();
                    asm.cvtsi2ss(xmm0, eax).unwrap();
                    asm.cvtsi2sd(xmm1, edx).unwrap();
                    asm.cvtdq2ps(xmm2, xmm3).unwrap();
                    asm.cvtdq2pd(xmm3, xmm0).unwrap();
                });
                p.state.mxcsr = mxcsr;
                p.state.regs[0] = 0x1234_5679;
                p.state.regs[2] = 0xfedc_ba98;
                let va = xmm_f32(a);
                let vb = xmm_f32(b);
                p.state.xmm[0] = va;
                p.state.xmm[1] = vb;
                p.state.xmm[2] = va;
                p.state.xmm[3] = va;
                p.state.xmm[4] = va;
                p.state.xmm[5] = va;
                p.state.xmm[6] = va;
                p.state.xmm[7] = va;
                run_diff(&p, features, Checks::Adaptive);
            }

            // Double-precision scalar and packed arithmetic + sqrt + compares.
            for (a, b) in f64_pairs {
                let mut p = fixture(|asm| {
                    asm.addsd(xmm0, xmm1).unwrap();
                    asm.subsd(xmm2, xmm1).unwrap();
                    asm.mulsd(xmm3, xmm1).unwrap();
                    asm.divsd(xmm4, xmm1).unwrap();
                    asm.sqrtsd(xmm5, xmm1).unwrap();
                    asm.addpd(xmm6, xmm1).unwrap();
                    asm.mulpd(xmm7, xmm1).unwrap();
                    asm.subpd(xmm6, xmm7).unwrap();
                    asm.divpd(xmm6, xmm1).unwrap();
                    asm.sqrtpd(xmm7, xmm1).unwrap();
                    for pred in 0..8 {
                        asm.cmppd(xmm2, xmm1, pred).unwrap();
                        asm.cmpsd_3(xmm3, xmm1, pred).unwrap();
                    }
                });
                p.state.mxcsr = mxcsr;
                let va = xmm_f64(a);
                let vb = xmm_f64(b);
                for r in [0, 2, 3, 4, 5, 6, 7] {
                    p.state.xmm[r] = va;
                }
                p.state.xmm[1] = vb;
                run_diff(&p, features, Checks::Adaptive);
            }
        }
    }

    // Verify lazy FPCR/FPSR efficiency across a straight-line AFP chain:
    // at most 2 FPCR writes (1 initial sync + 1 root exit restore) and 1 fold.
    if host_features().afp {
        let mut p = fixture(|asm| {
            for _ in 0..8 {
                asm.addss(xmm0, xmm1).unwrap();
                asm.mulss(xmm0, xmm2).unwrap();
            }
        });
        p.state.mxcsr = 0x1f80;
        p.state.xmm[0] = xmm_f32([1.0f32.to_bits(), 0, 0, 0]);
        p.state.xmm[1] = xmm_f32([0.3f32.to_bits(), 0, 0, 0]);
        p.state.xmm[2] = xmm_f32([1.1f32.to_bits(), 0, 0, 0]);
        let native = run_diff(&p, host_features(), Checks::Adaptive);
        let fp = native.native.as_ref().unwrap().diagnostics.fp;
        assert!(
            fp.fpcr_writes <= 2,
            "straight-line AFP chain should sync once and restore once, got {}",
            fp.fpcr_writes
        );
        assert!(
            fp.fpsr_reads <= 1 && fp.fpsr_folds <= 1,
            "straight-line AFP chain should fold once at exit, got reads={} folds={}",
            fp.fpsr_reads,
            fp.fpsr_folds
        );
    }
}

// spec: A64-ISA-4, A64F-FP-5
#[test]
fn afp_minmax_signed_zeros_nans_and_fallbacks() {
    let f32_cases: [([u32; 4], [u32; 4]); 5] = [
        // (+0, -0), (-0, +0), (1, 2), (2, 1)
        (
            [0x0000_0000, 0x8000_0000, 1.0f32.to_bits(), 2.0f32.to_bits()],
            [0x8000_0000, 0x0000_0000, 2.0f32.to_bits(), 1.0f32.to_bits()],
        ),
        // (QNaN, 1), (1, QNaN), (SNaN, 1), (1, SNaN)
        (
            [0x7fc0_0001, 1.0f32.to_bits(), 0x7f80_0002, 1.0f32.to_bits()],
            [1.0f32.to_bits(), 0x7fc0_0003, 1.0f32.to_bits(), 0x7f80_0004],
        ),
        // (QNaN1, QNaN2), (SNaN1, SNaN2), (-Inf, +Inf), (+Inf, -Inf)
        (
            [0x7fc0_0001, 0x7f80_0001, 0xff80_0000, 0x7f80_0000],
            [0x7fc0_0002, 0x7f80_0002, 0x7f80_0000, 0xff80_0000],
        ),
        // Subnormals vs normals (DAZ=0 raises DE, DAZ=1 flushes to 0)
        (
            [
                0x0040_0000,
                0x8040_0000,
                1.0f32.to_bits(),
                (-1.0f32).to_bits(),
            ],
            [
                1.0f32.to_bits(),
                (-1.0f32).to_bits(),
                0x0040_0000,
                0x8040_0000,
            ],
        ),
        // Equal finite values
        (
            [
                3.5f32.to_bits(),
                (-3.5f32).to_bits(),
                0x0000_0000,
                0x8000_0000,
            ],
            [
                3.5f32.to_bits(),
                (-3.5f32).to_bits(),
                0x0000_0000,
                0x8000_0000,
            ],
        ),
    ];
    let f64_cases: [([u64; 2], [u64; 2]); 3] = [
        (
            [0x0000_0000_0000_0000, 0x8000_0000_0000_0000],
            [0x8000_0000_0000_0000, 0x0000_0000_0000_0000],
        ),
        (
            [0x7ff8_0000_0000_0001, 0x7ff0_0000_0000_0002],
            [1.0f64.to_bits(), 0x7ff0_0000_0000_0003],
        ),
        (
            [0x0008_0000_0000_0000, 2.0f64.to_bits()],
            [1.0f64.to_bits(), 0x0008_0000_0000_0000],
        ),
    ];

    for features in feature_modes() {
        for mxcsr in [0x1f80u32, 0x1fc0, 0x9f80, 0x9fc0] {
            for (a, b) in f32_cases {
                let mut p = fixture(|asm| {
                    asm.minss(xmm0, xmm1).unwrap();
                    asm.maxss(xmm2, xmm1).unwrap();
                    asm.minps(xmm3, xmm1).unwrap();
                    asm.maxps(xmm4, xmm1).unwrap();
                });
                p.state.mxcsr = mxcsr;
                let (va, vb) = (xmm_f32(a), xmm_f32(b));
                p.state.xmm[0] = va;
                p.state.xmm[1] = vb;
                p.state.xmm[2] = va;
                p.state.xmm[3] = va;
                p.state.xmm[4] = va;
                run_diff(&p, features, Checks::Adaptive);
            }
            for (a, b) in f64_cases {
                let mut p = fixture(|asm| {
                    asm.minsd(xmm0, xmm1).unwrap();
                    asm.maxsd(xmm2, xmm1).unwrap();
                    asm.minpd(xmm3, xmm1).unwrap();
                    asm.maxpd(xmm4, xmm1).unwrap();
                });
                p.state.mxcsr = mxcsr;
                let (va, vb) = (xmm_f64(a), xmm_f64(b));
                p.state.xmm[0] = va;
                p.state.xmm[1] = vb;
                p.state.xmm[2] = va;
                p.state.xmm[3] = va;
                p.state.xmm[4] = va;
                run_diff(&p, features, Checks::Adaptive);
            }
        }
        // NaN + denormal peer must suppress DE under DAZ=0 and flush denormal
        // under DAZ=1 across arithmetic, min/max, ucomis/comis, and cmp*.
        let nan_denorm_pairs: [(u32, u32); 4] = [
            (0x0040_0000, 0x7fc0_0001), // denormal, QNaN
            (0x7fc0_0001, 0x0040_0000), // QNaN, denormal
            (0x0040_0000, 0x7f80_0001), // denormal, SNaN
            (0x7f80_0001, 0x0040_0000), // SNaN, denormal
        ];
        for mxcsr in [0x1f80u32, 0x1fc0, 0x9f80] {
            for (a, b) in nan_denorm_pairs {
                for op in 0..12 {
                    let mut p = fixture(|asm| match op {
                        0 => asm.addss(xmm0, xmm1).unwrap(),
                        1 => asm.mulss(xmm0, xmm1).unwrap(),
                        2 => asm.minss(xmm0, xmm1).unwrap(),
                        3 => asm.maxss(xmm0, xmm1).unwrap(),
                        4 => asm.ucomiss(xmm0, xmm1).unwrap(),
                        5 => asm.comiss(xmm0, xmm1).unwrap(),
                        6 => asm.cmpss(xmm0, xmm1, 0).unwrap(),
                        7 => asm.cmpss(xmm0, xmm1, 1).unwrap(),
                        8 => asm.cmpss(xmm0, xmm1, 3).unwrap(),
                        9 => asm.cmpss(xmm0, xmm1, 7).unwrap(),
                        10 => asm.cmpps(xmm0, xmm1, 3).unwrap(),
                        _ => asm.cmpps(xmm0, xmm1, 7).unwrap(),
                    });
                    p.state.mxcsr = mxcsr;
                    p.state.xmm[0] = xmm_f32([a, a, 1.0f32.to_bits(), 2.0f32.to_bits()]);
                    p.state.xmm[1] = xmm_f32([b, b, 2.0f32.to_bits(), 1.0f32.to_bits()]);
                    run_diff(&p, features, Checks::Adaptive);
                }
            }
        }
    }
}

// spec: A64-ISA-4, A64-FLAG-6, A64F-FP-5
#[test]
fn flag_compare_fusion_jcc_setcc_cmovcc_and_signaling_nans() {
    let f32_inputs: [(u32, u32); 9] = [
        (1.0f32.to_bits(), 2.0f32.to_bits()), // <
        (2.0f32.to_bits(), 2.0f32.to_bits()), // ==
        (3.0f32.to_bits(), 2.0f32.to_bits()), // >
        (0x0000_0000, 0x8000_0000),           // +0 == -0
        (0x7fc0_0001, 1.0f32.to_bits()),      // QNaN vs normal
        (1.0f32.to_bits(), 0x7fc0_0002),      // normal vs QNaN
        (0x7f80_0001, 1.0f32.to_bits()),      // SNaN vs normal
        (0x0040_0000, 0x0000_0000),           // subnormal vs 0 (tests DAZ)
        (0xff80_0000, 0x7f80_0000),           // -Inf vs +Inf
    ];
    let f64_inputs: [(u64, u64); 7] = [
        (1.0f64.to_bits(), 2.0f64.to_bits()),
        (2.0f64.to_bits(), 2.0f64.to_bits()),
        (3.0f64.to_bits(), 2.0f64.to_bits()),
        (0x0000_0000_0000_0000, 0x8000_0000_0000_0000),
        (0x7ff8_0000_0000_0001, 1.0f64.to_bits()),
        (0x7ff0_0000_0000_0001, 1.0f64.to_bits()),
        (0x0008_0000_0000_0000, 0x0000_0000_0000_0000),
    ];

    for features in feature_modes() {
        for mxcsr in [0x1f80u32, 0x1fc0] {
            for signaling in [false, true] {
                for &(a, b) in &f32_inputs {
                    // Exercise SETcc, CMOVcc, Jcc, and full EFLAGS (pushfd)
                    // with every unsigned/parity/equality condition code.
                    let mut p = fixture(|asm| {
                        let emit_cmp = |asm: &mut CodeAssembler| {
                            if signaling {
                                asm.comiss(xmm0, xmm1).unwrap();
                            } else {
                                asm.ucomiss(xmm0, xmm1).unwrap();
                            }
                        };
                        // Full EFLAGS observation (SF, ZF, CF, PF, OF, AF).
                        emit_cmp(asm);
                        asm.pushfd().unwrap();
                        asm.pop(eax).unwrap();
                        // Dead-SF/PF SETcc consumers (isolated CF/ZF/PF liveness).
                        emit_cmp(asm);
                        asm.setb(cl).unwrap();
                        asm.add(edx, 1).unwrap();
                        emit_cmp(asm);
                        asm.setae(ch).unwrap();
                        asm.add(edx, 1).unwrap();
                        emit_cmp(asm);
                        asm.seta(bl).unwrap();
                        asm.add(edx, 1).unwrap();
                        emit_cmp(asm);
                        asm.setbe(bh).unwrap();
                        asm.add(edx, 1).unwrap();
                        emit_cmp(asm);
                        asm.sete(dl).unwrap();
                        asm.add(esi, 1).unwrap();
                        emit_cmp(asm);
                        asm.setp(dh).unwrap();
                        asm.add(esi, 1).unwrap();
                        // CMOVcc consumers.
                        emit_cmp(asm);
                        asm.cmovb(esi, eax).unwrap();
                        asm.cmova(edi, ecx).unwrap();
                        asm.cmovne(esi, edx).unwrap();
                        // Jcc consumers (jb, jae, ja, jbe, je, jne, jp, jnp).
                        let mut l1 = asm.create_label();
                        let mut l2 = asm.create_label();
                        let mut l3 = asm.create_label();
                        let mut l4 = asm.create_label();
                        emit_cmp(asm);
                        asm.jb(l1).unwrap();
                        asm.add(esi, 0x10).unwrap();
                        asm.set_label(&mut l1).unwrap();
                        emit_cmp(asm);
                        asm.jbe(l2).unwrap();
                        asm.add(esi, 0x20).unwrap();
                        asm.set_label(&mut l2).unwrap();
                        emit_cmp(asm);
                        asm.je(l3).unwrap();
                        asm.add(esi, 0x40).unwrap();
                        asm.set_label(&mut l3).unwrap();
                        emit_cmp(asm);
                        asm.jp(l4).unwrap();
                        asm.add(esi, 0x80).unwrap();
                        asm.set_label(&mut l4).unwrap();
                    });
                    p.state.mxcsr = mxcsr;
                    p.state.xmm[0] = xmm_f32([a, 0, 0, 0]);
                    p.state.xmm[1] = xmm_f32([b, 0, 0, 0]);
                    run_diff(&p, features, Checks::Adaptive);
                }

                for &(a, b) in &f64_inputs {
                    let mut p = fixture(|asm| {
                        let emit_cmp = |asm: &mut CodeAssembler| {
                            if signaling {
                                asm.comisd(xmm0, xmm1).unwrap();
                            } else {
                                asm.ucomisd(xmm0, xmm1).unwrap();
                            }
                        };
                        emit_cmp(asm);
                        asm.pushfd().unwrap();
                        asm.pop(eax).unwrap();
                        emit_cmp(asm);
                        asm.setb(cl).unwrap();
                        asm.add(edx, 1).unwrap();
                        emit_cmp(asm);
                        asm.setbe(ch).unwrap();
                        asm.add(edx, 1).unwrap();
                        emit_cmp(asm);
                        asm.setne(dl).unwrap();
                        asm.add(esi, 1).unwrap();
                        emit_cmp(asm);
                        asm.setnp(dh).unwrap();
                        asm.add(esi, 1).unwrap();
                        let mut skip = asm.create_label();
                        emit_cmp(asm);
                        asm.ja(skip).unwrap();
                        asm.add(edi, 0x100).unwrap();
                        asm.set_label(&mut skip).unwrap();
                    });
                    p.state.mxcsr = mxcsr;
                    p.state.xmm[0] = xmm_f64([a, 0]);
                    p.state.xmm[1] = xmm_f64([b, 0]);
                    run_diff(&p, features, Checks::Adaptive);
                }
            }
        }
    }
}

// spec: A64-ISA-4, A64F-FP-5
#[test]
fn float_to_int_frintts_and_unmasked_packed_xm_priority() {
    let f32_vals: [u32; 12] = [
        0.0f32.to_bits(),
        (-0.0f32).to_bits(),
        1.5f32.to_bits(),
        (-1.5f32).to_bits(),
        0x0040_0000,            // subnormal (must NOT raise DE!)
        0xcf00_0000,            // exact -2^31
        0x4f00_0000,            // +2^31 (out of range -> 0x8000_0000 + IE)
        3.0e10f32.to_bits(),    // large positive overflow
        (-3.0e10f32).to_bits(), // large negative overflow
        0x7f80_0000,            // +Inf
        0x7fc0_0001,            // QNaN
        0x7f80_0001,            // SNaN
    ];
    let f64_vals: [u64; 10] = [
        0.0f64.to_bits(),
        1.5f64.to_bits(),
        (-2147483648.0f64).to_bits(), // exact -2^31
        (-2147483648.5f64).to_bits(), // RN ties-to-even -> -2^31 + PE; RD -> overflow + IE
        (-2147483648.6f64).to_bits(), // RN -> overflow + IE; RZ -> -2^31 + PE
        2147483647.5f64.to_bits(),    // RN -> overflow + IE; RZ -> i32::MAX + PE
        2147483647.9f64.to_bits(),    // RZ -> i32::MAX + PE
        0x0008_0000_0000_0000,        // subnormal (no DE)
        0x7ff8_0000_0000_0001,        // QNaN
        0x7ff0_0000_0000_0001,        // SNaN
    ];

    for features in feature_modes() {
        for mxcsr in [0x1f80u32, 0x3f80, 0x5f80, 0x7f80, 0x1fc0, 0x9fc0] {
            // Scalar float-to-int + packed float-to-int (f32)
            for &lanes in f32_vals.as_chunks::<4>().0 {
                for &scalar_in in &lanes {
                    let mut p = fixture(|asm| {
                        asm.cvttss2si(eax, xmm0).unwrap();
                        asm.cvtss2si(ecx, xmm0).unwrap();
                        asm.cvttps2dq(xmm2, xmm1).unwrap();
                        asm.cvtps2dq(xmm3, xmm1).unwrap();
                    });
                    p.state.mxcsr = mxcsr;
                    p.state.xmm[0] = xmm_f32([scalar_in, 0x7fc0_0000, 0, 0]);
                    p.state.xmm[1] = xmm_f32(lanes);
                    run_diff(&p, features, Checks::Adaptive);
                }
            }
            // Scalar float-to-int + packed float-to-int (f64)
            for &lanes in f64_vals.as_chunks::<2>().0 {
                for &scalar_in in &lanes {
                    let mut p = fixture(|asm| {
                        asm.cvttsd2si(eax, xmm0).unwrap();
                        asm.cvtsd2si(ecx, xmm0).unwrap();
                        asm.cvttpd2dq(xmm2, xmm1).unwrap();
                        asm.cvtpd2dq(xmm3, xmm1).unwrap();
                    });
                    p.state.mxcsr = mxcsr;
                    p.state.xmm[0] = xmm_f64([scalar_in, 0x7ff8_0000_0000_0000]);
                    p.state.xmm[1] = xmm_f64(lanes);
                    run_diff(&p, features, Checks::Adaptive);
                }
            }
        }

        // Critical SDM §11.5.3.2 unmasked #XM pre/post flag priority:
        // lane 0 raises PE (1.5), lane 1 raises IE (qNaN). When IM=0, ONLY IE
        // commits before #XM (NOT PE) and destination XMM stays intact.
        let mixed32 = xmm_f32([
            1.5f32.to_bits(),
            0x7fc0_0000,
            2.0f32.to_bits(),
            3.0f32.to_bits(),
        ]);
        let mixed64 = xmm_f64([1.5f64.to_bits(), 0x7ff8_0000_0000_0000]);
        let sentinel = [0xa5u8; 16];
        for mxcsr in [0x0000u32, 0x1000, 0x0080, 0x1080] {
            for op in 0..4 {
                let mut p = fixture(|asm| match op {
                    0 => asm.cvttps2dq(xmm0, xmm1).unwrap(),
                    1 => asm.cvtps2dq(xmm0, xmm1).unwrap(),
                    2 => asm.cvttpd2dq(xmm0, xmm1).unwrap(),
                    _ => asm.cvtpd2dq(xmm0, xmm1).unwrap(),
                });
                p.state.mxcsr = mxcsr;
                p.state.xmm[0] = sentinel;
                p.state.xmm[1] = if op < 2 { mixed32 } else { mixed64 };
                run_diff(&p, features, Checks::Adaptive);
            }
        }

        // Unmasked arithmetic #XM (IM=0 on 0/0, ZM=0 on 1/0, PM=0 on 1+0.3):
        // destination must remain untouched and exact flags must commit.
        for (mxcsr, a, b) in [
            (0x1f00u32, 0.0f32.to_bits(), 0.0f32.to_bits()), // IM=0, 0/0 -> #XM + IE
            (0x1d80u32, 1.0f32.to_bits(), 0.0f32.to_bits()), // ZM=0, 1/0 -> #XM + ZE
            (0x0f80u32, 1.0f32.to_bits(), 3.0f32.to_bits()), // PM=0, 1/3 -> #XM + PE
        ] {
            let mut p = fixture(|asm| {
                asm.divss(xmm0, xmm1).unwrap();
            });
            p.state.mxcsr = mxcsr;
            p.state.xmm[0] = xmm_f32([a, 0x1122_3344, 0x5566_7788, 0x99aa_bbcc]);
            p.state.xmm[1] = xmm_f32([b, 0, 0, 0]);
            run_diff(&p, features, Checks::Adaptive);
        }
    }
}

// spec: A64-ISA-4, A64F-FP-5
#[test]
fn stmxcsr_ldmxcsr_and_fxsave_fxrstor_interleaving() {
    for features in feature_modes() {
        // Interleave AFP arithmetic, stmxcsr (must fold lazy FPSR), ldmxcsr
        // (must discard lazy FPSR and switch rounding/DAZ/FTZ), and unmasked #XM.
        let mut p = fixture(|asm| {
            // 1. Default RN: 1.0 / 3.0 raises PE lazily in FPSR.
            asm.divss(xmm0, xmm1).unwrap();
            // 2. stmxcsr must observe PE=1 in [DATA + 0].
            asm.stmxcsr(dword_ptr(ebx)).unwrap();
            // 3. 1.0 / 0.0 raises ZE lazily in FPSR, then ldmxcsr loads RD (0x3f80)
            //    which MUST discard the uncommitted ZE!
            asm.divss(xmm2, xmm3).unwrap();
            asm.ldmxcsr(dword_ptr(ebx + 4)).unwrap();
            // 4. Exact op 2.0 + 2.0 under RD must NOT carry over old ZE/PE;
            //    stmxcsr to [DATA + 8] must see 0x3f80.
            asm.addss(xmm4, xmm4).unwrap();
            asm.stmxcsr(dword_ptr(ebx + 8)).unwrap();
            // 5. Inexact op 1.0 / 3.0 under RD rounds toward -Inf and sets PE;
            //    stmxcsr to [DATA + 12] must see 0x3fa0.
            asm.divss(xmm5, xmm1).unwrap();
            asm.stmxcsr(dword_ptr(ebx + 12)).unwrap();
            // 6. Switch to DAZ+FTZ (0x9fc0): subnormal + 0.0 -> 0.0 with NO DE!
            asm.ldmxcsr(dword_ptr(ebx + 16)).unwrap();
            asm.addss(xmm6, xmm3).unwrap();
            asm.stmxcsr(dword_ptr(ebx + 20)).unwrap();
            // 7. Switch to unmasked IM=0 (0x1f00) inside the same unit, then
            //    run a valid op followed by packed cvttps2dq on [1.5, qNaN].
            asm.ldmxcsr(dword_ptr(ebx + 24)).unwrap();
            asm.addss(xmm4, xmm4).unwrap();
            asm.cvttps2dq(xmm7, xmm7).unwrap();
        });
        p.state.mxcsr = 0x1f80;
        p.data[4..8].copy_from_slice(&0x3f80u32.to_le_bytes()); // RD, all masked
        p.data[16..20].copy_from_slice(&0x9fc0u32.to_le_bytes()); // DAZ+FTZ, all masked
        p.data[24..28].copy_from_slice(&0x1f00u32.to_le_bytes()); // IM=0 unmasked
        p.state.xmm[0] = xmm_f32([1.0f32.to_bits(), 0, 0, 0]);
        p.state.xmm[1] = xmm_f32([3.0f32.to_bits(), 0, 0, 0]);
        p.state.xmm[2] = xmm_f32([1.0f32.to_bits(), 0, 0, 0]);
        p.state.xmm[3] = xmm_f32([0.0f32.to_bits(), 0, 0, 0]);
        p.state.xmm[4] = xmm_f32([2.0f32.to_bits(), 0, 0, 0]);
        p.state.xmm[5] = xmm_f32([1.0f32.to_bits(), 0, 0, 0]);
        p.state.xmm[6] = xmm_f32([0x0040_0000, 0, 0, 0]);
        p.state.xmm[7] = xmm_f32([1.5f32.to_bits(), 0x7fc0_0000, 0, 0]);
        run_diff(&p, features, Checks::Adaptive);

        // Reserved bits in LDMXCSR must raise #GP WITHOUT discarding prior lazy FPSR.
        let mut gp = fixture(|asm| {
            asm.divss(xmm0, xmm1).unwrap(); // raises PE lazily
            asm.ldmxcsr(dword_ptr(ebx)).unwrap(); // reserved bit 16 -> #GP
        });
        gp.state.mxcsr = 0x1f80;
        gp.data[0..4].copy_from_slice(&0x0001_1f80u32.to_le_bytes());
        gp.state.xmm[0] = xmm_f32([1.0f32.to_bits(), 0, 0, 0]);
        gp.state.xmm[1] = xmm_f32([3.0f32.to_bits(), 0, 0, 0]);
        run_diff(&gp, features, Checks::Adaptive);

        // FXSAVE / FXRSTOR interleaving with lazy FPSR and FPCR mode changes.
        let mut fx = fixture(|asm| {
            asm.divss(xmm0, xmm1).unwrap(); // sets PE lazily
            asm.fxsave(ptr(ebx)).unwrap(); // must fold PE into saved image!
            asm.divss(xmm2, xmm3).unwrap(); // sets ZE lazily
            asm.fxrstor(ptr(ebx)).unwrap(); // restores image (PE=1, ZE=0, RD=0)
            asm.stmxcsr(dword_ptr(ebx + 512)).unwrap();
            asm.addss(xmm0, xmm1).unwrap();
        });
        fx.state.mxcsr = 0x1f80;
        fx.state.xmm[0] = xmm_f32([1.0f32.to_bits(), 0, 0, 0]);
        fx.state.xmm[1] = xmm_f32([3.0f32.to_bits(), 0, 0, 0]);
        fx.state.xmm[2] = xmm_f32([1.0f32.to_bits(), 0, 0, 0]);
        fx.state.xmm[3] = xmm_f32([0.0f32.to_bits(), 0, 0, 0]);
        run_diff(&fx, features, Checks::Adaptive);
    }
}

// spec: A64-ISA-4, A64F-FP-5, A64F-FP-9
#[test]
fn rcp_and_rsqrt_all_2048_buckets_and_specials() {
    // Build a buffer containing all 2048 rcp mantissa buckets (positive + negative
    // exponents) and all 2048 rsqrt buckets (1024 mantissas * even/odd exponent)
    // plus all architectural special values, processed in a guest loop.
    let mut inputs: Vec<u32> = Vec::with_capacity(4200);
    // Special values: ±0, ±subnormals, ±Inf, QNaN, SNaN, -1, exp=252/253/254, max
    inputs.extend_from_slice(&[
        0x0000_0000,
        0x8000_0000,
        0x0000_0001,
        0x8000_0001,
        0x0040_0000,
        0x8040_0000,
        0x007f_ffff,
        0x807f_ffff,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0001,
        0xffc0_0002,
        0x7f80_0001,
        0xff80_0002,
        0xbf80_0000,
        0x3f80_0000,
        0x7e00_0000,
        0x7e7f_ffff,
        0x7e80_0000,
        0x7f00_0000,
        0x7f7f_ffff,
        0xfe80_0000,
        0xff7f_ffff,
        0x0080_0000,
    ]);
    // All 2048 rcp buckets (bits 22:12 = 0..2048) with varying exponents and signs.
    for i in 0..2048u32 {
        let exp = 1 + (i % 251);
        let sign = (i & 1) << 31;
        inputs.push(sign | (exp << 23) | (i << 12) | ((i * 37) & 0xfff));
    }
    // All 2048 rsqrt buckets (bits 22:13 = 0..1024, both even and odd exponents).
    for i in 0..1024u32 {
        let low = (i * 131) & 0x1fff;
        inputs.push((126 << 23) | (i << 13) | low);
        inputs.push((127 << 23) | (i << 13) | low);
    }
    while !inputs.len().is_multiple_of(4) {
        inputs.push(1.0f32.to_bits());
    }
    let vec_count = (inputs.len() / 4) as u32;

    for features in feature_modes() {
        let mut p = fixture(|asm| {
            // Prime lazy SSE FPSR first (exact op -> 0 sticky bits) to verify
            // frecpe/frsqrte under AH=1 never pollute FPSR/MXCSR.
            asm.addss(xmm7, xmm7).unwrap();
            let mut loop_head = asm.create_label();
            asm.set_label(&mut loop_head).unwrap();
            asm.movups(xmm0, xmmword_ptr(esi)).unwrap();
            asm.rcpps(xmm1, xmm0).unwrap();
            asm.rsqrtps(xmm2, xmm0).unwrap();
            asm.rcpss(xmm3, xmm0).unwrap();
            asm.rsqrtss(xmm4, xmm0).unwrap();
            asm.pxor(xmm5, xmm1).unwrap();
            asm.pxor(xmm6, xmm2).unwrap();
            asm.paddd(xmm5, xmm3).unwrap();
            asm.paddd(xmm6, xmm4).unwrap();
            asm.add(esi, 16).unwrap();
            asm.dec(ecx).unwrap();
            asm.jnz(loop_head).unwrap();
            asm.stmxcsr(dword_ptr(edi)).unwrap();
        });
        p.data = vec![0u8; 64 * 1024];
        for (idx, &word) in inputs.iter().enumerate() {
            p.data[idx * 4..idx * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        p.state.mxcsr = 0x1f80;
        p.state.regs[1] = vec_count; // ECX
        p.state.regs[6] = DATA; // ESI = inputs
        p.state.regs[7] = DATA + 32 * 1024; // EDI = stmxcsr out
        p.state.xmm[3] = xmm_f32([0, 0x1111_2222, 0x3333_4444, 0x5555_6666]);
        p.state.xmm[4] = xmm_f32([0, 0x7777_8888, 0x9999_aaaa, 0xbbbb_cccc]);
        p.state.xmm[7] = xmm_f32([2.0f32.to_bits(), 0, 0, 0]);
        let before_fpcr = host_fpcr();
        let config = (features, false, Checks::Adaptive);
        let mut native = executor(&p, config, false);
        let mut oracle = executor(&p, config, true);
        let actual = native.run(p.stop, 1_000_000).unwrap();
        let expected = oracle.run(p.stop, 1_000_000).unwrap();
        assert_eq!(actual, expected, "rcp/rsqrt outcome under {features:?}");
        same(&native, &oracle);
        assert_eq!(
            native.state.mxcsr, 0x1f80,
            "rcp/rsqrt must never set MXCSR flags"
        );
        assert_eq!(host_fpcr(), before_fpcr);
    }
}

// spec: A64-ISA-4
#[test]
fn shufps_pshufd_and_pshufw_all_256_immediates() {
    let features = host_features();
    let a = xmm_f32([0x1011_1213, 0x2021_2223, 0x3031_3233, 0x4041_4243]);
    let b = xmm_f32([0x5051_5253, 0x6061_6263, 0x7071_7273, 0x8081_8283]);

    // Test all 256 immediates in batches of 32 for fast execution:
    // - pshufd dst != src and dst == src
    // - shufps adjacent (s = d + 1), reversed adjacent (d = s + 1), non-adjacent,
    //   same-register (d == s), and memory source
    // - pshufw dst != src and dst == src
    for base in (0..=255u32).step_by(32) {
        let mut p = fixture(|asm| {
            for imm in base..base + 32 {
                let imm = imm as i32;
                asm.pshufd(xmm4, xmm0, imm).unwrap();
                asm.pxor(xmm7, xmm4).unwrap();
                asm.movdqa(xmm4, xmm0).unwrap();
                asm.pshufd(xmm4, xmm4, imm).unwrap();
                asm.pxor(xmm7, xmm4).unwrap();

                // Adjacent s = d + 1 (xmm2, xmm3)
                asm.movdqa(xmm2, xmm0).unwrap();
                asm.shufps(xmm2, xmm3, imm).unwrap();
                asm.pxor(xmm7, xmm2).unwrap();

                // Reversed adjacent d = s + 1 (xmm5, xmm4)
                asm.movdqa(xmm4, xmm1).unwrap();
                asm.movdqa(xmm5, xmm0).unwrap();
                asm.shufps(xmm5, xmm4, imm).unwrap();
                asm.pxor(xmm7, xmm5).unwrap();

                // Non-adjacent (xmm2, xmm5)
                asm.movdqa(xmm2, xmm0).unwrap();
                asm.movdqa(xmm5, xmm1).unwrap();
                asm.shufps(xmm2, xmm5, imm).unwrap();
                asm.pxor(xmm7, xmm2).unwrap();

                // Same register (xmm2, xmm2)
                asm.movdqa(xmm2, xmm0).unwrap();
                asm.shufps(xmm2, xmm2, imm).unwrap();
                asm.pxor(xmm7, xmm2).unwrap();

                // Memory source
                asm.movdqa(xmm2, xmm0).unwrap();
                asm.shufps(xmm2, xmmword_ptr(ebx), imm).unwrap();
                asm.pxor(xmm7, xmm2).unwrap();

                // PSHUFW (64-bit MMX)
                asm.pshufw(mm2, mm0, imm).unwrap();
                asm.pxor(mm3, mm2).unwrap();
                asm.movq(mm2, mm0).unwrap();
                asm.pshufw(mm2, mm2, imm).unwrap();
                asm.pxor(mm3, mm2).unwrap();
            }
        });
        p.data[..16].copy_from_slice(&b);
        p.state.xmm[0] = a;
        p.state.xmm[1] = b;
        p.state.xmm[3] = b;
        p.state.mmx[0] = 0x1122_3344_5566_7788;
        p.state.mmx[1] = 0x99aa_bbcc_ddee_ff00;
        mmx_data(&mut p.state);
        run_diff(&p, features, Checks::Adaptive);
    }
}

// spec: A64-ISA-4, A64F-FP-4
#[test]
fn zero_and_all_ones_idioms_avoid_dead_reads_and_match_interpreter() {
    // Differential check on both XMM and MMX registers initialized to non-zero patterns.
    let mut p = fixture(|asm| {
        asm.pxor(xmm0, xmm0).unwrap();
        asm.xorps(xmm1, xmm1).unwrap();
        asm.xorpd(xmm2, xmm2).unwrap();
        asm.pandn(xmm3, xmm3).unwrap();
        asm.pcmpeqb(xmm4, xmm4).unwrap();
        asm.pcmpeqw(xmm5, xmm5).unwrap();
        asm.pcmpeqd(xmm6, xmm6).unwrap();
        asm.andnpd(xmm7, xmm7).unwrap();
        asm.pxor(mm0, mm0).unwrap();
        asm.pandn(mm1, mm1).unwrap();
        asm.pcmpeqb(mm2, mm2).unwrap();
        asm.pcmpeqw(mm3, mm3).unwrap();
        asm.pcmpeqd(mm4, mm4).unwrap();
    });
    for r in 0..8 {
        p.state.xmm[r] = [0x5a; 16];
        p.state.mmx[r] = 0x1234_5678_9abc_def0;
    }
    mmx_data(&mut p.state);
    run_diff(&p, host_features(), Checks::Adaptive);
}

// spec: A64-ISA-4, A64-FLAG-6, A64F-FP-5, A64F-FP-9
#[test]
fn tier1_hot_path_instruction_counts_shrink_under_afp_rpres_frintts_flagm2() {
    let full = Features {
        flagm: true,
        flagm2: true,
        frintts: true,
        afp: true,
        rpres: true,
    };
    let legacy = Features {
        flagm: true,
        flagm2: false,
        frintts: false,
        afp: false,
        rpres: false,
    };

    // Measure marginal steady-state A64 words per guest instruction/pair using
    // (9 repetitions - 1 repetition) / 8 (plan.md §1.1).
    let marginal = |features: Features, build_one: fn(&mut CodeAssembler)| -> usize {
        let w1 = tier1_words(features, 0x1f80, build_one).len();
        let w9 = tier1_words(features, 0x1f80, |a| {
            for _ in 0..9 {
                build_one(a);
            }
        })
        .len();
        assert!(w9 >= w1);
        (w9 - w1) / 8
    };

    // addss: 2 words/op (fadd s + ins) vs legacy ~69 words.
    let addss_afp = marginal(full, |a| a.addss(xmm0, xmm1).unwrap());
    let addss_v8 = marginal(legacy, |a| a.addss(xmm0, xmm1).unwrap());
    assert!(
        addss_afp <= 2 && addss_afp * 10 < addss_v8,
        "addss: afp={addss_afp}, v8={addss_v8}"
    );

    // addps: 1 word/op (fadd v.4s).
    let addps_afp = marginal(full, |a| a.addps(xmm0, xmm1).unwrap());
    assert_eq!(addps_afp, 1, "addps under AFP should emit 1 word/op");

    // minps: 1 word/op (fmin v.4s).
    let minps_afp = marginal(full, |a| a.minps(xmm0, xmm1).unwrap());
    assert_eq!(minps_afp, 1, "minps under AFP should emit 1 word/op");

    // ucomiss + setb: 6 words/pair when PF/AF are observed (fcmp + cset vc +
    // movz w11 + axflag + cset lo + bfi) vs legacy ~134 words; with a
    // flag-killing ALU op after setb, PF/AF are dead so ucomiss+setb is 4 words!
    let ucomiss_setb_afp = marginal(full, |a| {
        a.ucomiss(xmm0, xmm1).unwrap();
        a.setb(al).unwrap();
    });
    let ucomiss_setb_v8 = marginal(legacy, |a| {
        a.ucomiss(xmm0, xmm1).unwrap();
        a.setb(al).unwrap();
    });
    assert!(
        ucomiss_setb_afp <= 6 && ucomiss_setb_afp * 10 < ucomiss_setb_v8,
        "ucomiss+setb: afp={ucomiss_setb_afp}, v8={ucomiss_setb_v8}"
    );
    let ucomiss_setb_dead_pf_afp = marginal(full, |a| {
        a.ucomiss(xmm0, xmm1).unwrap();
        a.setb(al).unwrap();
        a.add(edx, 1).unwrap();
    });
    let add_only = marginal(full, |a| a.add(edx, 1).unwrap());
    assert!(
        ucomiss_setb_dead_pf_afp - add_only <= 4,
        "ucomiss+setb with dead PF/AF should emit <= 4 words, got {}",
        ucomiss_setb_dead_pf_afp - add_only
    );

    // rcpps: 1 word/op (frecpe v.4s) vs fallback ~118 words.
    let rcpps_rpres = marginal(full, |a| a.rcpps(xmm0, xmm1).unwrap());
    let rcpps_v8 = marginal(legacy, |a| a.rcpps(xmm0, xmm1).unwrap());
    assert!(
        rcpps_rpres == 1 && rcpps_v8 > 50,
        "rcpps: rpres={rcpps_rpres}, v8={rcpps_v8}"
    );

    // cvttss2si: 2 words/op (frint32z + fcvtzs).
    let cvttss2si_afp = marginal(full, |a| a.cvttss2si(eax, xmm1).unwrap());
    assert_eq!(
        cvttss2si_afp, 2,
        "cvttss2si under AFP+FRINTTS should emit 2 words/op"
    );

    // cvttps2dq: 2 words/op (frint32z + fcvtzs).
    let cvttps2dq_afp = marginal(full, |a| a.cvttps2dq(xmm0, xmm1).unwrap());
    assert_eq!(
        cvttps2dq_afp, 2,
        "cvttps2dq under AFP+FRINTTS should emit 2 words/op"
    );

    // shufps common immediates (0x4e -> ext #8, 0x88 -> uzp1): 1 word/op.
    assert_eq!(marginal(full, |a| a.shufps(xmm0, xmm1, 0x4e).unwrap()), 1);
    assert_eq!(marginal(full, |a| a.shufps(xmm0, xmm1, 0x88).unwrap()), 1);

    // xorps xmm0, xmm0: 1 word/op (movi v16.2d, #0).
    assert_eq!(marginal(full, |a| a.xorps(xmm0, xmm0).unwrap()), 1);
}
