//! Differential tests for Tier-1 x87 optimizations: native `fcomi*`/`fucomi*`,
//! lazy C1/C0–C3 recipes and `fnstsw` C1 elision, PC24 binary32 fact tracking,
//! `_ftol` `FLDCW` sandwiches with `FEAT_FRINTTS` and ARMv8.0 fallback,
//! cross-unit / SSE+x87 residency and FPSR/FPCR interleaving, and `fld m32/m64`
//! denormal `SW.DE` reporting.
//!
//! spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5

use super::*;
use adv_cpu_core::{HostFeatures, X87Register};

fn reg_f64(value: f64) -> X87Register {
    let bits = value.to_bits();
    let sign = ((bits >> 63) as u16) << 15;
    let exp = ((bits >> 52) & 0x7ff) as u16;
    let frac = bits & ((1u64 << 52) - 1);
    match exp {
        0 => X87Register::from_parts(0, sign),
        0x7ff => {
            if frac == 0 {
                X87Register::from_parts(1u64 << 63, sign | 0x7fff)
            } else {
                X87Register::from_parts((1u64 << 63) | (frac << 11), sign | 0x7fff)
            }
        }
        _ => X87Register::from_parts((1u64 << 63) | (frac << 11), sign | (exp + (0x3fff - 1023))),
    }
}

fn test_configs(flagm2_variants: bool, frintts_variants: bool, afp_variants: bool) -> Vec<Config> {
    let host = HostFeatures::get();
    let detected = Features {
        flagm: host.contains(HostFeatures::FLAGM),
        flagm2: host.contains(HostFeatures::FLAGM2),
        frintts: host.contains(HostFeatures::FRINTTS),
        afp: host.contains(HostFeatures::AFP),
        rpres: host.contains(HostFeatures::RPRES),
    };
    let flagm2s: &[bool] = if flagm2_variants && detected.flagm2 {
        &[true, false]
    } else {
        &[false]
    };
    let frinttss: &[bool] = if frintts_variants && detected.frintts {
        &[true, false]
    } else {
        &[false]
    };
    let afps: &[bool] = if afp_variants && detected.afp {
        &[true, false]
    } else {
        &[false]
    };
    let mut out = Vec::new();
    for &flagm2 in flagm2s {
        for &frintts in frinttss {
            for &afp in afps {
                out.push((
                    Features {
                        flagm2,
                        frintts,
                        afp,
                        ..detected
                    },
                    false,
                    Checks::Adaptive,
                ));
            }
        }
    }
    out
}

fn run_diff(program: &Program, config: Config, budget: u64) {
    let mut native = executor(program, config, false);
    let mut oracle = executor(program, config, true);
    let actual = native.run(program.stop, budget);
    let expected = oracle.run(program.stop, budget);
    assert_eq!(actual, expected, "exit mismatch under {:?}", config.0);
    same(&native, &oracle);
}

/// Executes `program` twice in the same native executor so Tier-1 promotion
/// compiles the hot unit, comparing the second (Tier-1) run against the
/// interpreter.
fn run_diff_tier1(program: &Program, config: Config, budget: u64) {
    let mut native = executor(program, config, false);
    let mut oracle = executor(program, config, true);
    for _ in 0..2 {
        native.state = program.state.clone();
        oracle.state = program.state.clone();
        load(native.memory_mut(), program);
        load(oracle.memory_mut(), program);
        let actual = native.run(program.stop, budget);
        let expected = oracle.run(program.stop, budget);
        assert_eq!(actual, expected, "exit mismatch under {:?}", config.0);
        same(&native, &oracle);
    }
}

// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5
#[test]
fn fcomi_fucomi_variants_ordered_unordered_c1_and_consumers() {
    // Values cover: ordered (<, =, >), +0/-0, equal 53-bit sticky doubles
    // differing in low 64-bit significand bits, +-Inf, QNaN, and SNaN.
    let pairs: &[(X87Register, X87Register)] = &[
        (reg_f64(1.5), reg_f64(2.5)),
        (reg_f64(2.5), reg_f64(2.5)),
        (reg_f64(3.5), reg_f64(2.5)),
        (reg_f64(-0.0), reg_f64(0.0)),
        (reg_f64(-5.0), reg_f64(-2.0)),
        // 64-bit significand tie-breakers (positive and negative).
        (
            X87Register::from_parts(0x8000_0000_0000_0001, 0x3fff),
            X87Register::from_parts(0x8000_0000_0000_0002, 0x3fff),
        ),
        (
            X87Register::from_parts(0x8000_0000_0000_0002, 0x3fff),
            X87Register::from_parts(0x8000_0000_0000_0001, 0x3fff),
        ),
        (
            X87Register::from_parts(0x8000_0000_0000_0001, 0xbfff),
            X87Register::from_parts(0x8000_0000_0000_0002, 0xbfff),
        ),
        (
            X87Register::from_parts(0x8000_0000_0000_0001, 0x3fff),
            X87Register::from_parts(0x8000_0000_0000_0001, 0x3fff),
        ),
        // Inf, QNaN, SNaN.
        (reg_f64(f64::INFINITY), reg_f64(1.0)),
        (reg_f64(f64::NEG_INFINITY), reg_f64(f64::INFINITY)),
        (reg_f64(f64::INFINITY), reg_f64(f64::INFINITY)),
        (
            X87Register::from_parts(0xc000_0000_0000_0000, 0x7fff), // QNaN
            reg_f64(1.0),
        ),
        (
            reg_f64(1.0),
            X87Register::from_parts(0xc000_0000_0000_0001, 0xffff), // -QNaN
        ),
        (
            X87Register::from_parts(0x8000_0000_0000_0001, 0x7fff), // SNaN
            reg_f64(1.0),
        ),
    ];

    for variant in 0..4u8 {
        for &(a, b) in pairs {
            for &control in &[0x037fu16, 0x037eu16] {
                // Both standalone (pre-populated stack) and cached-run (with Jcc/SETcc/CMOVcc).
                let mut program = fixture(|code| {
                    let mut below = code.create_label();
                    let mut done = code.create_label();
                    match variant {
                        0 => code.fcomi(st0, st1).unwrap(),
                        1 => code.fucomi(st0, st1).unwrap(),
                        2 => code.fcomip(st0, st1).unwrap(),
                        _ => code.fucomip(st0, st1).unwrap(),
                    }
                    code.setb(al).unwrap();
                    code.sete(ah).unwrap();
                    code.setp(cl).unwrap();
                    code.seta(ch).unwrap();
                    code.mov(edx, 0x1111_0000).unwrap();
                    code.mov(esi, 0x2222_0000).unwrap();
                    code.cmovb(edx, esi).unwrap();
                    code.jb(below).unwrap();
                    code.mov(edi, 1).unwrap();
                    code.jmp(done).unwrap();
                    code.set_label(&mut below).unwrap();
                    code.mov(edi, 2).unwrap();
                    code.set_label(&mut done).unwrap();
                });
                // Pre-set C1=1, C0=1, C2=1, C3=1 and OF=SF=AF=1 to verify
                // fcomi* preserves SW.C0..C3 (especially C1=1) and clears OF/SF/AF.
                program.state.x87.set_control(control);
                program.state.x87.status = 0x4700;
                program.state.x87.push(b);
                program.state.x87.push(a);
                program.state.eflags = 0x0ed7; // OF=1, SF=1, AF=1, ZF=1, PF=1, CF=1
                for config in test_configs(true, false, false) {
                    run_diff_tier1(&program, config, 200);
                }
            }
        }
    }
}

// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5
#[test]
fn fcomi_inside_cached_x87_loop_preserves_c1_vs_fcom_clearing_c1() {
    // Runs inside a hot x87 loop where `fld` + `fadd` + `fcomip` + `jb` keeps
    // pair registers resident and hands NZCV straight to `jb`.
    let mut program = fixture(|code| {
        let mut loop_head = code.create_label();
        let mut skip = code.create_label();
        code.mov(ecx, 12).unwrap();
        code.xor(eax, eax).unwrap();
        code.set_label(&mut loop_head).unwrap();
        code.fld(dword_ptr(ebx)).unwrap(); // 1.0
        code.fadd(dword_ptr(ebx + 4)).unwrap(); // + 2^-24 + 2^-25 -> rounds up (C1=1) under PC24
        code.fld(dword_ptr(ebx + 8)).unwrap(); // 1.0 + 2^-23
        code.fxch(st0, st1).unwrap();
        // Set C1=1 via an inexact round-up right before fcomip.
        code.fadd(dword_ptr(ebx + 4)).unwrap();
        code.fcomip(st0, st1).unwrap(); // MUST preserve C1=1!
        code.jb(skip).unwrap();
        code.inc(eax).unwrap();
        code.set_label(&mut skip).unwrap();
        code.fstp(st0).unwrap();
        code.dec(ecx).unwrap();
        code.jnz(loop_head).unwrap();
        // Also compare with `fcom` which MUST clear C1=0.
        code.fld(dword_ptr(ebx)).unwrap();
        code.fadd(dword_ptr(ebx + 4)).unwrap(); // C1=1
        code.fcom_2(st0, st0).unwrap(); // C1=0
        code.fnstsw(ax).unwrap();
        code.fstp(st0).unwrap();
    });
    program.state.x87.set_control(0x007f); // PC24, RN, masked
    program.state.x87.status |= 0x20; // PE already set
    let vals: [f32; 3] = [1.0, f32::from_bits(0x33c0_0000), 1.0 + 2.0f32.powi(-23)];
    for (i, v) in vals.iter().enumerate() {
        program.data[i * 4..(i + 1) * 4].copy_from_slice(&v.to_le_bytes());
    }
    for config in test_configs(true, false, false) {
        run_diff_tier1(&program, config, 2000);
    }
}

// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5
#[test]
fn lazy_c1_recipes_and_fnstsw_mask_elision() {
    // Tests:
    // 1. Full `fnstsw ax` observing C1 from lazy C1Round (Add/Sub/Mul/Div) under PC53 & PC24.
    // 2. `fnstsw ax; and ah, 0x45` eliding C1 into AX while preserving C1 on exit.
    // 3. `fnstsw ax; test ah, 0x45; mov eax, 0` (AX dead) vs live AX after `test`.
    // 4. `fcom st(1)` lazy Compare followed by C1-only writer (`fadd`) and `fnstsw ax`.
    let pairs_f64: &[(f64, f64)] = &[
        (1.0, 2.0f64.powi(-53) + 2.0f64.powi(-60)), // round up -> C1=1 (PC53)
        (1.0, 2.0f64.powi(-54)),                    // round down -> C1=0 (PC53)
        (-1.0, -2.0f64.powi(-53) - 2.0f64.powi(-60)), // negative round up -> C1=1
        (-1.0, 2.0f64.powi(-100)),                  // tiny opposite sign -> C1=0
        (1.0, -2.0f64.powi(-100)), // tiny opposite sign -> C1=1? no, |1-eps| < 1 -> C1=1!
        (3.0, 7.0),                // division inexact
        (1.0 + 2.0f64.powi(-23), 1.0 + 2.0f64.powi(-23)), // PC24 mul
    ];

    for &control in &[0x027fu16, 0x007fu16] {
        for &(a, b) in pairs_f64 {
            for mode in 0..5u8 {
                let mut program = fixture(|code| {
                    if control == 0x007f {
                        code.fld(dword_ptr(ebx)).unwrap();
                        code.fld(dword_ptr(ebx + 4)).unwrap();
                    } else {
                        code.fld(qword_ptr(ebx)).unwrap();
                        code.fld(qword_ptr(ebx + 8)).unwrap();
                    }
                    match mode {
                        0 => {
                            // Full C1 observation via fnstsw ax after Add/Sub/Mul/Div.
                            code.fadd_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                            code.mov(ecx, eax).unwrap();
                            code.fsub_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                            code.mov(edx, eax).unwrap();
                            code.fmul_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                            code.mov(esi, eax).unwrap();
                            code.fdiv_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                        }
                        1 => {
                            // `fnstsw ax; and ah, 0x45` masks out C1 (bit 9 = ah bit 1).
                            // Final exit still publishes exact C1 in CpuState::x87.status!
                            code.fadd_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                            code.and(ah, 0x45).unwrap();
                        }
                        2 => {
                            // `fnstsw ax; test ah, 0x45` with AX overwritten right after (dead).
                            code.fmul_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                            code.test(ah, 0x45).unwrap();
                            code.setnz(cl).unwrap();
                            code.xor(eax, eax).unwrap();
                        }
                        3 => {
                            // `fnstsw ax; test ah, 0x45` with AX LIVE afterwards (MUST preserve C1 in AX!).
                            code.fdiv_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                            code.test(ah, 0x45).unwrap();
                            code.mov(ecx, eax).unwrap();
                        }
                        _ => {
                            // Lazy Compare followed by C1-only writer (`fadd`) must preserve C0/C2/C3.
                            code.fcom_2(st0, st1).unwrap();
                            code.fadd_2(st0, st1).unwrap();
                            code.fnstsw(ax).unwrap();
                            code.fcom_2(st0, st1).unwrap();
                            code.fcom_2(st0, st0).unwrap(); // kills earlier Compare
                            code.fnstsw(word_ptr(ebx + 16)).unwrap();
                        }
                    }
                    code.fstp(st0).unwrap();
                    code.fstp(st0).unwrap();
                });
                program.state.x87.set_control(control);
                program.state.x87.status |= 0x20; // PE already set -> triggers lazy C1Round
                if control == 0x007f {
                    program.data[0..4].copy_from_slice(&(a as f32).to_le_bytes());
                    program.data[4..8].copy_from_slice(&(b as f32).to_le_bytes());
                } else {
                    program.data[0..8].copy_from_slice(&a.to_le_bytes());
                    program.data[8..16].copy_from_slice(&b.to_le_bytes());
                }
                for config in test_configs(false, false, false) {
                    run_diff_tier1(&program, config, 500);
                }
            }
        }
    }
}

// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5
#[test]
fn pc24_binary32_exactness_subnormal_and_15bit_overflow_boundaries() {
    // 1. Chained binary32 arithmetic under PC24 (hits sfloat fast path and f32_normal fstp).
    // 2. Subnormal boundary: 2^-126 * 0.75 and (2 - 2^-23)*2^-126 * (0.5 + 2^-23)
    //    must keep 15-bit x87 exponent precision (no f32 double-rounding at 2^-126!).
    // 3. Overflow boundary: f32::MAX * 2.0 * 0.25 must keep 15-bit exponent (no +Inf!).
    // 4. Non-binary32 wide operands (fld qword / fild dword) mixed with fld dword under PC24.
    let cases: &[(f32, f32, f32)] = &[
        (1.25, 3.5, -0.75),
        (1.0, 2.0f32.powi(-24) * 1.5, 7.0),
        (f32::MIN_POSITIVE, 0.75, 4.0), // 2^-126 * 0.75 -> subnormal in f32, normal in x87!
        (
            f32::from_bits(0x00ff_ffff), // (2 - 2^-23) * 2^-126
            0.5 + 2.0f32.powi(-23),
            2.0,
        ),
        (f32::MAX, 2.0, 0.25), // f32::MAX * 2.0 overflows f32, stays finite in x87!
        (-f32::MAX, f32::MAX, 2.0f32.powi(-120)), // deep extended exponent!
    ];

    for &(a, b, c) in cases {
        for &pe_init in &[0u16, 0x20u16] {
            let mut program = fixture(|code| {
                code.fld(dword_ptr(ebx)).unwrap();
                code.fld(dword_ptr(ebx + 4)).unwrap();
                code.fmulp(st1, st0).unwrap();
                code.fld(dword_ptr(ebx + 8)).unwrap();
                code.fmulp(st1, st0).unwrap();
                code.fld(dword_ptr(ebx)).unwrap();
                code.faddp(st1, st0).unwrap();
                code.fstp(dword_ptr(ebx + 16)).unwrap();
            });
            program.state.x87.set_control(0x007f); // PC24, RN, masked
            program.state.x87.status |= pe_init;
            program.data[0..4].copy_from_slice(&a.to_le_bytes());
            program.data[4..8].copy_from_slice(&b.to_le_bytes());
            program.data[8..12].copy_from_slice(&c.to_le_bytes());
            for config in test_configs(false, false, false) {
                run_diff_tier1(&program, config, 500);
            }
        }
    }

    // Mix wide f64 and i32 operands with f32 operands under PC24, plus fstp m32 under
    // directed rounding and PM=0 for known f32_normal values.
    for &control in &[0x007fu16, 0x047fu16, 0x087fu16, 0x0c7fu16, 0x005fu16] {
        let mut program = fixture(|code| {
            code.fld(dword_ptr(ebx)).unwrap();
            code.fstp(dword_ptr(ebx + 16)).unwrap(); // exact f32_normal store even with PM=0 / directed RC
            code.fld(qword_ptr(ebx + 8)).unwrap(); // wide f64
            code.fadd(dword_ptr(ebx)).unwrap();
            code.fiadd(dword_ptr(ebx + 24)).unwrap(); // 32-bit int (>24 bits)
            code.fstp(qword_ptr(ebx + 32)).unwrap();
        });
        program.state.x87.set_control(control);
        program.data[0..4].copy_from_slice(&1.375f32.to_le_bytes());
        program.data[8..16].copy_from_slice(&std::f64::consts::PI.to_le_bytes());
        program.data[24..28].copy_from_slice(&0x1234_5679i32.to_le_bytes());
        for config in test_configs(false, false, false) {
            run_diff_tier1(&program, config, 500);
        }
    }
}

// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5
#[test]
fn ftol_fldcw_sandwich_frintts_and_fallback() {
    let inputs_32: &[f64] = &[
        0.0,
        -0.0,
        1.9,
        -1.9,
        42.0,
        -42.0,
        2_147_483_647.9,
        -2_147_483_648.0,
        -2_147_483_648.5, // truncates to -2^31 (in range + PE!)
        -2_147_483_649.0, // out of range -> 0x8000_0000 + IE
        3.0e10,           // out of range -> 0x8000_0000 + IE
        -3.0e10,
        f64::NAN,
    ];
    let inputs_64: &[f64] = &[
        0.0,
        -123456.789,
        4_611_686_018_427_387_904.0,  // 2^62
        -9_223_372_036_854_775_808.0, // -2^63
        9_223_372_036_854_775_808.0,  // +2^63 (out of range -> 0x8000_0000_0000_0000 + IE)
        1.0e20,
        -1.0e20,
        f64::NAN,
    ];

    // Variant 0: classic MSVC `_ftol` (`fnstcw / movzx / or ah, 0x0c / fldcw / fistp m32 / fldcw`)
    // inside a loop with arithmetic before and after (verifies `control_stable` across sandwich).
    for &val in inputs_32 {
        let mut program = fixture(|code| {
            let mut lp = code.create_label();
            code.mov(ecx, 3).unwrap();
            code.set_label(&mut lp).unwrap();
            code.fld(qword_ptr(ebx)).unwrap();
            code.fadd(qword_ptr(ebx + 8)).unwrap();
            code.fnstcw(word_ptr(esp - 4)).unwrap();
            code.movzx(eax, word_ptr(esp - 4)).unwrap();
            code.or(ah, 0x0c).unwrap();
            code.mov(word_ptr(esp - 2), ax).unwrap();
            code.fldcw(word_ptr(esp - 2)).unwrap();
            code.fistp(dword_ptr(ebx + 16)).unwrap();
            code.fldcw(word_ptr(esp - 4)).unwrap();
            // Post-sandwich arithmetic verifies control_stable restoration.
            code.fld(qword_ptr(ebx + 24)).unwrap();
            code.fadd(qword_ptr(ebx + 32)).unwrap();
            code.fstp(qword_ptr(ebx + 24)).unwrap();
            code.dec(ecx).unwrap();
            code.jnz(lp).unwrap();
        });
        program.state.x87.set_control(0x027f); // PC53, RN, masked
        program.data[0..8].copy_from_slice(&val.to_le_bytes());
        program.data[8..16].copy_from_slice(&0.0f64.to_le_bytes());
        program.data[24..32].copy_from_slice(&1.5f64.to_le_bytes());
        program.data[32..40].copy_from_slice(&0.25f64.to_le_bytes());
        for config in test_configs(false, true, false) {
            run_diff_tier1(&program, config, 1000);
        }
    }

    // Variant 1: `_ftol2` / 64-bit `fistp m64` + in-place byte `or byte ptr [esp-1], 0x0c`.
    for &val in inputs_64 {
        let mut program = fixture(|code| {
            code.fld(qword_ptr(ebx)).unwrap();
            code.fnstcw(word_ptr(esp - 4)).unwrap();
            code.mov(ax, word_ptr(esp - 4)).unwrap();
            code.mov(word_ptr(esp - 2), ax).unwrap();
            code.or(byte_ptr(esp - 1), 0x0c).unwrap();
            code.fldcw(word_ptr(esp - 2)).unwrap();
            code.fistp(qword_ptr(ebx + 16)).unwrap();
            code.fldcw(word_ptr(esp - 4)).unwrap();
        });
        program.state.x87.set_control(0x037f); // PC64, RN, masked
        program.data[0..8].copy_from_slice(&val.to_le_bytes());
        for config in test_configs(false, true, false) {
            run_diff_tier1(&program, config, 500);
        }
    }
}

// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5
#[test]
fn cross_unit_x87_sse_residency_fpsr_folding_and_faults() {
    // 1. Interleaved SSE + x87 loop with live XMM across x87 runs, exercising
    //    SSE sticky flags (including subnormal IDC/DE, overflow OFC, inexact IXC)
    //    and non-default MXCSR rounding modes (RD/RU/RZ + FTZ/DAZ) right before x87.
    for &mxcsr in &[
        0x1f80u32, // default RN, all masked
        0x3f80u32, // RD (-Inf)
        0x5f80u32, // RU (+Inf)
        0x7f80u32, // RZ (chop)
        0x9fc0u32, // FTZ | DAZ | RN
    ] {
        let mut program = fixture(|code| {
            let mut lp = code.create_label();
            code.mov(ecx, 6).unwrap();
            code.movups(xmm0, xmmword_ptr(ebx)).unwrap();
            code.movups(xmm1, xmmword_ptr(ebx + 16)).unwrap();
            code.set_label(&mut lp).unwrap();
            // SSE op that raises IDC/DE + IXC on subnormal lane, keeping XMM0/XMM1 resident.
            code.addps(xmm0, xmm1).unwrap();
            // Interleaved x87 run: must fold SSE FPSR and keep RN/no-FZ/FIZ FPCR
            // while keeping XMM0/XMM1 resident across the x87 run!
            code.fld(qword_ptr(ebx + 32)).unwrap();
            code.fadd(qword_ptr(ebx + 40)).unwrap();
            code.fmul(qword_ptr(ebx + 48)).unwrap();
            code.fstp(qword_ptr(ebx + 32)).unwrap();
            // Follow-up SSE op consuming resident XMM0/XMM1.
            code.mulss(xmm0, xmm1).unwrap();
            code.dec(ecx).unwrap();
            code.jnz(lp).unwrap();
            code.movups(xmmword_ptr(ebx + 64), xmm0).unwrap();
        });
        program.state.mxcsr = mxcsr;
        program.state.x87.set_control(0x027f); // PC53, RN
        // xmm0: [subnormal f32 (triggers IDC/DE), 1.5, -2.25, f32::MAX]
        let x0: [u32; 4] = [
            0x0000_0001,
            1.5f32.to_bits(),
            (-2.25f32).to_bits(),
            10.0f32.to_bits(),
        ];
        let x1: [u32; 4] = [
            1.0f32.to_bits(),
            0.3f32.to_bits(),
            0.7f32.to_bits(),
            2.0f32.to_bits(),
        ];
        for (i, w) in x0.iter().enumerate() {
            program.data[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
        }
        for (i, w) in x1.iter().enumerate() {
            program.data[16 + i * 4..16 + (i + 1) * 4].copy_from_slice(&w.to_le_bytes());
        }
        program.data[32..40].copy_from_slice(&1.1f64.to_le_bytes());
        program.data[40..48].copy_from_slice(&2.3f64.to_le_bytes());
        program.data[48..56].copy_from_slice(&0.9f64.to_le_bytes());

        for config in test_configs(false, false, true) {
            run_diff_tier1(&program, config, 2000);
            // Also test mid-loop budget exits + resumes (cold exits and unit transfers).
            let mut native = executor(&program, config, false);
            let mut oracle = executor(&program, config, true);
            for _ in 0..40 {
                native.run(program.stop, 7).unwrap();
                if native.state.eip == program.stop {
                    break;
                }
            }
            oracle.run(program.stop, 2000).unwrap();
            assert_eq!(native.state.eip, program.stop);
            same(&native, &oracle);
        }
    }

    // 2. Fault inside an x87 run while XMM0 is dirty and a lazy C1 recipe is live.
    let mut fault_prog = fixture(|code| {
        code.movups(xmm0, xmmword_ptr(ebx)).unwrap();
        code.addps(xmm0, xmm0).unwrap(); // XMM0 dirty
        code.fld(qword_ptr(ebx + 16)).unwrap();
        code.fadd(qword_ptr(ebx + 24)).unwrap(); // lazy C1Round live
        code.fld(dword_ptr(edx)).unwrap(); // edx = 0 -> page fault!
        code.fstp(qword_ptr(ebx + 32)).unwrap();
    });
    fault_prog.state.regs[2] = 0; // EDX = 0 (unmapped)
    fault_prog.state.x87.set_control(0x027f);
    fault_prog.state.x87.status |= 0x20;
    fault_prog.data[0..16].copy_from_slice(&[0x00, 0x00, 0x80, 0x3f].repeat(4));
    fault_prog.data[16..24].copy_from_slice(&1.0f64.to_le_bytes());
    fault_prog.data[24..32].copy_from_slice(&(2.0f64.powi(-53) + 2.0f64.powi(-60)).to_le_bytes());
    for config in test_configs(false, false, true) {
        run_diff(&fault_prog, config, 200);
    }
}

// spec: A64-FLAG-6, A64F-FP-1, A64F-FP-7, A64F-FP-5
#[test]
fn fld_m32_m64_denormal_sets_sw_de_accurately() {
    let f32_cases: &[u32] = &[
        0x0000_0000, // +0.0 (NO DE)
        0x8000_0000, // -0.0 (NO DE)
        0x0000_0001, // smallest +denormal (DE=1)
        0x007f_ffff, // largest +denormal (DE=1)
        0x8000_0001, // smallest -denormal (DE=1)
        0x0080_0000, // smallest normal (NO DE)
    ];
    let f64_cases: &[u64] = &[
        0x0000_0000_0000_0000, // +0.0 (NO DE)
        0x8000_0000_0000_0000, // -0.0 (NO DE)
        0x0000_0000_0000_0001, // smallest +denormal (DE=1)
        0x000f_ffff_ffff_ffff, // largest +denormal (DE=1)
        0x8000_0000_0000_0001, // smallest -denormal (DE=1)
        0x0010_0000_0000_0000, // smallest normal (NO DE)
    ];

    for &control in &[0x037fu16, 0x037du16] {
        // DM=1 (masked) and DM=0 (unmasked #MF)
        for &bits in f32_cases {
            // Inside a cached x87 run AND standalone.
            let mut program = fixture(|code| {
                code.fld(dword_ptr(ebx + 16)).unwrap(); // normal 1.0 opens run
                code.fld(dword_ptr(ebx)).unwrap(); // test value
                code.faddp(st1, st0).unwrap();
                code.fstp(qword_ptr(ebx + 24)).unwrap();
            });
            program.state.x87.set_control(control);
            program.data[0..4].copy_from_slice(&bits.to_le_bytes());
            program.data[16..20].copy_from_slice(&1.0f32.to_le_bytes());
            for config in test_configs(false, false, true) {
                run_diff_tier1(&program, config, 200);
            }
        }
        for &bits in f64_cases {
            let mut program = fixture(|code| {
                code.fld(qword_ptr(ebx)).unwrap();
                code.fstp(qword_ptr(ebx + 16)).unwrap();
            });
            program.state.x87.set_control(control);
            program.data[0..8].copy_from_slice(&bits.to_le_bytes());
            for config in test_configs(false, false, true) {
                run_diff_tier1(&program, config, 200);
            }
        }
    }
}
