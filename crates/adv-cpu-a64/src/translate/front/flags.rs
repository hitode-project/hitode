//! Arithmetic flags observed and written by decoded instructions.

use super::tags::ConditionCode;

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

#[cfg(test)]
mod tests {
    use super::{ConditionCode, FlagSet};

    // spec: A64-FLAG-1
    #[test]
    fn condition_codes_read_their_flags() {
        assert_eq!(FlagSet::of(ConditionCode::a), FlagSet::CF.or(FlagSet::ZF));
        assert!(FlagSet::of(ConditionCode::g).any(FlagSet::OF));
        assert!(!FlagSet::of(ConditionCode::ne).any(FlagSet::CF));
    }
}
