//! PC=64 exact-result admission, including the native and portable residuals.
use super::*;

fn inputs(a: X87Register, b: X87Register) -> CpuState {
    let mut s = state();
    s.x87.control = 0x037f;
    s.x87.status |= 0x4721; // Sticky PE/IE and all condition bits, including C1.
    for (logical, value) in [a, b].into_iter().enumerate() {
        let slot = s.x87.physical_index(logical);
        s.x87.regs[slot] = value;
        s.x87.set_physical_tag(slot, value.tag());
    }
    s.refresh_mmx();
    s
}

fn ext(value: f64) -> X87Register {
    X87Register(crate::backend::x87::encode_f64(value))
}

#[test]
fn pc64_arithmetic_guards_match_all_state_and_require_native_hits() {
    for single in [false, true] {
        testing::SINGLE_ONLY.set(single);
        for dekker in [false, true] {
            testing::FORCE_DEKKER.set(dekker);
            for form in 0..8 {
                let code = bytes(|a| {
                    match form {
                        0 => a.fadd_2(st0, st1),
                        1 => a.fsub_2(st0, st1),
                        2 => a.fsubr_2(st0, st1),
                        3 => a.fmul_2(st0, st1),
                        4 => a.faddp(st1, st0),
                        5 => a.fsubp(st1, st0),
                        6 => a.fsubrp(st1, st0),
                        _ => a.fmulp(st1, st0),
                    }
                    .unwrap();
                });
                for (a, b) in [
                    (1.25, 2.5),
                    (1.0, 2f64.powi(-52)),
                    (2f64.powi(400), 2f64.powi(400)),
                    (2f64.powi(-400), 2f64.powi(-400)),
                    (-0.0, -0.0),
                    (0.0, -0.0),
                    (1.0, -1.0),
                ] {
                    // All operations in this set are exact, including zero signs.
                    compare(&code, inputs(ext(a), ext(b)), |_| {}, Some(0));
                }
                let (a, b) = if form == 3 || form == 7 {
                    (1.0 + f64::EPSILON, 1.0 + f64::EPSILON)
                } else {
                    (1.0, 2f64.powi(-63))
                };
                compare(&code, inputs(ext(a), ext(b)), |_| {}, Some(1));
                for special in [
                    ext(2f64.powi(401)),
                    ext(f64::MIN_POSITIVE),
                    X87Register::from_parts(1 << 63, 0x3fff - 1074),
                    X87Register::from_parts((1 << 63) | 1, 0x3fff),
                    X87Register::from_parts(1 << 63, 0x43ff),
                    X87Register::from_parts(1 << 63, 0x7fff),
                    X87Register::from_parts(3 << 62, 0x7fff),
                    X87Register::from_parts(1, 0),
                ] {
                    compare(&code, inputs(special, ext(1.0)), |_| {}, Some(1));
                }
                for control in [0x077f, 0x0b7f, 0x0f7f, 0x017f, 0x037e] {
                    let mut s = inputs(ext(1.25), ext(2.5));
                    s.x87.status &= !1; // Do not introduce a pending unmasked IE.
                    s.x87.control = control;
                    compare(&code, s, |_| {}, Some(1));
                }
            }
        }
    }
    testing::FORCE_DEKKER.set(false);
    testing::SINGLE_ONLY.set(false);
}

#[test]
fn pc64_stores_already_admit_exact_values_and_preserve_rounding_flags() {
    for width in [4, 8] {
        for pop in [false, true] {
            let code = bytes(|a| {
                match (width, pop) {
                    (4, false) => a.fst(dword_ptr(esi)),
                    (4, true) => a.fstp(dword_ptr(esi)),
                    (8, false) => a.fst(qword_ptr(esi)),
                    _ => a.fstp(qword_ptr(esi)),
                }
                .unwrap();
            });
            for value in [
                -0.0,
                0.0,
                1.25,
                1.0 + 2f64.powi(-52),
                f64::MAX,
                f64::MIN_POSITIVE,
            ] {
                let calls =
                    u64::from(width == 4 && (value == f64::MAX || value == f64::MIN_POSITIVE));
                compare(&code, inputs(ext(value), ext(1.0)), |_| {}, Some(calls));
            }
            compare(
                &code,
                inputs(X87Register::from_parts((1 << 63) | 1, 0x3fff), ext(1.0)),
                |_| {},
                Some(1),
            );
        }
    }
}

#[test]
fn pc64_residual_guard_recovers_after_committed_stores_and_control_changes() {
    for wide in [false, true] {
        let code = bytes(|a| {
            a.fld1().unwrap();
            a.fadd_2(st0, st1).unwrap();
            a.fst(qword_ptr(esi + 16)).unwrap();
            a.fldcw(word_ptr(esi + 64)).unwrap();
            a.fmul(qword_ptr(esi + 8)).unwrap();
            a.fsubr_2(st0, st1).unwrap();
            a.fstp(dword_ptr(esi + 24)).unwrap();
        });
        for control in [0x007f, 0x027f, 0x037f, 0x0f7f] {
            compare(
                &code,
                inputs(ext(2f64.powi(-52)), ext(1.0)),
                |m| {
                    m.write(DATA + 64, &u16::to_le_bytes(control)).unwrap();
                    m.write(
                        DATA + 8,
                        &(if wide { 1.0 + f64::EPSILON } else { 2.0f64 }).to_le_bytes(),
                    )
                    .unwrap();
                },
                None,
            );
        }
    }
}
