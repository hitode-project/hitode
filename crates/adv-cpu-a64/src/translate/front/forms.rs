//! Prepared operands and instruction forms consumed by both lowerings.

use adv_cpu_core::floating::fast::Fast;

use crate::encode::{Float, Logic, Narrowing, Permute, Same, ShiftImm};

use super::decode::{Mem, Part, ShiftKind, Src};
use super::flags::FlagSet;
use super::tags::{ConditionCode, Mnemonic as M};

// Integer operations and their trace effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integer {
    NarrowShift(ShiftKind, u8, u8, u32),
    NarrowNeg(u8, u8),
    /// BSF (false) or BSR of a word/dword source, retaining dst on zero.
    Scan(bool, u8, Src, u8),
    /// `rcl` (true) or `rcr` of a 32-bit register by 1 to 31.
    Through(bool, u8, u32),
    /// `xadd dst, src`.
    Xadd(Src, u8),
    /// `xchg dst, src`.
    Xchg(Src, u8),
    CompareExchange8(Mem),
    Cbw,
    Bswap(u8),
    /// `mul` or `imul` (true) of EAX into EDX:EAX.
    Multiply(Src, bool),
    /// `div` or `idiv` (true) of EDX:EAX, with the helper that raises #DE.
    Divide(Src, bool, u8),
    Sahf,
    Lea16(u8, Mem),
    ReadSegment(Part, u8, u8),
    TestBit(Src, u8),
    Bit(M, Part, Part, u8),
    Product(Part, Option<(Part, Option<u32>)>, bool, u8),
    Quotient(Part, bool, u8, u8),
    Exchange(M, Part, u8, u8),
    Cwd,
    MemoryNeg(Part, u8),
    Set(ConditionCode, Part),
    Cmov(ConditionCode, Part, Part, u8),
    Extend(Part, Part, u8, u8, bool),
    Complement(Part, u8),
    ClearWord(u8),
    Touch(Mem),
    Xlat(Mem),
    /// `bound reg, mem` of `width` bytes: #BR unless the signed register is
    /// inside the signed pair at `mem`.
    Bound(u8, Mem, u8),
    CarryFlag(M),
}

impl Integer {
    /// Flags read, flags written, and whether it has a fault site.
    pub fn effects(self) -> (FlagSet, FlagSet, bool) {
        use FlagSet as F;
        let mem = |src: Src| matches!(src, Src::Mem(_));
        match self {
            Integer::Bit(_, dst, _, _) => (F::NONE, F::CF, matches!(dst, Part::Mem(_))),
            Integer::Product(src, other, _, _) => (
                F::NONE,
                F::CF.or(F::OF),
                matches!(src, Part::Mem(_)) || matches!(other, Some((Part::Mem(_), _))),
            ),
            Integer::Quotient(..) => (F::NONE, F::NONE, true),
            Integer::Exchange(kind, dst, _, _) => (
                F::NONE,
                if kind == M::Xchg { F::NONE } else { F::ALL },
                matches!(dst, Part::Mem(_)),
            ),
            Integer::MemoryNeg(_, _) => (F::NONE, F::ALL, true),
            Integer::Set(cc, dst) => (F::of(cc), F::NONE, matches!(dst, Part::Mem(_))),
            Integer::Cmov(cc, _, src, _) => (F::of(cc), F::NONE, matches!(src, Part::Mem(_))),
            Integer::Extend(_, src, _, _, _) => (F::NONE, F::NONE, matches!(src, Part::Mem(_))),
            Integer::Cwd | Integer::ClearWord(_) => (F::NONE, F::NONE, false),
            Integer::Complement(dst, _) => (F::NONE, F::NONE, matches!(dst, Part::Mem(_))),
            Integer::Touch(_) | Integer::Xlat(_) | Integer::Bound(..) => (F::NONE, F::NONE, true),
            Integer::CarryFlag(kind) => {
                (if kind == M::Cmc { F::CF } else { F::NONE }, F::CF, false)
            }
            Integer::NarrowNeg(..) => (F::NONE, F::ALL, false),
            Integer::NarrowShift(kind, _, width, count) => {
                let mut writes = F::PF.or(F::ZF).or(F::SF);
                if kind == ShiftKind::Sar || count <= u32::from(width) * 8 {
                    writes = writes.or(F::CF);
                }
                if count == 1 {
                    writes = writes.or(F::OF);
                }
                if count == 0 {
                    writes = F::NONE;
                }
                (F::NONE, writes, false)
            }
            Integer::Scan(_, _, src, _) => (F::NONE, F::ZF, mem(src)),
            Integer::Through(_, _, count) => {
                let overflow = if count == 1 { F::OF } else { F::NONE };
                (F::CF, F::CF.or(overflow), false)
            }
            Integer::Xadd(dst, _) => (F::NONE, F::ALL, mem(dst)),
            Integer::Xchg(dst, _) => (F::NONE, F::NONE, mem(dst)),
            Integer::CompareExchange8(..) => (F::NONE, F::ZF, true),
            Integer::Bswap(_) | Integer::Cbw => (F::NONE, F::NONE, false),
            Integer::Multiply(src, _) => (F::NONE, F::CF.or(F::OF), mem(src)),
            // The helper reads every flag.
            Integer::Divide(..) => (F::NONE, F::NONE, true),
            Integer::Lea16(..) => (F::NONE, F::NONE, false),
            Integer::ReadSegment(_, _, _) => (F::NONE, F::NONE, true),
            Integer::Sahf => (F::NONE, F::ALL.minus(F::OF), false),
            Integer::TestBit(src, _) => (F::NONE, F::CF, mem(src)),
        }
    }
}

// Integer operations over partial operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// The masked immediate count, 0 to 31.
    Imm(u8),
    Cl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    pub kind: M,
    /// 1, 2 or 4 bytes.
    pub width: u8,
    pub dst: Part,
    /// A binary source, or the register a double shift fills from, and its width.
    pub source: Option<(Part, u8)>,
    /// A shift's count.
    pub count: Option<Count>,
}

impl Parts {
    /// The operand in memory, if one is.
    pub fn memory(&self) -> Option<Mem> {
        [Some(self.dst), self.source.map(|(part, _)| part)]
            .into_iter()
            .find_map(|part| match part {
                Some(Part::Mem(mem)) => Some(mem),
                _ => None,
            })
    }
}

// String operations and prefixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringKind {
    Movs,
    Stos,
    Lods,
    Scas,
    Cmps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rep {
    None,
    /// `rep`, or `repe` before a compare.
    #[allow(clippy::enum_variant_names)] // The x86 prefix name.
    Rep,
    /// `repne` before a compare.
    Repne,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strings {
    pub kind: StringKind,
    /// 1, 2 or 4 bytes.
    pub width: u8,
    pub rep: Rep,
    pub word: bool,
    pub fs: bool,
}

impl Strings {
    /// Whether it writes the arithmetic flags, which a REP of no elements
    /// leaves as they were.
    pub fn compares(self) -> bool {
        matches!(self.kind, StringKind::Scas | StringKind::Cmps)
    }
}

// x87 operation and its decoded operands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X87 {
    pub fast: Fast,
    /// The memory operand, if any.
    pub mem: Option<Mem>,
    /// The helper that runs the instruction otherwise.
    pub helper: u8,
    /// The opcode the instruction records, if it records one.
    pub opcode: Option<u16>,
    /// For `FCOMI`/`FCOMIP`/`FUCOMI`/`FUCOMIP` (lowered with `Fast::Compare`):
    /// `Some(quiet)` (`true` for `FUCOMI*`, `false` for signaling `FCOMI*`).
    /// `None` for status-word `FCOM*`/`FUCOM*` and all other forms.
    pub flag_compare: Option<bool>,
}

impl X87 {
    pub(crate) fn waits(self) -> bool {
        !matches!(self.fast, Fast::StoreControl | Fast::StoreStatus)
    }

    /// Returns `Some(quiet)` for `FCOMI`/`FCOMIP`/`FUCOMI`/`FUCOMIP`.
    pub(crate) fn flag_compare(self) -> Option<bool> {
        self.flag_compare
    }
}

/// An operand of a SIMD instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc {
    Mmx(u8),
    Xmm(u8),
    Gpr(u8),
    /// `Simd::mem`, of `Simd::width` bytes.
    Mem,
    /// `Simd::imm`, or no operand.
    Imm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimdKind {
    /// `dst = src`, zero-extending a narrower value into a register.
    Move,
    /// Scalar register moves: only the lane of `1 << log2` bytes changes.
    MoveLow(u8),
    MoveHalf(bool),
    MaskMove,
    PackedToInt(bool, bool),
    SignMask(bool),
    ShuffleWords(bool),
    Shufpd,
    MulDwords,
    SadBytes,
    /// A selected 16-bit lane, zero-extended into a GPR.
    ExtractWord,
    /// One sign bit per byte of an MMX or XMM source.
    ByteMask,
    InsertWord,
    ShiftBytes(bool),
    StoreControl,
    LoadControl,
    /// Scalar integer conversion: double precision selector.
    ScalarFromInt(bool),
    /// Scalar to signed dword: double precision and truncation selectors.
    ScalarToInt(bool, bool),
    /// A lane-wise operation on lanes of `1 << log2` bytes.
    Same(Same, u8),
    Logic(Logic),
    /// `pandn`: `!dst & src`.
    AndNot,
    /// Interleaves the low (`Zip1`) or high (`Zip2`) halves.
    Unpack(Permute, u8),
    /// Saturates the lanes of dst then src to lanes of `1 << log2` bytes.
    Pack(Narrowing, u8),
    ShiftImm(ShiftImm, u8),
    /// A shift by the 64-bit count in src.
    ShiftBy(ShiftImm, u8),
    /// The high halves of signed (true) or unsigned 16-bit products.
    MulHigh(bool),
    /// `pmaddwd`.
    MulAdd,
    /// `pshufw` or `pshufd`: lanes of `1 << log2` bytes of src picked by
    /// `imm`.
    Shuffle(u8),
    Shufps,
    Emms,
    /// `addps`-style arithmetic on every lane, or on the low one (`true`).
    Float(Float, bool),
    /// Double-precision arithmetic, scalar when the selector is true.
    Double(Float, bool),
    /// Signed packed dwords to single or double precision (`true`).
    FromInt(bool),
    /// Single to double precision, packed or scalar (`true`).
    Widen(bool),
    /// Scalar double to single precision.
    Narrow(bool),
    Approximate(bool, bool),
    /// COMIS/UCOMIS, using core value semantics for NaNs: double precision,
    /// and whether a quiet NaN also raises invalid (COMIS).
    FlagCompare(bool, bool),
    /// CMPPS/CMPSS/CMPPD/CMPSD: scalar and double-precision selectors.
    Compare(bool, bool),
    /// `maxps` (true) or `minps`, or their scalar forms (`.1`).
    MinMax(bool, bool, bool),
    Sqrt(bool, bool),
    /// `cvtps2pi`, or `cvttps2pi` (true).
    /// `movlps` or `movhps` (true): a 64-bit half to or from memory.
    Half(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Simd {
    pub kind: SimdKind,
    pub dst: Loc,
    pub src: Loc,
    pub mem: Mem,
    pub imm: u8,
    /// Bytes a move or a memory operand covers.
    pub width: u8,
    /// A 16-byte memory operand that must be aligned.
    pub aligned: bool,
    /// The helper for forms without native lowering.
    pub helper: u8,
}

impl Simd {
    pub fn memory(&self) -> bool {
        self.dst == Loc::Mem || self.src == Loc::Mem || self.kind == SimdKind::MaskMove
    }

    /// Whether it is an MMX instruction, with the x87 effects of one.
    pub fn mmx(&self) -> bool {
        matches!(self.dst, Loc::Mmx(_))
            || matches!(self.src, Loc::Mmx(_))
            || self.kind == SimdKind::Emms
    }

    /// Whether it can require a value call.
    pub fn floating(&self) -> bool {
        matches!(
            self.kind,
            SimdKind::Float(..)
                | SimdKind::Double(..)
                | SimdKind::ScalarFromInt(_)
                | SimdKind::ScalarToInt(..)
                | SimdKind::FromInt(_)
                | SimdKind::Widen(_)
                | SimdKind::Narrow(..)
                | SimdKind::FlagCompare(..)
                | SimdKind::Compare(..)
                | SimdKind::MinMax(..)
                | SimdKind::Sqrt(..)
        )
    }
}

/// ESP and EBP use the biased stack-register representation.
pub(in crate::translate) const fn biased(index: u8) -> bool {
    index == super::decode::ESP || index == super::decode::EBP
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Division {
    /// The dividend is sign extended from EAX and the divisor is positive.
    SignExtended,
    /// EAX is a signed remainder plus its positive divisor (at most 2^30).
    Normalized,
}
