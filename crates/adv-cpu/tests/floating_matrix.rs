use adv_cpu::interpreter::{Interpreter, Step};
use adv_cpu::{CpuState, EXEC, Memory, READ, WRITE, X87Register as Ext};
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
