//! AArch64 instruction encoders.
//!
//! Each encoder is a `const fn` returning one instruction word, so the
//! translator pays only for shifts and ors. Field ranges are asserted:
//! a silently truncated field would be a wrong instruction.

/// A general-purpose register number. 31 is the zero register or SP,
/// depending on the instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reg(u8);

impl Reg {
    pub const fn new(number: u8) -> Self {
        assert!(number < 32);
        Self(number)
    }

    pub const fn number(self) -> u32 {
        self.0 as u32
    }

    /// The link register.
    pub const LR: Self = Self(30);
    /// The zero register in data-processing operands.
    pub const ZR: Self = Self(31);
}

/// Operand width: the `sf` bit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    W,
    X,
}

impl Size {
    const fn sf(self) -> u32 {
        match self {
            Self::W => 0,
            Self::X => 1 << 31,
        }
    }

    const fn bits(self) -> u32 {
        match self {
            Self::W => 32,
            Self::X => 64,
        }
    }
}

/// Shift applied to the second register operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    Lsl = 0,
    Lsr = 1,
    Asr = 2,
}

const fn wide(base: u32, size: Size, rd: Reg, imm: u16, shift: u32) -> u32 {
    assert!(shift.is_multiple_of(16) && shift < size.bits());
    base | size.sf() | (shift / 16) << 21 | (imm as u32) << 5 | rd.number()
}

/// `movz rd, #imm, lsl #shift`.
pub const fn movz(size: Size, rd: Reg, imm: u16, shift: u32) -> u32 {
    wide(0x5280_0000, size, rd, imm, shift)
}

/// `movk rd, #imm, lsl #shift`.
pub const fn movk(size: Size, rd: Reg, imm: u16, shift: u32) -> u32 {
    wide(0x7280_0000, size, rd, imm, shift)
}

const fn shifted(
    base: u32,
    size: Size,
    rd: Reg,
    rn: Reg,
    rm: Reg,
    shift: Shift,
    amount: u32,
) -> u32 {
    assert!(amount < size.bits());
    base | size.sf()
        | (shift as u32) << 22
        | rm.number() << 16
        | amount << 10
        | rn.number() << 5
        | rd.number()
}

/// `add rd, rn, rm, <shift> #amount`.
pub const fn add(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x0B00_0000, size, rd, rn, rm, shift, amount)
}

/// `sub rd, rn, rm, <shift> #amount`.
pub const fn sub(size: Size, rd: Reg, rn: Reg, rm: Reg, shift: Shift, amount: u32) -> u32 {
    shifted(0x4B00_0000, size, rd, rn, rm, shift, amount)
}

/// `ret rn`.
pub const fn ret(rn: Reg) -> u32 {
    0xD65F_0000 | rn.number() << 5
}

/// `brk #imm`.
pub const fn brk(imm: u16) -> u32 {
    0xD420_0000 | (imm as u32) << 5
}

/// `nop`.
pub const NOP: u32 = 0xD503_201F;

#[cfg(test)]
mod tests {
    use super::Shift::*;
    use super::Size::*;
    use super::*;

    const fn r(number: u8) -> Reg {
        Reg::new(number)
    }

    /// Every case pairs an encoder call with the assembly clang encoded.
    fn cases() -> Vec<(u32, &'static str)> {
        vec![
            (movz(W, r(19), 0xffff, 16), "movz w19, #0xffff, lsl #16"),
            (movz(X, r(0), 0x1234, 0), "movz x0, #0x1234"),
            (movz(X, r(27), 1, 48), "movz x27, #0x1, lsl #48"),
            (movk(W, r(1), 7, 0), "movk w1, #0x7"),
            (movk(X, r(0), 0xbeef, 32), "movk x0, #0xbeef, lsl #32"),
            (add(W, r(19), r(19), r(20), Lsl, 0), "add w19, w19, w20"),
            (add(X, r(23), r(27), r(23), Lsl, 0), "add x23, x27, x23"),
            (add(X, r(0), r(1), r(2), Lsl, 3), "add x0, x1, x2, lsl #3"),
            (sub(W, r(0), r(1), r(2), Asr, 31), "sub w0, w1, w2, asr #31"),
            (sub(X, r(5), r(6), r(7), Lsr, 12), "sub x5, x6, x7, lsr #12"),
            (ret(Reg::LR), "ret"),
            (ret(r(1)), "ret x1"),
            (brk(0), "brk #0x0"),
            (brk(0xf00d), "brk #0xf00d"),
            (NOP, "nop"),
        ]
    }

    #[test]
    fn encoders_match_clang_vectors_and_cover_every_vector() {
        let text = include_str!("../testdata/encode.tsv");
        let mut vectors = std::collections::HashMap::new();
        for line in text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            let (word, asm) = line.split_once('\t').expect("word<TAB>assembly");
            let word = u32::from_str_radix(word, 16).expect("hex word");
            assert!(vectors.insert(asm, word).is_none(), "duplicate {asm}");
        }
        for (word, asm) in cases() {
            let expected = vectors
                .remove(asm)
                .unwrap_or_else(|| panic!("no vector for {asm}"));
            assert_eq!(word, expected, "{asm}: {word:08x} != {expected:08x}");
        }
        assert!(
            vectors.is_empty(),
            "vectors without a case: {:?}",
            vectors.keys()
        );
    }

    #[test]
    #[should_panic]
    fn out_of_range_shift_is_rejected() {
        let _ = add(W, r(0), r(0), r(0), Lsl, 32);
    }
}
