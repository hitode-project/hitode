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
    /// An I32 sign-extended to I64.
    SignWiden,
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

impl FlagKind {
    /// Whether the flags depend on operand `b`: a logical result's flags,
    /// and a step's or negation's, come from `a` alone.
    pub(crate) fn reads_b(self) -> bool {
        !matches!(
            self,
            FlagKind::Logic | FlagKind::Increment | FlagKind::Decrement | FlagKind::Negate
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Constant(Type, u128),
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
    /// `UnorderedCompare` compares F64 operands.
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
    /// exactly when `single`. Fails on NaN, and on nonzero denormals when
    /// `denormal` or `single`, since FZ would flush a binary32 denormal.
    X87Load {
        bits: Value,
        denormal: bool,
        single: bool,
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
            | Op::Host(a)
            | Op::FrameLoad { base: a, .. }
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
            }
            | Op::FrameStore {
                base: a, value: b, ..
            } => [Some(a), Some(b), None],
            Op::Flags { a, b, prior, .. } => [Some(a), Some(b), prior],
            Op::Select { cond, a, b }
            | Op::Remainder {
                quotient: cond,
                dividend: a,
                divisor: b,
            } => [Some(cond), Some(a), Some(b)],
        };
        list.into_iter().flatten()
    }

    /// Whether the operation can set a flag that validation checks.
    pub(crate) fn floating(&self) -> bool {
        match *self {
            Op::Fp(..) | Op::ToInt { .. } | Op::Vec(VecOp::LessF64, ..) => true,
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

    /// Whether the operation must stay although nothing uses its result.
    pub(crate) fn effect(&self) -> bool {
        self.can_fail() || self.floating() || matches!(self, Op::FrameStore { .. })
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
    /// The guest instruction the block translates, for profiles.
    pub eip: Option<u32>,
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
    /// A valid x87 physical register as binary64, F64.
    X87(u8),
    /// The x87 last instruction pointer and opcode, I32.
    X87Ip,
    X87Opcode,
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
            Source::Reg(_) | Source::X87Ip | Source::X87Opcode => Type::I32,
            Source::Frame { width, .. } if width <= 4 => Type::I32,
            Source::Frame { .. } => Type::I64,
            Source::Eflags => Type::Flags,
            Source::Xmm(_) => Type::V128,
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
    /// Registers (bits 0–7) and XMM registers (bits 8–15) the form never
    /// writes: records leave them at their entry values.
    pub kept: u16,
}

impl Func {
    pub(crate) fn block(&mut self, head: Option<Head>) -> Block {
        self.blocks.push(BlockData {
            params: Vec::new(),
            insts: Vec::new(),
            end: End::Unset,
            head,
            eip: None,
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
            | Op::Test { .. }
            | Op::ToInt { wide: false, .. } => Type::I32,
            Op::ToInt { wide: true, .. } => Type::I64,
            Op::Flags { .. } => Type::Flags,
            Op::Fp(..) | Op::FpUnary(..) | Op::FromInt(_) | Op::X87Load { .. } => Type::F64,
            Op::Vec(..) => Type::V128,
            Op::Store { .. } | Op::FrameStore { .. } | Op::Validate => return None,
        })
    }
}
