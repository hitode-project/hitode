//! The optimizer's SSA IR. Blocks take parameters instead of phi nodes, and
//! block 0 is the loop head the job enters. A value is defined once, by a
//! block parameter or an instruction, and is used only where its definition
//! dominates. Frame bytes are values; memory outside the frame is reached
//! only through `Load` and `Store`. Guest addresses are 32-bit values that
//! emission offsets by the private alias base.

use iced_x86::ConditionCode;

use super::recovery::IrRecord;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Value(pub u32);

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// Rounds toward zero to an integral value (FRINTZ).
    RoundZero,
    /// Rounds toward minus infinity to an integral value (FRINTM).
    RoundDown,
}

/// SSE2 operations on 128-bit values. Shifts take their count from the low
/// 64 bits of the second operand; counts above 63 give zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VecOp {
    And,
    AndNot,
    Or,
    Xor,
    SubI32,
    ShlI64,
    ShrI64,
    /// `cmpltpd`: all ones in each lane where the first is less.
    LessF64,
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
    /// `ucomisd`: ZF, PF and CF from an unordered comparison.
    UnorderedCompare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Constant(Type, u128),
    Int(IntOp, Value, Value),
    IntUnary(IntUnary, Value),
    /// Divides the I64 `dividend` by the I32 `divisor`. Unless `proved`, a
    /// zero divisor or a quotient that does not fit fails.
    Div {
        signed: bool,
        remainder: bool,
        dividend: Value,
        divisor: Value,
        proved: bool,
    },
    Flags {
        kind: FlagKind,
        a: Value,
        b: Value,
        carry: Option<Value>,
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
    Fp(FpOp, Value, Value),
    FpUnary(FpUnary, Value),
    /// A signed integer as F64; fails when it is not exactly representable.
    FromInt(Value),
    /// F64 to a signed integer, truncating; out of range sets IOC.
    ToInt {
        value: Value,
        wide: bool,
    },
    Vec(VecOp, Value, Value),
    /// A load of `width` bytes outside the frame, zero-extended to `ty`.
    Load {
        ty: Type,
        width: u8,
        addr: Value,
    },
    /// A store outside the frame: an effect point. Region `after` starts at
    /// the guest boundary after it.
    Store {
        width: u8,
        addr: Value,
        value: Value,
        after: RecordId,
    },
    /// The x87 reading of binary64 bits. Fails on NaN, and on nonzero
    /// denormals when `denormal`.
    X87Load {
        bits: Value,
        denormal: bool,
    },
    /// Fails when IOC, DZC, OFC, UFC or IDC is set.
    Validate,
}

impl Op {
    pub(crate) fn uses(&self) -> impl Iterator<Item = Value> {
        let list: [Option<Value>; 3] = match *self {
            Op::Constant(..) | Op::Validate => [None; 3],
            Op::IntUnary(_, a)
            | Op::Bits { value: a, .. }
            | Op::FpUnary(_, a)
            | Op::FromInt(a)
            | Op::ToInt { value: a, .. }
            | Op::Load { addr: a, .. }
            | Op::X87Load { bits: a, .. }
            | Op::Test { flags: a, .. } => [Some(a), None, None],
            Op::Int(_, a, b)
            | Op::Fp(_, a, b)
            | Op::Vec(_, a, b)
            | Op::Div {
                dividend: a,
                divisor: b,
                ..
            }
            | Op::Store {
                addr: a, value: b, ..
            } => [Some(a), Some(b), None],
            Op::Flags { a, b, carry, .. } => [Some(a), Some(b), carry],
            Op::Select { cond, a, b } => [Some(cond), Some(a), Some(b)],
        };
        list.into_iter().flatten()
    }

    /// Whether the operation can set a flag that validation checks.
    pub(crate) fn floating(&self) -> bool {
        match *self {
            Op::Fp(..) | Op::ToInt { .. } | Op::Vec(VecOp::LessF64, ..) => true,
            Op::FpUnary(op, _) => matches!(op, FpUnary::RoundZero | FpUnary::RoundDown),
            Op::Flags { kind, .. } => kind == FlagKind::UnorderedCompare,
            _ => false,
        }
    }

    /// Whether the operation can fail to the enclosing region start.
    pub(crate) fn can_fail(&self) -> bool {
        matches!(
            self,
            Op::Div { proved: false, .. }
                | Op::FromInt(_)
                | Op::Load { .. }
                | Op::Store { .. }
                | Op::X87Load { .. }
                | Op::Validate
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub block: Block,
    pub args: Vec<Value>,
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

/// A loop head: an effect point where the countdown and the head-exit word
/// are checked, and a region start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Head {
    pub prepaid: u32,
    pub record: RecordId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockData {
    pub params: Vec<Value>,
    pub insts: Vec<(Option<Value>, Op)>,
    pub end: End,
    pub head: Option<Head>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Func {
    pub blocks: Vec<BlockData>,
    pub types: Vec<Type>,
    pub records: Vec<IrRecord>,
}

impl Func {
    pub(crate) fn block(&mut self, head: Option<Head>) -> Block {
        self.blocks.push(BlockData {
            params: Vec::new(),
            insts: Vec::new(),
            end: End::Unset,
            head,
        });
        Block(self.blocks.len() as u32 - 1)
    }

    pub(crate) fn param(&mut self, block: Block, ty: Type) -> Value {
        let value = self.value(ty);
        self.blocks[block.0 as usize].params.push(value);
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

    fn result(&self, op: &Op) -> Option<Type> {
        Some(match *op {
            Op::Constant(ty, _) | Op::Bits { to: ty, .. } | Op::Load { ty, .. } => ty,
            Op::Int(_, a, _) | Op::Select { a, .. } => self.ty(a),
            Op::IntUnary(IntUnary::Low32 | IntUnary::High32, _) => Type::I32,
            Op::IntUnary(IntUnary::Widen, _) => Type::I64,
            Op::IntUnary(_, a) => self.ty(a),
            Op::Div { .. } | Op::Test { .. } | Op::ToInt { wide: false, .. } => Type::I32,
            Op::ToInt { wide: true, .. } => Type::I64,
            Op::Flags { .. } => Type::Flags,
            Op::Fp(..) | Op::FpUnary(..) | Op::FromInt(_) | Op::X87Load { .. } => Type::F64,
            Op::Vec(..) => Type::V128,
            Op::Store { .. } | Op::Validate => return None,
        })
    }
}
