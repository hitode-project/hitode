//! Guard admission and segment recovery tests beyond binary64-only transfers.
use super::*;

fn input(a: X87Register, b: X87Register) -> CpuState {
    let mut s = state();
    s.x87.control = 0x037f;
    s.x87.status |= 0x473f;
    for (logical, value) in [a, b].into_iter().enumerate() {
        let slot = s.x87.physical_index(logical);
        s.x87.regs[slot] = value;
        s.x87.set_physical_tag(slot, value.tag());
    }
    s.refresh_mmx();
    s
}

fn ext(v: f64) -> X87Register {
    X87Register(crate::backend::x87::encode_f64(v))
}

#[test]
fn a_zero_fused_division_residual_without_range_guards_is_not_a_proof() {
    let a = f64::MIN_POSITIVE;
    let b = (1.0 - f64::EPSILON) * 2f64.powi(-511);
    let q = a / b;
    assert!(q.is_normal());
    // q*b differs from a by 2^-1126, which even fused binary64 rounds to zero.
    // The exact path must still report PE, despite an unchanged rounded value.
    assert_eq!(q.mul_add(b, -a), 0.0);
    let code = bytes(|asm| asm.fdiv_2(st0, st1).unwrap());
    let mut initial = input(ext(a), ext(b));
    initial.x87.status &= !0x3f;
    let mut expected = initial.clone();
    assert_eq!(
        Interpreter::new().step(&mut expected, &mut memory(&code)),
        Step::Executed
    );
    assert_ne!(expected.x87.status & 0x20, 0);
    for dekker in [false, true] {
        testing::FORCE_DEKKER.set(dekker);
        compare(&code, initial.clone(), |_| {}, Some(1));
    }
    testing::FORCE_DEKKER.set(false);
}

#[test]
fn exact_division_requires_native_hits_and_protects_the_quotient_range() {
    for single in [false, true] {
        testing::SINGLE_ONLY.set(single);
        for dekker in [false, true] {
            testing::FORCE_DEKKER.set(dekker);
            for form in 0..6 {
                let code = bytes(|a| {
                    match form {
                        0 => a.fdiv_2(st0, st1),
                        1 => a.fdivr_2(st0, st1),
                        2 => a.fdiv_2(st1, st0),
                        3 => a.fdivr_2(st1, st0),
                        4 => a.fdivp(st1, st0),
                        _ => a.fdivrp(st1, st0),
                    }
                    .unwrap();
                });
                for (a, b) in [
                    (1.0, 2.0),
                    (-1.0, 2.0),
                    (1.0, -2.0),
                    (-1.0, -2.0),
                    (2f64.powi(400), 1.0),
                    (2f64.powi(-400), 1.0),
                ] {
                    compare(&code, input(ext(a), ext(b)), |_| {}, Some(0));
                }
                for (a, b) in [
                    (2f64.powi(400), 2f64.powi(-400)),
                    (2f64.powi(-400), 2f64.powi(400)),
                    (2f64.powi(401), 1.0),
                ] {
                    compare(&code, input(ext(a), ext(b)), |_| {}, Some(1));
                }
                // Neither direction of 3/5 is exactly representable.
                compare(&code, input(ext(3.0), ext(5.0)), |_| {}, Some(1));
                for (a, b) in [(0.0, -2.0), (-0.0, 2.0), (-0.0, -2.0), (0.0, 0.0)] {
                    compare(&code, input(ext(a), ext(b)), |_| {}, None);
                }
            }
        }
    }
    testing::FORCE_DEKKER.set(false);
    testing::SINGLE_ONLY.set(false);
}

#[test]
fn raw80_and_division_recoveries_keep_every_budget_prefix_and_entry() {
    let code = bytes(|a| {
        a.fld1().unwrap();
        a.fstp(qword_ptr(esi + 24)).unwrap();
        a.fld(st0).unwrap();
        a.fdiv(qword_ptr(esi)).unwrap();
        a.fxch(st0, st1).unwrap();
        a.fchs().unwrap();
        a.fstp(dword_ptr(esi + 32)).unwrap();
        a.fstp(qword_ptr(esi + 40)).unwrap();
    });
    let instructions: Vec<_> = Decoder::with_ip(32, &code, u64::from(CODE), DecoderOptions::NONE)
        .into_iter()
        .collect();
    for value in [
        ext(2.5),
        X87Register::from_parts((1 << 63) | 1, 0x3fff),
        X87Register::from_parts(1, 0),
        X87Register::from_parts((1 << 63) | 1, 0x7fff),
    ] {
        for control in [
            0x007f, 0x017f, 0x027f, 0x037f, 0x077f, 0x0b7f, 0x0f7f, 0x035f,
        ] {
            for start in &instructions {
                for fuel in 1..=instructions.len() {
                    let mut actual_memory = memory(&code);
                    let mut reference = memory(&code);
                    let region = Region::new(CODE, code.len());
                    let compiled = compile(&region, &mut actual_memory).unwrap();
                    let mut actual = input(value, ext(3.0));
                    actual.x87.control = control;
                    actual.x87.status &= !0x3f;
                    actual.eip = start.ip32();
                    actual.fuel = fuel as u64;
                    let mut expected = actual.clone();
                    let before = testing::calls();
                    let result = testing::run(&compiled, &mut actual, &mut actual_memory);
                    let retired = fuel as u64 - actual.fuel;
                    let mut interpreter = Interpreter::new();
                    for _ in 0..retired {
                        match interpreter.step(&mut expected, &mut reference) {
                            Step::Executed => {}
                            other => panic!("unexpected prefix step {other:?}"),
                        }
                    }
                    expected.fuel = actual.fuel;
                    assert_eq!(
                        actual,
                        expected,
                        "fuel {fuel}, control {control:#x}, helper {}",
                        testing::calls() - before
                    );
                    assert_eq!(
                        actual_memory.read_data(DATA, 64).unwrap(),
                        reference.read_data(DATA, 64).unwrap()
                    );
                    assert!(
                        matches!(result, x if x == Exit::Dispatch as u32 || x == Exit::Budget as u32 || x == Exit::Fallback as u32)
                    );
                }
            }
        }
    }
}
