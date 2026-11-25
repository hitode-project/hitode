//! The optimizer's SSA IR. Blocks take parameters instead of phi nodes, and
//! block 0 is the loop head the job enters. A value is defined once, by a
//! block parameter or an instruction, and is used only where its definition
//! dominates. Frame bytes are values; memory outside the frame is reached
//! only through `Load` and `Store`. Guest addresses are 32-bit values that
//! emission offsets by the private alias base.

use crate::translate::front::tags::ConditionCode;
use std::collections::{BTreeMap, BTreeSet};

use super::recovery::IrRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Value(pub u32);

pub(crate) use crate::policy::resident::{MAX_DOMAIN_GUARDS, MAX_PLAIN_FACTS};

/// Disjoint byte ranges in wrapping 32-bit guest address arithmetic.
pub(crate) fn disjoint_ranges(a: u32, aw: u32, b: u32, bw: u32) -> bool {
    b.wrapping_sub(a) >= aw && a.wrapping_sub(b) >= bw
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Block(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RecordId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Type {
    I32,
    I64,
    F64,
    V128,
    /// The EFLAGS a guest operation produces, read through `Test`.
    Flags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntOp {
    Add,
    Sub,
    And,
    Or,
    Xor,
    /// Shifts mask their count to the operand width, as the guest does.
    Shl,
    Shr,
    Sar,
    Ror,
    Mul,
    MulHighSigned,
    MulHighUnsigned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntUnary {
    Not,
    Neg,
    ZeroExtend8,
    ZeroExtend16,
    SignExtend8,
    SignExtend16,
    /// The low half of an I64.
    Low32,
    /// The high half of an I64.
    High32,
    /// An I32 zero-extended to I64.
    Widen,
    /// An I32 sign-extended to I64.
    SignWiden,
    ByteSwap,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum FpOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FpUnary {
    Neg,
    Abs,
}

/// MMX/SSE packed-integer and comparison operations. `full` selects 128-bit
/// XMM vs 64-bit MMX (upper 64 bits zeroed). Shifts take their count from
/// the low 64 bits of the second operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VecOp {
    And(bool),
    AndNot(bool),
    Or(bool),
    Xor(bool),
    Same(crate::encode::Same, u8, bool),
    Unpack(crate::encode::Permute, u8, bool),
    Pack(crate::encode::Narrowing, u8, bool),
    MulHigh(bool, bool),
    MulAdd(bool),
    Shift(crate::encode::ShiftImm, u8, bool),
    Shufps(u8),
    Shufpd(u8),
    MulDwords(bool),
    SadBytes(bool),
    MoveLow32,
    Insert32(u8),
    Insert64(bool, bool),
    InsertWord(u8, bool),
    /// `cmpltpd`: all ones in each lane where the first is less.
    LessF64,
}

/// Unary vector lane shifts, half extracts, and immediate shuffles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VecUnary {
    ShiftImm(crate::encode::ShiftImm, u8, u8, bool),
    /// Narrows a binary64 scalar to binary32.
    NarrowScalar,
    /// Widens a binary32 scalar in lane 0 to binary64 in lane 0.
    WidenScalar,
    /// Converts 4 signed `i32` lanes to 4 `f32` lanes (`cvtdq2ps`).
    FromInt4s,
    /// Converts low 2 signed `i32` lanes to 2 `f64` lanes (`cvtdq2pd`).
    FromInt2d,
    /// Broadcasts lane 0 (`f64` / low 64 bits) into `.2d`.
    Dup64,
    /// Broadcasts lane 0 (`f32` / low 32 bits) into `.4s`.
    Dup32,
    /// Zeroes bits 64..128 of a V128 value.
    Low64,
    /// Copies bits 64..128 to bits 0..64 and zeroes bits 64..128.
    High64,
    Shuffle(u8, u8, bool),
    ShuffleWords(bool, u8),
    ShiftBytes(bool, u8),
}

/// Vector-to-I32 extractions and sign/byte masks (`pextrw`, `pmovmskb`, `movmskps`/`pd`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VecToInt {
    ExtractWord(u8),
    /// Extracts 32-bit lane `0..4` (`FMOV W, S` for lane 0, `UMOV W, V.s[lane]` otherwise).
    Extract32(u8),
    ByteMask(bool),
    SignMask(bool),
}

/// The guest operation whose EFLAGS a `Flags` value holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlagKind {
    Add,
    AddCarry,
    Sub,
    SubBorrow,
    Logic,
    Increment,
    Decrement,
    Negate,
    Shl,
    Shr,
    Sar,
    MulSigned,
    /// `ucomisd`/`ucomiss`/`fucomi`: ZF, PF and CF from an unordered comparison.
    UnorderedCompare,
    /// `comisd`/`comiss`/`fcomi`: the same flags, but a quiet NaN also raises invalid.
    OrderedCompare,
    CarryBit,
    MulUnsigned,
    Rol,
    Ror,
    /// `sahf`: loads SF, ZF, AF, PF, CF from `a` (bits 7:0 = AH), preserving OF.
    Sahf,
    /// Folded `fcom`/`fucom`/`ftst` + `fnstsw ax` + `sahf` (AX dead): compares F64
    /// `a` and `b`, preserves OF from `prior`, sets AF from `width & 1` (TOP[1]),
    /// clears SF (`ES == 0`), and raises invalid on QNaN when `width & 2 != 0`.
    X87CompareFold,
}

impl FlagKind {
    /// Whether it compares floating-point operands.
    pub(crate) fn floating(self) -> bool {
        matches!(
            self,
            FlagKind::UnorderedCompare | FlagKind::OrderedCompare | FlagKind::X87CompareFold
        )
    }

    /// Whether the flags depend on operand `b`: a logical result's flags,
    /// and a step's or negation's, come from `a` alone.
    pub(crate) fn reads_b(self) -> bool {
        !matches!(
            self,
            FlagKind::Logic
                | FlagKind::Increment
                | FlagKind::Decrement
                | FlagKind::Negate
                | FlagKind::CarryBit
                | FlagKind::MulUnsigned
                | FlagKind::Sahf
        )
    }
}

/// SSE floating-point width conversions (`cvtss2sd`, `cvtps2pd`, `cvtsd2ss`, `cvtpd2ps`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SseConvertKind {
    /// `cvtss2sd`: low f32 -> low f64.
    WidenScalar,
    /// `cvtps2pd`: low 2 f32 -> 2 f64.
    WidenPacked,
    /// `cvtsd2ss`: low f64 -> low f32.
    NarrowScalar,
    /// `cvtpd2ps`: 2 f64 -> low 2 f32 (upper 64 bits zeroed).
    NarrowPacked,
}

/// Predicates for `Block(0)` entry domain guards (`Op::DomainGuard`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DomainGuardKind {
    /// `value` (F64) is finite and either `±0.0` or `2^min_exp <= |value| < 2^(max_exp + 1)`.
    /// When `exact_f32` is true, `f64::from(value as f32) == value` must also hold.
    FpWindow {
        value: Value,
        min_exp: i16,
        max_exp: i16,
        exact_f32: bool,
    },
    /// `a` and `b` (F64) do not have opposite nonzero signs:
    /// `a == 0.0 || b == 0.0 || signbit(a) == signbit(b)`.
    SameSign { a: Value, b: Value },
    /// `value` (F64) is ordered and `<= f64::from_bits(bound_bits)`.
    FpLe { value: Value, bound_bits: u64 },
    /// `value` (I32, unsigned) `<= max`.
    U32Le { value: Value, max: u32 },
    /// `value` (F64) is exact in binary32 (`f64::from(value as f32) == value`).
    ExactF32 { value: Value },
}

/// Proof bits for [`Op::X87Single`] and [`Op::Fma`].
pub(crate) const SINGLE_A: u8 = 1;
pub(crate) const SINGLE_B: u8 = 2;
pub(crate) const SINGLE_RESULT: u8 = 4;
/// Binary64 arithmetic matches binary32 bit-for-bit (implies all operand & result proofs).
pub(crate) const SINGLE_EXACT: u8 = 8;
/// Third operand (`c`) of [`Op::Fma`] is exact in binary32.
pub(crate) const SINGLE_C: u8 = 16;

/// Fused multiply-add/sub sign form for a proved-exact product:
/// - `Add`: `a * b + c` (`FMADD`)
/// - `Sub`: `a * b - c` (`FNMSUB`)
/// - `RevSub`: `c - a * b` (`FMSUB`)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FmaOp {
    Add,
    Sub,
    RevSub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Constant(Type, u128),
    /// Keep a form-entry induction value in a recovery spill cell.
    Snapshot(Value),
    Int(IntOp, Value, Value),
    IntUnary(IntUnary, Value),
    /// The quotient of the I64 `dividend` by the I32 `divisor`. Unless
    /// `proved`, a zero divisor or a quotient that does not fit fails.
    Div {
        signed: bool,
        dividend: Value,
        divisor: Value,
        proved: bool,
    },
    /// The low 32 bits of `dividend` minus `quotient` times `divisor`: the
    /// remainder of the `Div` that gave `quotient`.
    Remainder {
        quotient: Value,
        dividend: Value,
        divisor: Value,
    },
    /// EFLAGS after `kind` on `width`-byte operands, as the interpreter
    /// computes them. Flags the operation keeps, and the carry that ADC, SBB,
    /// INC and DEC read, come from `prior`. `Logic` takes its result as `a`;
    /// shifts take the unmasked count as `b`, and a zero count keeps `prior`.
    /// `UnorderedCompare` and `OrderedCompare` compare F64 operands.
    Flags {
        kind: FlagKind,
        width: u8,
        a: Value,
        b: Value,
        prior: Option<Value>,
    },
    /// 1 when the condition holds for `flags`, else 0.
    Test {
        flags: Value,
        cond: ConditionCode,
    },
    Select {
        cond: Value,
        a: Value,
        b: Value,
    },
    /// Reinterprets bits, truncating or zero-extending to the target width.
    Bits {
        to: Type,
        value: Value,
    },
    /// Binary64 x87 arithmetic; when `proved`, inputs and result cannot fail.
    Fp(FpOp, Value, Value, bool),
    /// x87 precision 24 when both inputs are exact in binary32; `proved` carries `SINGLE_*` bits.
    X87Single(FpOp, Value, Value, u8),
    /// Fused multiply-add/sub for a proved-exact product (`a * b ± c` or `c - a * b`).
    /// For `single == false`, `SINGLE_RESULT` in `proved` elides the post-check;
    /// for `single == true`, `proved` carries `SINGLE_A`/`SINGLE_B`/`SINGLE_C`/`SINGLE_RESULT`/`SINGLE_EXACT`.
    Fma {
        op: FmaOp,
        a: Value,
        b: Value,
        c: Value,
        single: bool,
        proved: u8,
    },
    Fp32(FpOp, Value, Value),
    SseFp64(FpOp, Value, Value),
    FpUnary(FpUnary, Value),
    /// A signed integer as F64; fails when it is not exactly representable.
    FromInt(Value),
    /// F64 to a signed integer, truncating; fails on NaN or out of range.
    ToInt {
        value: Value,
        wide: bool,
        sse: bool,
    },
    /// Packed SSE float-to-int32 conversion (`cvttps2dq`, `cvtps2dq`,
    /// `cvttpd2dq`, `cvtpd2dq`) under default MXCSR with `PE = 1`. Bails to
    /// Tier 1 if any lane is NaN, Inf, or outside the fast-path `i32` range.
    SsePackedToInt {
        value: Value,
        double: bool,
        truncate: bool,
    },
    /// Round to a signed integer or binary32 under an x87 control word.
    /// When `proved`, an integer store fits in `[-2^(8*width-1), 2^(8*width-1))`
    /// and a binary32 store cannot overflow or underflow.
    X87Store {
        value: Value,
        width: u8,
        rounding: u8,
        integer: bool,
        proved: bool,
    },
    /// Whether a checked store increased the magnitude of its operand.
    X87StoreC1 {
        value: Value,
        stored: Value,
        integer: bool,
    },
    Vec(VecOp, Value, Value),
    VecUnary(VecUnary, Value),
    VecToInt(VecToInt, Value),
    /// Proved-safe lane-wise `.2d` (`double = true`) or `.4s` (`double = false`) FP arithmetic.
    VecFp {
        op: FpOp,
        double: bool,
        a: Value,
        b: Value,
    },
    /// Proved-safe by-element `.2d` or `.4s` FP multiply: `a * b[lane]`.
    VecFpMulElem {
        double: bool,
        a: Value,
        b: Value,
        lane: u8,
    },
    /// Proved-safe de-interleaving AoS load (`LD2`/`LD3`/`LD4`), returning `channel` of `channels`.
    VecDeintLoad {
        addr: Value,
        channels: u8,
        channel: u8,
        double: bool,
        pc: u32,
    },
    /// Proved-safe broadcast-from-memory load (`LD1R`) into `.4s` or `.2d`.
    VecLoadBcast {
        addr: Value,
        double: bool,
        pc: u32,
    },
    /// Proved-safe lane-wise `.2d` or `.4s` fused multiply-add/sub: `c + a * b` or `c - a * b`.
    VecFma {
        sub: bool,
        double: bool,
        a: Value,
        b: Value,
        c: Value,
    },
    /// Proved-safe lane-wise `.2d` (`f64 -> i64`) or `.4s` (`f32 -> i32`) rounding conversion.
    VecFpToInt {
        value: Value,
        double: bool,
        rounding: u8,
    },
    /// A memory load of `width` bytes, zero-extended to `ty`.
    Load {
        ty: Type,
        width: u8,
        addr: Value,
        /// Tests page classes first, as Tier-1 checked accesses do.
        checked: bool,
        pc: u32,
        scope: Option<u8>,
        /// A shared clock observation stays at its original instruction.
        clock: bool,
    },
    /// A memory store: an effect point. Region `after` starts at
    /// the guest boundary after it.
    Store {
        width: u8,
        addr: Value,
        value: Value,
        after: RecordId,
        /// Tests page classes first, as Tier-1 checked accesses do.
        checked: bool,
        pc: u32,
        scope: Option<u8>,
    },
    /// An entry fact of a resident form: the pages `addr..addr + bytes`
    /// are ordinary (A64-MEM-14). Only the entry block holds these, before
    /// any effect; a failure is an environment miss.
    PlainPage {
        addr: Value,
        bytes: u32,
        write: bool,
    },
    /// An entry numerical domain guard in `Block(0)` before any guest side
    /// effect. Failure routes through the separate `DomainGuardMiss` path
    /// keyed by `func.domain_guards[ordinal]`, never body `Validation`.
    DomainGuard {
        kind: DomainGuardKind,
        ordinal: u8,
    },
    /// Protects a crossing store whose region already read those bytes.
    /// Recovers to `record` if either page is not ordinary and writable.
    PageCross {
        addr: Value,
        width: u8,
        record: RecordId,
    },
    /// Recovers to the store's region start if its bytes intersect the frame.
    FrameDisjoint {
        addr: Value,
        width: u8,
        base: Value,
        lo: i32,
        hi: i32,
        record: RecordId,
    },
    /// The host address of a guest address: the base frame accesses add
    /// their offsets to.
    Host(Value),
    /// A frame cell of 4 or 8 bytes, or 8 bytes as F64, at `offset` from
    /// the host address `base`. Admission maps the frame, so it cannot fail; it reads the
    /// cell's value whenever no value of the cell is held (A64-OPT-3).
    FrameLoad {
        ty: Type,
        base: Value,
        offset: i32,
    },
    /// Commits a frame cell's value to memory on an edge into a loop head,
    /// after validation: only operations that cannot fail follow it before
    /// the head (A64-OPT-3).
    FrameStore {
        base: Value,
        offset: i32,
        value: Value,
    },
    /// The x87 reading of binary64 bits, or of binary32 bits widened
    /// exactly when `single`. Fails on NaN and, when `denormal`, subnormals,
    /// unless `proved`.
    X87Load {
        bits: Value,
        denormal: bool,
        single: bool,
        proved: bool,
    },
    /// Checks raw SSE lanes for denormals when MXCSR.DE is clear.
    SseInput {
        a: Value,
        b: Option<Value>,
        packed: bool,
        single: bool,
        pc: u32,
    },
    /// Accumulate actual writes for the owner's scoped GPR join.
    ScopeWritten(u32),
    /// x87 arithmetic with directed rounding (`rounding != 0`) or PC64 (`pc == 3`).
    /// `pc`: 0 = PC24, 2 = PC53, 3 = PC64. `rounding`: 0..=3 (RN, RD, RU, RZ).
    X87Arith {
        op: FpOp,
        a: Value,
        b: Value,
        pc: u8,
        rounding: u8,
        proved: bool,
    },
    /// x87 `fsqrt` under precision `pc` (0/2/3) and rounding `rounding` (0..=3).
    X87Sqrt {
        value: Value,
        pc: u8,
        rounding: u8,
        proved: bool,
    },
    /// x87 `frndint` under rounding `rounding` (0..=3).
    X87Round {
        value: Value,
        rounding: u8,
        denormal: bool,
        proved: bool,
    },
    /// x87 `fcom`/`fucom`/`ftst`: returns status CC mask `(C3<<14 | C2<<10 | C0<<8)`.
    X87Compare {
        a: Value,
        b: Value,
        ordered: bool,
    },
    /// x87 `fxam`: returns status mask `(C3<<14 | C2<<10 | C1<<9 | C0<<8)`.
    X87Exam(Value),
    /// Evaluates x87 C1 (0 or 1) for an arithmetic rounding operation (used on
    /// `merge_c1` joins and directed/PC64 arithmetic).
    X87RoundC1 {
        op: FpOp,
        a: Value,
        b: Value,
        pc: u8,
        rounding: u8,
    },
    /// Evaluates x87 C1 (0 or 1) for `fsqrt`: `result * result > input`.
    X87SqrtC1 {
        input: Value,
        result: Value,
    },
    /// Packed SSE FP arithmetic (`addps`/`subps`/`mulps`/`divps`, `addpd`..`divpd`).
    SsePackedFp {
        op: FpOp,
        double: bool,
        a: Value,
        b: Value,
        proved: bool,
    },
    /// Scalar or packed SSE `min`/`max` (`minss`/`maxss`/`minsd`/`maxsd`/`minps`/`maxps`/`minpd`/`maxpd`).
    SseMinMax {
        max: bool,
        scalar: bool,
        double: bool,
        a: Value,
        b: Value,
        proved: bool,
    },
    /// Scalar or packed SSE `sqrt` (`sqrtss`/`sqrtsd`/`sqrtps`/`sqrtpd`).
    SseSqrt {
        scalar: bool,
        double: bool,
        a: Value,
        proved: bool,
    },
    /// SSE floating-point conversions (`cvtss2sd`, `cvtps2pd`, `cvtsd2ss`, `cvtpd2ps`).
    SseConvert {
        kind: SseConvertKind,
        a: Value,
        proved: bool,
    },
    /// Scalar SSE float-to-int32 conversion (`cvtss2si`, `cvttss2si`, `cvtsd2si`, `cvttsd2si`).
    SseScalarToInt {
        value: Value,
        double: bool,
        truncate: bool,
    },
    /// Explicit SSA floating-point validation of up to 4 `F64` values placed
    /// before an effect point or barrier (A64-OPT-4, A64-OPT-5).
    /// Bit `i` in `strict` excludes the `2^-1022` binade and unproved zeros (for `Mul`).
    FpValidate {
        values: [Option<Value>; 4],
        strict: u8,
    },
    /// Effect boundary for floating-point checks.
    Validate,
}

impl Op {
    pub(crate) fn uses(&self) -> impl Iterator<Item = Value> {
        let list: [Option<Value>; 4] = match *self {
            Op::Constant(..) | Op::Validate | Op::ScopeWritten(_) => [None; 4],
            Op::FpValidate { values, .. } => values,
            Op::SseInput { a, b, .. } => [Some(a), b, None, None],
            Op::DomainGuard { kind, .. } => match kind {
                DomainGuardKind::FpWindow { value: a, .. }
                | DomainGuardKind::FpLe { value: a, .. }
                | DomainGuardKind::U32Le { value: a, .. }
                | DomainGuardKind::ExactF32 { value: a } => [Some(a), None, None, None],
                DomainGuardKind::SameSign { a, b } => [Some(a), Some(b), None, None],
            },
            Op::Snapshot(a)
            | Op::IntUnary(_, a)
            | Op::Bits { value: a, .. }
            | Op::FpUnary(_, a)
            | Op::VecUnary(_, a)
            | Op::VecToInt(_, a)
            | Op::VecFpToInt { value: a, .. }
            | Op::FromInt(a)
            | Op::ToInt { value: a, .. }
            | Op::SsePackedToInt { value: a, .. }
            | Op::SseScalarToInt { value: a, .. }
            | Op::X87Store { value: a, .. }
            | Op::X87Sqrt { value: a, .. }
            | Op::X87Round { value: a, .. }
            | Op::X87Exam(a)
            | Op::SseSqrt { a, .. }
            | Op::SseConvert { a, .. }
            | Op::Load { addr: a, .. }
            | Op::VecDeintLoad { addr: a, .. }
            | Op::VecLoadBcast { addr: a, .. }
            | Op::PlainPage { addr: a, .. }
            | Op::PageCross { addr: a, .. }
            | Op::Host(a)
            | Op::FrameLoad { base: a, .. }
            | Op::X87Load { bits: a, .. }
            | Op::Test { flags: a, .. } => [Some(a), None, None, None],
            Op::Int(_, a, b)
            | Op::Fp(_, a, b, _)
            | Op::X87Single(_, a, b, _)
            | Op::X87Arith { a, b, .. }
            | Op::X87Compare { a, b, .. }
            | Op::X87RoundC1 { a, b, .. }
            | Op::X87SqrtC1 {
                input: a,
                result: b,
            }
            | Op::Fp32(_, a, b)
            | Op::SseFp64(_, a, b)
            | Op::SsePackedFp { a, b, .. }
            | Op::SseMinMax { a, b, .. }
            | Op::Vec(_, a, b)
            | Op::VecFp { a, b, .. }
            | Op::VecFpMulElem { a, b, .. }
            | Op::X87StoreC1 {
                value: a,
                stored: b,
                ..
            }
            | Op::Div {
                dividend: a,
                divisor: b,
                ..
            }
            | Op::Store {
                addr: a, value: b, ..
            }
            | Op::FrameStore {
                base: a, value: b, ..
            }
            | Op::FrameDisjoint {
                addr: a, base: b, ..
            } => [Some(a), Some(b), None, None],
            Op::Flags { a, b, prior, .. } => [Some(a), Some(b), prior, None],
            Op::Select { cond, a, b }
            | Op::Fma {
                a: cond,
                b: a,
                c: b,
                ..
            }
            | Op::VecFma {
                a: cond,
                b: a,
                c: b,
                ..
            }
            | Op::Remainder {
                quotient: cond,
                dividend: a,
                divisor: b,
            } => [Some(cond), Some(a), Some(b), None],
        };
        list.into_iter().flatten()
    }

    /// Whether the operation requires validation before an effect point.
    pub(crate) fn floating(&self) -> bool {
        match *self {
            Op::Fp(_, _, _, proved)
            | Op::X87Arith { proved, .. }
            | Op::X87Sqrt { proved, .. }
            | Op::X87Round { proved, .. }
            | Op::SsePackedFp { proved, .. }
            | Op::SseMinMax { proved, .. }
            | Op::SseSqrt { proved, .. }
            | Op::SseConvert { proved, .. } => !proved,
            Op::X87Single(_, _, _, proved) => {
                proved & SINGLE_EXACT == 0
                    && proved & (SINGLE_A | SINGLE_B | SINGLE_RESULT)
                        != (SINGLE_A | SINGLE_B | SINGLE_RESULT)
            }
            Op::Fma { single, proved, .. } => {
                if single {
                    proved & SINGLE_EXACT == 0
                        && proved & (SINGLE_A | SINGLE_B | SINGLE_C | SINGLE_RESULT)
                            != (SINGLE_A | SINGLE_B | SINGLE_C | SINGLE_RESULT)
                } else {
                    proved & SINGLE_RESULT == 0
                }
            }
            Op::Fp32(..)
            | Op::SseFp64(..)
            | Op::ToInt { .. }
            | Op::SseScalarToInt { .. }
            | Op::X87Compare { .. }
            | Op::X87Store { proved: false, .. }
            | Op::Vec(VecOp::LessF64, ..) => true,
            Op::Flags { kind, .. } => kind.floating(),
            _ => false,
        }
    }

    /// Whether the operation can fail to the enclosing region start.
    pub(crate) fn can_fail(&self) -> bool {
        self.floating()
            || matches!(
                self,
                Op::Div { proved: false, .. }
                    | Op::FromInt(_)
                    | Op::PageCross { .. }
                    | Op::FrameDisjoint { .. }
                    | Op::Load { .. }
                    | Op::VecDeintLoad { .. }
                    | Op::VecLoadBcast { .. }
                    | Op::Store { .. }
                    | Op::X87Load { proved: false, .. }
                    | Op::SseInput { .. }
                    | Op::SsePackedToInt { .. }
                    | Op::FpValidate { .. }
                    | Op::DomainGuard { .. }
            )
    }

    pub(crate) fn region_guard(&self) -> Option<RecordId> {
        match *self {
            Op::PageCross { record, .. } | Op::FrameDisjoint { record, .. } => Some(record),
            _ => None,
        }
    }

    /// Whether the operation must stay although nothing uses its result.
    pub(crate) fn effect(&self) -> bool {
        self.can_fail()
            || matches!(
                self,
                Op::FrameStore { .. }
                    | Op::Validate
                    | Op::FpValidate { .. }
                    | Op::PlainPage { .. }
                    | Op::ScopeWritten(_)
            )
    }

    /// Whether the result depends only on the operands, so that equal
    /// operations may share one result or move.
    pub(crate) fn pure(&self) -> bool {
        !self.effect() && !matches!(self, Op::FrameLoad { .. })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub block: Block,
    pub args: Vec<Value>,
    /// Instructions the edge charges to the countdown, as the trace's edge
    /// net: a nonzero edge reaches a head, and a positive charge checks the
    /// countdown and may leave there through its complete record; a negative
    /// one refunds.
    pub charge: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum End {
    Unset,
    Jump(Target),
    Branch {
        cond: Value,
        taken: Target,
        fall: Target,
    },
    /// Leaves with the exact state the record gives (A64-OPT-6).
    Exit(RecordId),
}

impl End {
    pub(crate) fn targets(&self) -> impl Iterator<Item = &Target> {
        let list = match self {
            End::Jump(target) => [Some(target), None],
            End::Branch { taken, fall, .. } => [Some(taken), Some(fall)],
            End::Unset | End::Exit(_) => [None, None],
        };
        list.into_iter().flatten()
    }
}

/// A region start that arrivals reach with no unvalidated result: block 0,
/// loop heads, targets of edges that adjust the countdown, and merges of
/// arrivals from different regions. Code at a head comes only from its
/// arrivals' charges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Head {
    pub record: RecordId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockData {
    pub params: Vec<Value>,
    pub insts: Vec<(Option<Value>, Op)>,
    pub end: End,
    pub head: Option<Head>,
    /// Guest instructions translated by this block, in execution order.
    pub pcs: Vec<u32>,
}

/// Where a parameter of block 0 comes from when the job enters. Admission
/// stores each one in the entry parameter area the form loads from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// A general register, I32.
    Reg(u8),
    /// EFLAGS, as a `Flags` value.
    Eflags,
    /// An XMM register, V128.
    Xmm(u8),
    /// An MMX register, V128 with upper 64 bits zero.
    Mmx(u8),
    /// A valid x87 physical register as binary64, F64.
    X87(u8),
    /// The x87 last instruction pointer and opcode, I32.
    X87Ip,
    X87Opcode,
    FsBase,
    /// Dynamic guest MXCSR (used to pass runtime sticky bits without specialization), I32.
    Mxcsr,
    /// Frame bytes at entry ESP plus `offset`: I32 for 4 bytes, I64 for 8.
    /// Narrower accesses extract from these.
    Frame {
        offset: i32,
        width: u8,
    },
}

impl Source {
    pub(crate) fn ty(self) -> Type {
        match self {
            Source::Reg(_) | Source::X87Ip | Source::X87Opcode | Source::FsBase | Source::Mxcsr => {
                Type::I32
            }
            Source::Frame { width, .. } if width <= 4 => Type::I32,
            Source::Frame { .. } => Type::I64,
            Source::Eflags => Type::Flags,
            Source::Xmm(_) | Source::Mmx(_) => Type::V128,
            Source::X87(_) => Type::F64,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Func {
    pub blocks: Vec<BlockData>,
    pub types: Vec<Type>,
    pub records: Vec<IrRecord>,
    /// The sources of block 0's parameters, in order.
    pub entry: Vec<Source>,
    /// The head at trace index 0 when the form returns at its head: charged
    /// edges to it also check the head-exit word. Block 0 may be an entry
    /// prologue that jumps there.
    pub head_exit: Option<Block>,
    /// Registers (bits 0–7), XMM registers (bits 8–15) and MMX registers
    /// (bits 16–23) the form never writes: records leave them at their entry
    /// values.
    pub kept: u32,
    /// Charged heads in resident forms leave through complete records.
    pub resident: bool,
    /// Block parameters introduced solely to carry lazy EFLAGS recipes.
    pub flag_params: BTreeSet<Value>,
    /// Guest calls and returns translated into the SSA form.
    pub calls: Vec<u32>,
    /// Guest EIPs of resident checks whose operations produce a value.
    pub cold_pcs: BTreeMap<Value, u32>,
    /// Distinct `Block(0)` numerical domain guard IDs (separate from `plain_sites`).
    pub domain_guards: Vec<u32>,
    /// Marginal host instruction words saved per loop iteration, parallel to `domain_guards`.
    pub domain_saved_per_iter: Vec<u32>,
    /// `Op::Fp` results whose runtime validation is decoupled into explicit
    /// `Op::FpValidate` SSA ops before effect points (A64-OPT-4, A64-OPT-5).
    pub deferred_fp: BTreeSet<Value>,
}

impl Func {
    pub(crate) fn block(&mut self, head: Option<Head>) -> Block {
        self.blocks.push(BlockData {
            params: Vec::new(),
            insts: Vec::new(),
            end: End::Unset,
            head,
            pcs: Vec::new(),
        });
        Block(self.blocks.len() as u32 - 1)
    }

    pub(crate) fn param(&mut self, block: Block, ty: Type) -> Value {
        let value = self.value(ty);
        self.blocks[block.0 as usize].params.push(value);
        value
    }

    pub(crate) fn flag_param(&mut self, block: Block, ty: Type) -> Value {
        let value = self.param(block, ty);
        self.flag_params.insert(value);
        value
    }

    /// Appends `op` to `block`, returning its result, if any.
    pub(crate) fn push(&mut self, block: Block, op: Op) -> Option<Value> {
        let result = self.result(&op).map(|ty| self.value(ty));
        self.blocks[block.0 as usize].insts.push((result, op));
        result
    }

    pub(crate) fn end(&mut self, block: Block, end: End) {
        self.blocks[block.0 as usize].end = end;
    }

    pub(crate) fn record(&mut self, record: IrRecord) -> RecordId {
        self.records.push(record);
        RecordId(self.records.len() as u32 - 1)
    }

    pub(crate) fn ty(&self, value: Value) -> Type {
        self.types[value.0 as usize]
    }

    fn value(&mut self, ty: Type) -> Value {
        self.types.push(ty);
        Value(self.types.len() as u32 - 1)
    }

    pub(crate) fn result(&self, op: &Op) -> Option<Type> {
        Some(match *op {
            Op::Constant(ty, _)
            | Op::Bits { to: ty, .. }
            | Op::Load { ty, .. }
            | Op::FrameLoad { ty, .. } => ty,
            Op::Host(_) => Type::I64,
            Op::Int(_, a, _) | Op::Select { a, .. } => self.ty(a),
            Op::IntUnary(IntUnary::Low32 | IntUnary::High32, _) => Type::I32,
            Op::IntUnary(IntUnary::Widen | IntUnary::SignWiden, _) => Type::I64,
            Op::IntUnary(_, a) => self.ty(a),
            Op::Div { .. }
            | Op::Remainder { .. }
            | Op::Snapshot(_)
            | Op::Test { .. }
            | Op::ToInt { wide: false, .. }
            | Op::SseScalarToInt { .. }
            | Op::X87Store { width: 2 | 4, .. }
            | Op::X87StoreC1 { .. }
            | Op::X87Compare { .. }
            | Op::X87Exam(_)
            | Op::X87RoundC1 { .. }
            | Op::X87SqrtC1 { .. }
            | Op::VecToInt(..) => Type::I32,
            Op::ToInt { wide: true, .. } | Op::X87Store { .. } => Type::I64,
            Op::Flags { .. } => Type::Flags,
            Op::Fp(..)
            | Op::X87Single(..)
            | Op::X87Arith { .. }
            | Op::X87Sqrt { .. }
            | Op::X87Round { .. }
            | Op::Fma { .. }
            | Op::SseFp64(..)
            | Op::FpUnary(..)
            | Op::FromInt(_)
            | Op::X87Load { .. } => Type::F64,
            Op::Vec(..)
            | Op::VecUnary(..)
            | Op::VecFp { .. }
            | Op::VecFpMulElem { .. }
            | Op::VecDeintLoad { .. }
            | Op::VecLoadBcast { .. }
            | Op::VecFma { .. }
            | Op::VecFpToInt { .. }
            | Op::Fp32(..)
            | Op::SsePackedFp { .. }
            | Op::SseMinMax { .. }
            | Op::SseSqrt { .. }
            | Op::SseConvert { .. }
            | Op::SsePackedToInt { .. } => Type::V128,
            Op::Store { .. }
            | Op::FrameStore { .. }
            | Op::SseInput { .. }
            | Op::FpValidate { .. }
            | Op::Validate
            | Op::ScopeWritten(_)
            | Op::PlainPage { .. }
            | Op::DomainGuard { .. }
            | Op::PageCross { .. }
            | Op::FrameDisjoint { .. } => {
                return None;
            }
        })
    }
}
