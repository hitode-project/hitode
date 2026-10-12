//! The x86 flags as the translator tracks them (§A3).
//!
//! SF ZF OF always live in NZCV. CF lives in NZCV's C in a tracked carry
//! sense, or lazily as a `Carry` over values still in registers, which lets
//! an `inc` or `dec` clobber C for free. PF and AF stay a `Recipe` over such
//! values until something would overwrite them.

use iced_x86::ConditionCode;

/// A set of x86 arithmetic flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlagSet(u8);

impl FlagSet {
    pub const NONE: Self = Self(0);
    pub const CF: Self = Self(1);
    pub const PF: Self = Self(2);
    pub const AF: Self = Self(4);
    pub const ZF: Self = Self(8);
    pub const SF: Self = Self(16);
    pub const OF: Self = Self(32);
    pub const ALL: Self = Self(63);
    /// The flags NZCV holds directly.
    pub const NZV: Self = Self(8 | 16 | 32);

    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn minus(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Whether any flag of `other` is in the set.
    pub const fn any(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// The flags a condition code reads.
    pub const fn of(cc: ConditionCode) -> Self {
        use ConditionCode as C;
        match cc {
            C::o | C::no => Self::OF,
            C::b | C::ae => Self::CF,
            C::e | C::ne => Self::ZF,
            C::be | C::a => Self::CF.or(Self::ZF),
            C::s | C::ns => Self::SF,
            C::p | C::np => Self::PF,
            C::l | C::ge => Self::SF.or(Self::OF),
            C::le | C::g => Self::ZF.or(Self::SF).or(Self::OF),
            C::None => Self::NONE,
        }
    }
}

/// How NZCV's C encodes CF (§A3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sense {
    /// C is NOT CF, as after `subs`: the canonical sense at exits.
    Subtraction,
    /// C is CF, as after `adds` and `ands`.
    Addition,
}

/// A 32-bit value a recipe reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    /// A guest register by `CpuState::regs` index.
    Guest(u8),
    /// Recipe temporary 0 (x7) or 1 (x8).
    Temp(u8),
    Imm(u32),
}

/// Where PF and AF come from (§A3.3): PF from the low byte of a result and
/// AF from bit 4 of left ^ right ^ result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipe {
    /// x10 and x11 hold them, or they are dead.
    Canonical,
    /// `result = left + right`, with the result and the right operand known.
    AddResult(Operand, Operand),
    /// `result = left - right`, with the result and the right operand known.
    SubResult(Operand, Operand),
    /// `left + right`.
    Add(Operand, Operand),
    /// `left - right`.
    Sub(Operand, Operand),
    /// A logic result; AF is clear.
    Logic(Operand),
    /// `left & right`; AF is clear.
    And(Operand, Operand),
    /// `left | right`; AF is clear.
    Or(Operand, Operand),
    /// `left ^ right`; AF is clear.
    Xor(Operand, Operand),
    /// `result = 0 - right`, with the result known.
    Neg(Operand),
    /// `result = x + x`, whose AF is bit 4 of the result: x ^ x is 0.
    Doubled(Operand),
    /// PF from the result; x11 holds AF, which the instruction kept.
    Parity(Operand),
    /// `result` and `left ^ right`, as after a carrying add or subtract.
    Mixed(Operand, Operand),
}

impl Recipe {
    /// The PF source and the AF source, reading operands through `value`;
    /// `None` when they are already canonical, and no AF source when only
    /// PF is pending.
    pub fn evaluate(self, value: impl Fn(Operand) -> u32) -> Option<(u32, Option<u32>)> {
        use Recipe::*;
        let pair = |a: Operand, b: Operand| (value(a), value(b));
        Some(match self {
            Canonical => return None,
            Parity(r) => return Some((value(r), None)),
            AddResult(r, b) => {
                let (r, b) = pair(r, b);
                (r, r.wrapping_sub(b) ^ b ^ r)
            }
            SubResult(r, b) => {
                let (r, b) = pair(r, b);
                (r, r.wrapping_add(b) ^ b ^ r)
            }
            Add(a, b) => {
                let (a, b) = pair(a, b);
                let r = a.wrapping_add(b);
                (r, a ^ b ^ r)
            }
            Sub(a, b) => {
                let (a, b) = pair(a, b);
                let r = a.wrapping_sub(b);
                (r, a ^ b ^ r)
            }
            Logic(r) => (value(r), 0),
            And(a, b) => (value(a) & value(b), 0),
            Or(a, b) => (value(a) | value(b), 0),
            Xor(a, b) => (value(a) ^ value(b), 0),
            Neg(r) => {
                let r = value(r);
                (r, r.wrapping_neg() ^ r)
            }
            Doubled(r) => (value(r), value(r)),
            Mixed(r, t) => {
                let (r, t) = pair(r, t);
                (r, t ^ r)
            }
        })
        .map(|(parity, adjust)| (parity, Some(adjust)))
    }

    /// Whether AF is known to be clear.
    pub fn clears_af(self) -> bool {
        matches!(
            self,
            Recipe::Logic(_) | Recipe::And(..) | Recipe::Or(..) | Recipe::Xor(..)
        )
    }

    fn operands(self) -> [Option<Operand>; 2] {
        use Recipe::*;
        match self {
            Canonical => [None, None],
            Logic(a) | Neg(a) | Doubled(a) | Parity(a) => [Some(a), None],
            Mixed(a, b)
            | AddResult(a, b)
            | SubResult(a, b)
            | Add(a, b)
            | Sub(a, b)
            | And(a, b)
            | Or(a, b)
            | Xor(a, b) => [Some(a), Some(b)],
        }
    }

    pub fn uses(self, operand: Operand) -> bool {
        self.operands().contains(&Some(operand))
    }
}

/// Where CF is (§A3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carry {
    /// NZCV's C in the tracked sense.
    Host,
    /// Bit `n` of a value.
    Bit(Operand, u8),
    /// `a < b`, unsigned.
    Below(Operand, Operand),
    /// The carry out of `a + b`.
    Sum(Operand, Operand),
    /// `a != 0`.
    Nonzero(Operand),
    Clear,
}

impl Carry {
    /// CF, reading operands through `value`; `None` for `Host`.
    pub fn evaluate(self, value: impl Fn(Operand) -> u32) -> Option<bool> {
        use Carry::*;
        Some(match self {
            Host => return None,
            Bit(a, n) => value(a) >> n & 1 != 0,
            Below(a, b) => value(a) < value(b),
            Sum(a, b) => value(a).checked_add(value(b)).is_none(),
            Nonzero(a) => value(a) != 0,
            Clear => false,
        })
    }

    pub fn uses(self, operand: Operand) -> bool {
        use Carry::*;
        match self {
            Host | Clear => false,
            Bit(a, _) | Nonzero(a) => a == operand,
            Below(a, b) | Sum(a, b) => a == operand || b == operand,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_carries_match_x86_cf() {
        let regs = [0xffff_fff0u32, 0x20, 0, 5];
        let value = |operand| match operand {
            Operand::Guest(index) => regs[usize::from(index)],
            Operand::Temp(_) => unreachable!(),
            Operand::Imm(value) => value,
        };
        let (a, b, zero) = (Operand::Guest(0), Operand::Guest(1), Operand::Guest(2));
        // add a, b wraps: the result 0x10 is below b.
        let sum = 0xffff_fff0u32.wrapping_add(0x20);
        assert_eq!(sum, 0x10);
        assert_eq!(
            Carry::Below(Operand::Imm(sum), b).evaluate(value),
            Some(true)
        );
        assert_eq!(Carry::Sum(a, b).evaluate(value), Some(true));
        // sub b, a borrows: the result plus a carries out.
        let difference = 0x20u32.wrapping_sub(0xffff_fff0);
        assert_eq!(
            Carry::Sum(Operand::Imm(difference), a).evaluate(value),
            Some(true)
        );
        assert_eq!(Carry::Below(b, a).evaluate(value), Some(true));
        assert_eq!(Carry::Nonzero(zero).evaluate(value), Some(false));
        assert_eq!(Carry::Bit(a, 31).evaluate(value), Some(true));
        assert_eq!(Carry::Host.evaluate(value), None);
    }

    #[test]
    fn condition_codes_read_their_flags() {
        assert_eq!(FlagSet::of(ConditionCode::a), FlagSet::CF.or(FlagSet::ZF));
        assert!(FlagSet::of(ConditionCode::g).any(FlagSet::OF));
        assert!(!FlagSet::of(ConditionCode::ne).any(FlagSet::CF));
    }
}
