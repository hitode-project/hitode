use adv_jit::interpreter::{Interpreter, Step};
use adv_jit::{CpuState, EXEC, Memory, READ, WRITE, X87Register as Ext};
const CODE: u32 = 0x10000;
fn memory() -> Memory {
    let mut m = Memory::new(CODE, 0x20000).unwrap();
    m.protect(CODE, 0x20000, READ | WRITE).unwrap();
    m.protect(CODE, 4096, READ | EXEC).unwrap();
    m
}
fn step(bytes: &[u8], state: &mut CpuState) -> Step {
    let mut m = memory();
    m.write(CODE, bytes).unwrap();
    state.eip = CODE;
    Interpreter::new().step(state, &mut m)
}
fn integer(value: u64) -> Ext {
    if value == 0 {
        return Ext::from_parts(0, 0);
    }
    let exponent = 63 - value.leading_zeros();
    Ext::from_parts(value << (63 - exponent), 0x3fff + exponent as u16)
}
fn binary64(value: f64) -> Ext {
    let bits = value.to_bits();
    let sign = ((bits >> 48) & 0x8000) as u16;
    let exponent = ((bits >> 52) & 0x7ff) as u16;
    let fraction = bits & 0x000f_ffff_ffff_ffff;
    if exponent == 0x7ff {
        return Ext::from_parts((1 << 63) | (fraction << 11), sign | 0x7fff);
    }
    if exponent == 0 {
        if fraction == 0 {
            return Ext::from_parts(0, sign);
        }
        let shift = fraction.leading_zeros();
        return Ext::from_parts(fraction << shift, sign | (15361 - (shift - 11) as u16));
    }
    Ext::from_parts(((1 << 52) | fraction) << 11, sign | (exponent + 15360))
}
fn top(s: &CpuState, logical: usize) -> Ext {
    s.x87.regs[s.x87.physical_index(logical)]
}

#[test]
fn fxch_exchanges_each_explicit_iced_stack_operand_without_touching_other_slots() {
    for logical in 0..8 {
        let mut s = CpuState::default();
        for value in (1..=8).rev() {
            s.x87.push(integer(value));
        }
        s.refresh_mmx();
        let before = s.x87.clone();
        assert_eq!(step(&[0xd9, 0xc8 + logical as u8], &mut s), Step::Executed);
        assert_eq!(s.x87.top(), before.top());
        assert_eq!(s.x87.tag, before.tag);
        for slot in 0..8 {
            let source = if slot == 0 {
                logical
            } else if slot == logical {
                0
            } else {
                slot
            };
            assert_eq!(
                top(&s, slot),
                before.regs[before.physical_index(source)],
                "FXCH ST({logical}) slot {slot}"
            );
        }
    }
}

#[test]
fn noncommuting_stack_arithmetic_covers_both_destinations_reverse_and_pop_forms() {
    // Every result below is exact, so no floating library or emulator is the oracle.
    for logical in 1..8 {
        for (opcode, base, destination, reverse, pop, divide) in [
            (0xd8, 0xe0, 0, false, false, false),
            (0xd8, 0xe8, 0, true, false, false),
            (0xd8, 0xf0, 0, false, false, true),
            (0xd8, 0xf8, 0, true, false, true),
            (0xdc, 0xe8, logical, false, false, false),
            (0xdc, 0xe0, logical, true, false, false),
            (0xdc, 0xf8, logical, false, false, true),
            (0xdc, 0xf0, logical, true, false, true),
            (0xde, 0xe8, logical, false, true, false),
            (0xde, 0xe0, logical, true, true, false),
            (0xde, 0xf8, logical, false, true, true),
            (0xde, 0xf0, logical, true, true, true),
        ] {
            let mut s = CpuState::default();
            for _ in 0..8 {
                s.x87.push(integer(2));
            }
            s.x87.regs[s.x87.physical_index(0)] = integer(8);
            s.refresh_mmx();
            let (a, b) = if destination == 0 {
                (8f64, 2f64)
            } else {
                (2f64, 8f64)
            };
            let (a, b) = if reverse { (b, a) } else { (a, b) };
            let expected = binary64(if divide { a / b } else { a - b });
            assert_eq!(
                step(&[opcode, base + logical as u8], &mut s),
                Step::Executed
            );
            let result_slot = if pop { destination - 1 } else { destination };
            assert_eq!(
                top(&s, result_slot),
                expected,
                "opcode {opcode:02x} {base:02x} ST({logical})"
            );
            assert_eq!(s.x87.top(), usize::from(pop));
        }
    }
}

#[test]
fn vorbis_mdct_initialization_computes_logn_for_both_block_sizes() {
    // FILD n; FLDLN2; FXCH; FYL2X; FLDLN2; FLD 2; FYL2X;
    // FDIVP ST1,ST0; FADD .5 -- the exact startup sequence that exposed FXCH.
    for n in [256u32, 2048] {
        let mut s = CpuState::default();
        s.x87.push(integer(n as u64));
        assert_eq!(step(&[0xd9, 0xed], &mut s), Step::Executed);
        assert_eq!(step(&[0xd9, 0xc9], &mut s), Step::Executed);
        assert_eq!(step(&[0xd9, 0xf1], &mut s), Step::Executed);
        assert_eq!(step(&[0xd9, 0xed], &mut s), Step::Executed);
        s.x87.push(integer(2));
        assert_eq!(step(&[0xd9, 0xf1], &mut s), Step::Executed);
        assert_eq!(step(&[0xde, 0xf9], &mut s), Step::Executed);
        let expected = integer(n.ilog2() as u64);
        let actual = top(&s, 0);
        assert_eq!(actual.sign_exponent(), expected.sign_exponent());
        assert!(actual.significand().abs_diff(expected.significand()) <= 1);
    }
}

#[cfg(target_arch = "x86_64")]
fn host(op: u8, x: Ext, y: Ext, control: u16) -> (Ext, u16) {
    #[repr(align(16))]
    struct Saved([u8; 512]);
    let mut saved = Saved([0; 512]);
    let mut out = Ext::default();
    let status: u16;
    macro_rules! instruction {
        ($ins:literal) => {{
            // SAFETY: bounded x87 fixture with full host FP/SIMD save/restore;
            // all pointers remain live and FXSAVE storage is 16-byte aligned.
            unsafe { std::arch::asm!("fxsave [{save}]","fninit","fldcw [{cw}]",
                "fld tbyte ptr [{y}]","fld tbyte ptr [{x}]",$ins,"fnstsw ax",
                "fstp tbyte ptr [{out}]","fxrstor [{save}]",
                save=in(reg)saved.0.as_mut_ptr(),cw=in(reg)&control,
                x=in(reg)x.0.as_ptr(),y=in(reg)y.0.as_ptr(),out=in(reg)out.0.as_mut_ptr(),
                out("ax")status,options(nostack)); }
        }};
    }
    match op {
        0 => instruction!("fyl2x"),
        1 => instruction!("fyl2xp1"),
        2 => instruction!("f2xm1"),
        3 => instruction!("fscale"),
        4 => instruction!("fsin"),
        5 => instruction!("fcos"),
        6 => instruction!("fpatan"),
        7 => instruction!("fptan; fstp st(0)"),
        8 => instruction!("fsincos"),
        9 => instruction!("2: fprem; fnstsw ax; test ax,0x400; jnz 2b"),
        10 => instruction!("2: fprem1; fnstsw ax; test ax,0x400; jnz 2b"),
        _ => unreachable!(),
    }
    (out, status)
}

#[cfg(target_arch = "x86_64")]
fn ulp_distance(a: Ext, b: Ext) -> u128 {
    assert_eq!(
        a.sign_exponent() >> 15,
        b.sign_exponent() >> 15,
        "opposite signs {a:?} {b:?}"
    );
    let index = |v: Ext| {
        ((v.sign_exponent() as u128 & 0x7fff) << 63)
            | (v.significand() as u128 & ((1u128 << 63) - 1))
    };
    index(a).abs_diff(index(b))
}

#[cfg(target_arch = "x86_64")]
#[test]
fn software_transcendentals_match_host_x87_small_range_and_guest_rounding() {
    for (op, opcode, x, y) in [
        (0, 0xf1, 1.125, 3.0),
        (0, 0xf1, 256.0, 1.0),
        (0, 0xf1, 0.75, -2.0),
        (1, 0xf9, 0.000_000_01, 2.0),
        (1, 0xf9, -0.125, 1.5),
        (2, 0xf0, 0.5, 0.0),
        (2, 0xf0, -0.5, 0.0),
        (2, 0xf0, 1.0, 0.0),
        (2, 0xf0, -1.0, 0.0),
        (3, 0xfd, 1.25, 5.75),
        (3, 0xfd, -2.5, -7.25),
        (4, 0xfe, 0.25, 0.0),
        (4, 0xfe, -0.75, 0.0),
        (5, 0xff, 0.25, 0.0),
        (5, 0xff, -0.75, 0.0),
        (6, 0xf3, 0.5, 0.75),
        (6, 0xf3, -0.5, -0.75),
        (7, 0xf2, 0.25, 0.0),
        (7, 0xf2, -0.75, 0.0),
        (8, 0xfb, 0.25, 0.0),
        (8, 0xfb, -0.75, 0.0),
    ] {
        for mode in 0..4 {
            // PC24 deliberately verifies these instructions retain the full
            // 64-bit significand regardless of arithmetic precision control.
            let control = 0x7f | (mode << 10);
            let (x, y) = (binary64(x), binary64(y));
            let (expected, status) = host(op, x, y, control);
            let mut s = CpuState::default();
            s.x87.control = control;
            s.x87.push(y);
            s.x87.push(x);
            assert_eq!(step(&[0xd9, opcode], &mut s), Step::Executed);
            let actual = top(&s, usize::from(op == 7));
            assert!(
                ulp_distance(actual, expected) <= 2,
                "op {op} RC {mode}: {actual:?} vs {expected:?}"
            );
            assert_eq!(s.x87.status & 0x3f, status & 0x3f, "op {op} RC {mode}");
        }
    }
}

#[test]
fn transcendental_special_values_preserve_signed_zero_nan_and_stack_contracts() {
    let mut s = CpuState::default();
    s.x87.push(integer(2));
    s.x87.push(Ext::from_parts(0, 0x8000));
    assert_eq!(step(&[0xd9, 0xf1], &mut s), Step::Executed);
    assert_eq!(top(&s, 0), Ext::from_parts(1 << 63, 0xffff));
    assert_eq!(s.x87.status & 5, 4);
    let mut s = CpuState::default();
    s.x87.push(integer(1));
    s.x87.push(binary64(-2.0));
    assert_eq!(step(&[0xd9, 0xf1], &mut s), Step::Executed);
    assert_eq!(top(&s, 0), Ext::from_parts(0xc000_0000_0000_0000, 0xffff));
    assert_eq!(s.x87.status & 1, 1);
    for op in [0xfe, 0xff, 0xfb, 0xf2] {
        let mut s = CpuState::default();
        s.x87.push(Ext::from_parts(1 << 63, 0x403e));
        let before = s.x87.clone();
        assert_eq!(step(&[0xd9, op], &mut s), Step::Executed);
        assert_eq!(s.x87.regs, before.regs);
        assert_eq!(s.x87.top(), before.top());
        assert_eq!(s.x87.status & 0x400, 0x400);
    }
    let tiny = Ext::from_parts(1, 0);
    let mut s = CpuState::default();
    s.x87.push(integer(1));
    s.x87.push(tiny);
    assert_eq!(step(&[0xd9, 0xfd], &mut s), Step::Executed);
    assert_eq!(top(&s, 0), Ext::from_parts(2, 0));
    assert_eq!(s.x87.status & 0x12, 2); // denormal source, exact scaled result
}

#[cfg(target_arch = "x86_64")]
#[test]
fn exact_remainders_match_native_truncation_nearest_quotient_bits_and_partial_loops() {
    for nearest in [false, true] {
        for (x, y) in [
            (7.0, 2.0),
            (-7.0, 2.0),
            (7.0, -2.0),
            (-7.0, -2.0),
            (5.0, 2.0),
            (3.0, 2.0),
            (0.75, 1.0),
            (-0.75, 1.0),
            (0.5, 1.0),
            (123456789012345.0, 1.125),
        ] {
            for mode in 0..4 {
                let control = 0x7f | (mode << 10);
                let (x, y) = (binary64(x), binary64(y));
                let (expected, status) = host(if nearest { 10 } else { 9 }, x, y, control);
                let mut s = CpuState::default();
                s.x87.control = control;
                s.x87.push(y);
                s.x87.push(x);
                assert_eq!(
                    step(&[0xd9, if nearest { 0xf5 } else { 0xf8 }], &mut s),
                    Step::Executed
                );
                assert_eq!(top(&s, 0), expected, "nearest {nearest} RC {mode}");
                assert_eq!(
                    s.x87.status & 0x473f,
                    status & 0x473f,
                    "quotient bits nearest {nearest} x {x:?} y {y:?}"
                );
            }
        }
        for exponent in [0x4040, 0x4100, 0x7ffe] {
            let x = Ext::from_parts(0xcafe_1234_5678_9abc, exponent);
            let y = Ext::from_parts(0x9234_5678_9abc_def0, 0x3fff);
            let (expected, status) = host(if nearest { 10 } else { 9 }, x, y, 0x37f);
            let mut s = CpuState::default();
            s.x87.push(y);
            s.x87.push(x);
            let mut steps = 0;
            loop {
                assert_eq!(
                    step(&[0xd9, if nearest { 0xf5 } else { 0xf8 }], &mut s),
                    Step::Executed
                );
                steps += 1;
                if steps == 1 {
                    assert_ne!(s.x87.status & 0x400, 0);
                    assert_eq!(s.x87.top(), 6);
                }
                if s.x87.status & 0x400 == 0 {
                    break;
                }
                assert!(steps < 1100);
            }
            assert_eq!(
                top(&s, 0),
                expected,
                "partial nearest {nearest} exponent {exponent:x}"
            );
            assert_eq!(
                s.x87.status & 0x473f,
                status & 0x473f,
                "final quotient bits exponent {exponent:x}"
            );
        }
    }
}
