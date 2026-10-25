//! Typed, immutable preparation-time expressions. Loads name the memory state
//! they observe; an exit cannot silently reread a value after a store.
use std::sync::Arc;

#[cfg(test)]
use adv_cpu::integer;
use iced_x86::{ConditionCode, Mnemonic};

use super::Register;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Width {
    Bit = 1,
    Byte = 8,
    Word = 16,
    Dword = 32,
    Qword = 64,
}

impl Width {
    pub const fn bits(self) -> u8 {
        self as u8
    }

    pub const fn mask(self) -> u64 {
        u64::MAX >> (64 - self.bits())
    }
}

/// Zero denotes memory at the current cutpoint, including earlier iterations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MemoryVersion(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Binary {
    Add,
    Subtract,
    Multiply,
    And,
    Or,
    Xor,
    ShiftLeft,
    ShiftRight,
    ShiftArithmetic,
    Equal,
    LessUnsigned,
    LessSigned,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expression {
    width: Width,
    kind: Arc<Kind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Constant(u64),
    Register(Register),
    Flags,
    Binary(Binary, Expression, Expression),
    Extract(Expression, u8),
    Insert(Expression, Expression, u8),
    Extend(Expression, bool),
    Load(MemoryVersion, Expression),
    Select(Expression, Expression, Expression),
    IntegerFlags(Mnemonic, Expression, Expression, Expression),
    Condition(ConditionCode, Expression),
}

impl Expression {
    fn new(width: Width, kind: Kind) -> Self {
        Self {
            width,
            kind: Arc::new(kind),
        }
    }

    pub const fn width(&self) -> Width {
        self.width
    }

    pub fn constant(width: Width, value: u64) -> Self {
        Self::new(width, Kind::Constant(value & width.mask()))
    }

    pub fn register(register: Register) -> Self {
        Self::new(Width::Dword, Kind::Register(register))
    }

    pub fn flags() -> Self {
        Self::new(Width::Dword, Kind::Flags)
    }

    pub fn as_constant(&self) -> Option<u64> {
        match *self.kind {
            Kind::Constant(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn register_offset(&self) -> Option<(Register, u32)> {
        match self.kind.as_ref() {
            Kind::Register(r) => Some((*r, 0)),
            Kind::Binary(Binary::Add, base, offset) => {
                let Kind::Register(r) = base.kind.as_ref() else {
                    return None;
                };
                Some((*r, offset.as_constant()? as u32))
            }
            _ => None,
        }
    }

    pub fn binary(op: Binary, left: Self, right: Self) -> Option<Self> {
        if left.width != right.width {
            return None;
        }
        let width = left.width;
        let comparison = matches!(
            op,
            Binary::Equal | Binary::LessUnsigned | Binary::LessSigned
        );
        let output = if comparison { Width::Bit } else { width };
        if let (Some(a), Some(b)) = (left.as_constant(), right.as_constant()) {
            return Some(Self::constant(output, calculate(op, a, b, width)));
        }
        if matches!(op, Binary::Add | Binary::Subtract) && right.as_constant() == Some(0) {
            return Some(left);
        }
        if op == Binary::Add
            && let (Kind::Binary(Binary::Add, base, old), Some(amount)) =
                (left.kind.as_ref(), right.as_constant())
            && let Some(old) = old.as_constant()
        {
            return Self::binary(
                Binary::Add,
                base.clone(),
                Self::constant(width, old.wrapping_add(amount)),
            );
        }
        Some(Self::new(output, Kind::Binary(op, left, right)))
    }

    pub fn extract(value: Self, shift: u8, width: Width) -> Option<Self> {
        if u16::from(shift) + u16::from(width.bits()) > u16::from(value.width.bits()) {
            return None;
        }
        if shift == 0 && width == value.width {
            return Some(value);
        }
        if let Some(constant) = value.as_constant() {
            return Some(Self::constant(width, constant >> shift));
        }
        match value.kind.as_ref() {
            Kind::Insert(_, inserted, offset) if *offset == shift && inserted.width == width => {
                Some(inserted.clone())
            }
            Kind::Extend(inner, _) if shift == 0 && inner.width == width => Some(inner.clone()),
            _ => Some(Self::new(width, Kind::Extract(value, shift))),
        }
    }

    pub fn insert(base: Self, value: Self, shift: u8) -> Option<Self> {
        if u16::from(shift) + u16::from(value.width.bits()) > u16::from(base.width.bits()) {
            return None;
        }
        if shift == 0 && value.width == base.width {
            return Some(value);
        }
        let width = base.width;
        let base = match base.kind.as_ref() {
            Kind::Insert(original, old, offset) if *offset == shift && old.width == value.width => {
                original.clone()
            }
            _ => base,
        };
        Some(Self::new(width, Kind::Insert(base, value, shift)))
    }

    pub fn extend(value: Self, width: Width, signed: bool) -> Option<Self> {
        if width.bits() < value.width.bits() {
            return None;
        }
        if width == value.width {
            return Some(value);
        }
        if let Some(constant) = value.as_constant() {
            let constant = if signed {
                sign(constant, value.width) as u64
            } else {
                constant
            };
            return Some(Self::constant(width, constant));
        }
        Some(Self::new(width, Kind::Extend(value, signed)))
    }

    pub fn load(memory: MemoryVersion, address: Self, width: Width) -> Option<Self> {
        if address.width != Width::Dword || width == Width::Bit {
            return None;
        }
        Some(Self::new(width, Kind::Load(memory, address)))
    }

    pub fn select(condition: Self, yes: Self, no: Self) -> Option<Self> {
        if condition.width != Width::Bit || yes.width != no.width {
            return None;
        }
        if let Some(value) = condition.as_constant() {
            return Some(if value == 0 { no } else { yes });
        }
        if yes == no {
            return Some(yes);
        }
        Some(Self::new(yes.width, Kind::Select(condition, yes, no)))
    }

    /// Flags are defined by the shared CPU integer semantics, not host flags.
    pub fn integer_flags(kind: Mnemonic, old: Self, left: Self, right: Self) -> Option<Self> {
        if old.width != Width::Dword
            || left.width != right.width
            || !matches!(left.width, Width::Byte | Width::Word | Width::Dword)
            || !matches!(
                kind,
                Mnemonic::Add
                    | Mnemonic::Adc
                    | Mnemonic::Sub
                    | Mnemonic::Sbb
                    | Mnemonic::Cmp
                    | Mnemonic::And
                    | Mnemonic::Test
                    | Mnemonic::Or
                    | Mnemonic::Xor
                    | Mnemonic::Inc
                    | Mnemonic::Dec
                    | Mnemonic::Neg
                    | Mnemonic::Not
                    | Mnemonic::Shl
                    | Mnemonic::Shr
                    | Mnemonic::Sar
            )
        {
            return None;
        }
        Some(Self::new(
            Width::Dword,
            Kind::IntegerFlags(kind, old, left, right),
        ))
    }

    pub fn condition(condition: ConditionCode, flags: Self) -> Option<Self> {
        (flags.width == Width::Dword)
            .then(|| Self::new(Width::Bit, Kind::Condition(condition, flags)))
    }

    pub(crate) fn kind(&self) -> &Kind {
        &self.kind
    }
}

fn sign(value: u64, width: Width) -> i64 {
    ((value << (64 - width.bits())) as i64) >> (64 - width.bits())
}

fn calculate(op: Binary, a: u64, b: u64, width: Width) -> u64 {
    match op {
        Binary::Add => a.wrapping_add(b),
        Binary::Subtract => a.wrapping_sub(b),
        Binary::Multiply => a.wrapping_mul(b),
        Binary::And => a & b,
        Binary::Or => a | b,
        Binary::Xor => a ^ b,
        Binary::ShiftLeft => {
            if b >= u64::from(width.bits()) {
                0
            } else {
                a << b
            }
        }
        Binary::ShiftRight => {
            if b >= u64::from(width.bits()) {
                0
            } else {
                a >> b
            }
        }
        Binary::ShiftArithmetic => (sign(a, width) >> b.min(u64::from(width.bits() - 1))) as u64,
        Binary::Equal => u64::from(a == b),
        Binary::LessUnsigned => u64::from(a < b),
        Binary::LessSigned => u64::from(sign(a, width) < sign(b, width)),
    }
}

/// Concrete oracle for development checks; execution bridges compile expressions.
#[cfg(test)]
impl Expression {
    pub(super) fn evaluate(
        &self,
        registers: &[u32; 8],
        flags: u32,
        memory: &impl Fn(MemoryVersion, u32, Width) -> u64,
    ) -> u64 {
        let eval = |value: &Self| value.evaluate(registers, flags, memory);
        let value = match self.kind.as_ref() {
            Kind::Constant(value) => *value,
            Kind::Register(r) => u64::from(registers[r.index() as usize]),
            Kind::Flags => u64::from(flags),
            Kind::Binary(op, a, b) => calculate(*op, eval(a), eval(b), a.width),
            Kind::Extract(value, shift) => eval(value) >> shift,
            Kind::Insert(base, value, shift) => {
                (eval(base) & !(value.width.mask() << shift))
                    | ((eval(value) & value.width.mask()) << shift)
            }
            Kind::Extend(value, signed) => {
                if *signed {
                    sign(eval(value), value.width) as u64
                } else {
                    eval(value)
                }
            }
            Kind::Load(version, address) => memory(*version, eval(address) as u32, self.width),
            Kind::Select(condition, yes, no) => eval(if eval(condition) != 0 { yes } else { no }),
            Kind::Condition(cc, value) => {
                u64::from(integer::condition_true(*cc, eval(value) as u32))
            }
            Kind::IntegerFlags(kind, old, left, right) => {
                let mut flags = eval(old) as u32;
                let (a, b, width) = (eval(left) as u32, eval(right) as u32, left.width.bits() / 8);
                match kind {
                    Mnemonic::Inc | Mnemonic::Dec | Mnemonic::Neg | Mnemonic::Not => {
                        integer::unary(&mut flags, *kind, a, width);
                    }
                    Mnemonic::Shl | Mnemonic::Shr | Mnemonic::Sar => {
                        let count = b & 31;
                        if count != 0 {
                            integer::shift(&mut flags, *kind, a, count, width);
                        }
                    }
                    _ => {
                        integer::binary(&mut flags, *kind, a, b, width);
                    }
                }
                u64::from(flags)
            }
        };
        value & self.width.mask()
    }
}
