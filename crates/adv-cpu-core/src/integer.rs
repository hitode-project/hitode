//! Shared x86-32 integer operands and arithmetic semantics.
//!
//! Tier 0 owns instruction decoding; [`decode`] exposes its supported integer
//! operations to preparation-time analysis. Concrete execution and symbolic
//! state restoration use the same register slices, addresses and flag rules.
use iced_x86::{Code, ConditionCode, Instruction, Mnemonic as M, OpKind, Register as R};

pub const CF: u32 = 1;
pub const PF: u32 = 1 << 2;
pub const AF: u32 = 1 << 4;
pub const ZF: u32 = 1 << 6;
pub const SF: u32 = 1 << 7;
pub const OF: u32 = 1 << 11;

/// The integer subset exported by Tier 0's decoder. Atomic, stack-control,
/// floating and external effects are intentionally outside this interface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Nop,
    Mov(Operand, Operand, bool),
    Lea(Reg, Address),
    Binary(M, Operand, Operand),
    Unary(M, Operand),
    Shift(M, Operand, Operand),
    Branch(Operand, ConditionCode),
    Set(Operand, ConditionCode),
    Cmov(Operand, Operand, ConditionCode),
}

/// Decode with Tier 0's operand and prefix rules, without executing code.
/// Relocatable-region callers additionally check the encoded branch width.
pub fn decode(instruction: &Instruction) -> Option<Operation> {
    if instruction.has_lock_prefix() {
        return None;
    }
    crate::interpreter::integer_operation(instruction)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Reg {
    pub index: u8,
    pub shift: u8,
    pub width: u8,
}

impl Reg {
    pub fn decode(r: R) -> Option<Self> {
        let number = r as u32;
        let (index, shift, width) = if (R::EAX as u32..=R::EDI as u32).contains(&number) {
            (number - R::EAX as u32, 0, 4)
        } else if (R::AX as u32..=R::DI as u32).contains(&number) {
            (number - R::AX as u32, 0, 2)
        } else if (R::AL as u32..=R::BL as u32).contains(&number) {
            (number - R::AL as u32, 0, 1)
        } else if (R::AH as u32..=R::BH as u32).contains(&number) {
            (number - R::AH as u32, 8, 1)
        } else {
            return None;
        };
        Some(Self {
            index: index as u8,
            shift,
            width,
        })
    }
    pub fn read(self, regs: &[u32; 8]) -> u32 {
        (regs[self.index as usize] >> self.shift) & mask(self.width)
    }
    pub fn write(self, regs: &mut [u32; 8], value: u32) {
        let mask = mask(self.width) << self.shift;
        let slot = &mut regs[self.index as usize];
        *slot = (*slot & !mask) | ((value << self.shift) & mask);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Address {
    pub base: Option<Reg>,
    pub index: Option<Reg>,
    pub scale: u8,
    pub displacement: u32,
    pub word: bool,
    pub fs: bool,
}

impl Address {
    pub fn decode(i: &Instruction, lea: bool) -> Option<Self> {
        let base = if i.memory_base() == R::None {
            None
        } else {
            Some(Reg::decode(i.memory_base())?)
        };
        let index = if i.memory_index() == R::None {
            None
        } else {
            Some(Reg::decode(i.memory_index())?)
        };
        // XLAT's implicit address is the sole byte-indexed form: iced supplies
        // BX/EBX + unsigned AL. The usual address-width wrap still precedes FS.
        if base.is_some_and(|r| r.width == 1)
            || (index.is_some_and(|r| r.width == 1)
                && !(i.code() == Code::Xlat_m8 && i.memory_index() == R::AL))
        {
            return None;
        }
        if !lea
            && !matches!(
                i.memory_segment(),
                R::DS | R::SS | R::ES | R::CS | R::FS | R::GS
            )
        {
            return None;
        }
        // In a displacement-only address the decoder retains the address width
        // in the encoded displacement size (16 or 32 bits).
        let word = base
            .or(index)
            .map_or(i.memory_displ_size() == 2, |r| r.width == 2);
        Some(Self {
            base,
            index,
            scale: i.memory_index_scale() as u8,
            displacement: i.memory_displacement32(),
            word,
            fs: !lea && i.memory_segment() == R::FS,
        })
    }
    pub fn resolve_regs(self, regs: &[u32; 8], fs_base: u32) -> u32 {
        let mut offset = self.displacement;
        if let Some(r) = self.base {
            offset = offset.wrapping_add(r.read(regs));
        }
        if let Some(r) = self.index {
            offset = offset.wrapping_add(r.read(regs).wrapping_mul(u32::from(self.scale)));
        }
        if self.word {
            offset &= 0xffff;
        }
        offset.wrapping_add(if self.fs { fs_base } else { 0 })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Operand {
    Register(Reg),
    Memory(Address, u8),
    Immediate(u32),
}

impl Operand {
    pub fn decode(i: &Instruction, n: u32) -> Option<Self> {
        Some(match i.op_kind(n) {
            OpKind::Register => Self::Register(Reg::decode(i.op_register(n))?),
            OpKind::Memory => {
                let width = i.memory_size().size();
                if !matches!(width, 1 | 2 | 4) {
                    return None;
                }
                Self::Memory(Address::decode(i, false)?, width as u8)
            }
            OpKind::Immediate8 => Self::Immediate(u32::from(i.immediate8())),
            OpKind::Immediate16 => Self::Immediate(u32::from(i.immediate16())),
            OpKind::Immediate32 => Self::Immediate(i.immediate32()),
            OpKind::Immediate8to16 => Self::Immediate(i.immediate8to16() as u32),
            OpKind::Immediate8to32 => Self::Immediate(i.immediate8to32() as u32),
            OpKind::NearBranch32 => Self::Immediate(i.near_branch32()),
            OpKind::NearBranch16 => Self::Immediate(u32::from(i.near_branch16())),
            _ => return None,
        })
    }
    pub fn width(self) -> Option<u8> {
        match self {
            Self::Register(r) => Some(r.width),
            Self::Memory(_, width) => Some(width),
            Self::Immediate(_) => None,
        }
    }
    pub fn writable(self) -> bool {
        !matches!(self, Self::Immediate(_))
    }
}

pub(crate) fn flag(flags: &mut u32, bit: u32, value: bool) {
    *flags = (*flags & !bit) | if value { bit } else { 0 };
}

pub fn szp(flags: &mut u32, value: u32, width: u8) {
    let value = value & mask(width);
    flag(flags, ZF, value == 0);
    flag(flags, SF, value & (1 << (u32::from(width) * 8 - 1)) != 0);
    flag(flags, PF, (value as u8).count_ones().is_multiple_of(2));
}
pub fn arithmetic(flags: &mut u32, a: u32, b: u32, carry: u32, width: u8, subtract: bool) -> u32 {
    let a = a & mask(width);
    let b = b & mask(width);
    let result = if subtract {
        a.wrapping_sub(b).wrapping_sub(carry)
    } else {
        a.wrapping_add(b).wrapping_add(carry)
    } & mask(width);
    let wide = if subtract {
        signed(a, width) - signed(b, width) - i64::from(carry)
    } else {
        signed(a, width) + signed(b, width) + i64::from(carry)
    };
    let limit = 1i64 << (u32::from(width) * 8 - 1);
    flag(
        flags,
        CF,
        if subtract {
            u64::from(a) < u64::from(b) + u64::from(carry)
        } else {
            u64::from(a) + u64::from(b) + u64::from(carry) > u64::from(mask(width))
        },
    );
    flag(flags, OF, wide < -limit || wide >= limit);
    flag(flags, AF, (a ^ b ^ result) & 0x10 != 0);
    szp(flags, result, width);
    result
}

pub fn binary(flags: &mut u32, kind: M, a: u32, b: u32, width: u8) -> u32 {
    let carry = u32::from(*flags & CF != 0 && matches!(kind, M::Adc | M::Sbb));
    match kind {
        M::Add | M::Adc => arithmetic(flags, a, b, carry, width, false),
        M::Sub | M::Sbb | M::Cmp => arithmetic(flags, a, b, carry, width, true),
        _ => {
            let value = match kind {
                M::And | M::Test => a & b,
                M::Or => a | b,
                M::Xor => a ^ b,
                _ => unreachable!(),
            };
            // AF is undefined; hardware clears it, as Unicorn and native code do.
            *flags &= !(CF | OF | AF);
            szp(flags, value, width);
            value
        }
    }
}

pub fn unary(flags: &mut u32, kind: M, value: u32, width: u8) -> u32 {
    match kind {
        M::Not => !value,
        M::Neg => arithmetic(flags, 0, value, 0, width, true),
        _ => {
            let carry = *flags & CF;
            let result = arithmetic(flags, value, 1, 0, width, kind == M::Dec);
            *flags = (*flags & !CF) | carry;
            result
        }
    }
}

pub fn shift(flags: &mut u32, kind: M, value: u32, count: u32, width: u8) -> u32 {
    let bits = u32::from(width) * 8;
    let value = value & mask(width);
    let sign = value & (1 << (bits - 1)) != 0;
    let old_cf = *flags & CF != 0;
    match kind {
        M::Shl => {
            if count <= bits {
                flag(flags, CF, value & (1 << (bits - count)) != 0);
            }
            let result = (u64::from(value) << count) as u32 & mask(width);
            if count == 1 {
                flag(
                    flags,
                    OF,
                    (result & (1 << (bits - 1)) != 0) != (*flags & CF != 0),
                );
            }
            szp(flags, result, width);
            result
        }
        M::Shr => {
            if count <= bits {
                flag(flags, CF, value & (1 << (count - 1)) != 0);
            }
            let result = value >> count;
            if count == 1 {
                flag(flags, OF, sign);
            }
            szp(flags, result, width);
            result
        }
        M::Sar => {
            flag(
                flags,
                CF,
                if count >= bits {
                    sign
                } else {
                    value & (1 << (count - 1)) != 0
                },
            );
            let result = (signed(value, width) >> count) as u32 & mask(width);
            if count == 1 {
                flag(flags, OF, false);
            }
            szp(flags, result, width);
            result
        }
        M::Rol | M::Ror => {
            let n = count % bits;
            let result = if n == 0 {
                value
            } else if kind == M::Rol {
                (value << n) | (value >> (bits - n))
            } else {
                (value >> n) | (value << (bits - n))
            } & mask(width);
            flag(
                flags,
                CF,
                if kind == M::Rol {
                    result & 1 != 0
                } else {
                    result & (1 << (bits - 1)) != 0
                },
            );
            if count == 1 {
                flag(
                    flags,
                    OF,
                    if kind == M::Rol {
                        (result & (1 << (bits - 1)) != 0) != (*flags & CF != 0)
                    } else {
                        (result >> (bits - 1)) ^ (result >> (bits - 2)) & 1 != 0
                    },
                );
            }
            result
        }
        M::Rcl | M::Rcr => {
            let n = count % (bits + 1);
            if n == 0 {
                return value;
            }
            let extended = (u64::from(old_cf) << bits) | u64::from(value);
            let all = (1u64 << (bits + 1)) - 1;
            let extended = if kind == M::Rcl {
                ((extended << n) | (extended >> (bits + 1 - n))) & all
            } else {
                ((extended >> n) | (extended << (bits + 1 - n))) & all
            };
            let result = extended as u32 & mask(width);
            flag(flags, CF, extended & (1u64 << bits) != 0);
            if count == 1 {
                flag(
                    flags,
                    OF,
                    if kind == M::Rcl {
                        (result & (1 << (bits - 1)) != 0) != (*flags & CF != 0)
                    } else {
                        ((result >> (bits - 1)) ^ (result >> (bits - 2))) & 1 != 0
                    },
                );
            }
            result
        }
        _ => unreachable!(),
    }
}

/// `shld` (`left`) or `shrd` of a word or dword `value` by `count` in 1 to
/// 31, filling from `source`.
pub fn double_shift(
    flags: &mut u32,
    left: bool,
    value: u32,
    source: u32,
    count: u32,
    width: u8,
) -> u32 {
    let bits = u32::from(width) * 8;
    let concat = if left {
        (u64::from(value) << bits) | u64::from(source)
    } else {
        (u64::from(source) << bits) | u64::from(value)
    };
    // 16-bit counts 17..31 are architecturally undefined. Use the
    // conventional 32-bit concatenation rotation, including the original
    // destination, rather than host shift UB.
    let result = if width == 2 && left {
        (concat as u32).rotate_left(count) >> 16
    } else if width == 2 {
        (concat as u32).rotate_right(count)
    } else if left {
        ((concat << count) >> bits) as u32
    } else {
        (concat >> count) as u32
    } & mask(width);
    let carry = if left { bits * 2 - count } else { count - 1 };
    flag(flags, CF, concat & (1u64 << carry) != 0);
    if count == 1 {
        flag(flags, OF, ((value ^ result) >> (bits - 1)) & 1 != 0);
    }
    szp(flags, result, width);
    result
}

pub fn mask(width: u8) -> u32 {
    match width {
        1 => 0xff,
        2 => 0xffff,
        4 => u32::MAX,
        _ => unreachable!(),
    }
}

pub fn signed(value: u32, width: u8) -> i64 {
    match width {
        1 => i64::from(value as i8),
        2 => i64::from(value as i16),
        4 => i64::from(value as i32),
        _ => unreachable!(),
    }
}

pub fn condition_true(condition: ConditionCode, flags: u32) -> bool {
    use ConditionCode::*;
    let carry = flags & CF != 0;
    let zero = flags & ZF != 0;
    let sign = flags & SF != 0;
    let overflow = flags & OF != 0;
    match condition {
        None => true,
        o => overflow,
        no => !overflow,
        b => carry,
        ae => !carry,
        e => zero,
        ne => !zero,
        be => carry || zero,
        a => !carry && !zero,
        s => sign,
        ns => !sign,
        p => flags & PF != 0,
        np => flags & PF == 0,
        l => sign != overflow,
        ge => sign == overflow,
        le => zero || sign != overflow,
        g => !zero && sign == overflow,
    }
}
