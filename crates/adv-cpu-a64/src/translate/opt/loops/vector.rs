//! Replace an independent integer data path with a vector of guest iterations.
//! Compute only the low bits observed by stores. All architectural state is
//! established by the original scalar iteration following the batch region.

use super::{
    Access, BatchBody, BatchEmitter, Cc, Defs, End, FlagKind, FmaOp, FpOp, FpUnary, Func, IntOp,
    IntUnary, Loop, LoopKernel, Op, Plan, SINGLE_A, SINGLE_B, SINGLE_C, SINGLE_EXACT,
    SINGLE_RESULT, Stream, Target, Type, Value, VecOp, VecUnary, invert,
};
use std::collections::{BTreeMap, BTreeSet};

use crate::encode::*;

mod conditional;
pub(in crate::translate::opt) use conditional::Path as ClassPath;

pub(in crate::translate::opt) struct Vector;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Scalar(u8),
    Packed(u8),
    /// 4-wide binary32 independent floating-point vector strip (`A64-TR-9`).
    Float32,
    /// 2-wide binary64 independent floating-point vector strip (`A64-TR-9`).
    Float64,
}

impl Shape {
    fn lanes(self) -> u8 {
        match self {
            Self::Scalar(width) => 16 / width,
            Self::Packed(_) | Self::Float64 => 2,
            Self::Float32 => 4,
        }
    }

    fn arr(self) -> Arr {
        match self {
            Self::Scalar(width) => Arr::new(width.trailing_zeros(), true),
            Self::Packed(_) | Self::Float64 => Arr::D2,
            Self::Float32 => Arr::S4,
        }
    }

    fn integers(self) -> Arr {
        if self == Self::Packed(8) {
            Arr::S4
        } else {
            self.arr()
        }
    }

    fn is_float(self) -> bool {
        matches!(self, Self::Float32 | Self::Float64)
    }

    fn repeat(self) -> bool {
        self == Self::Packed(16)
    }
}

fn single_proved(proved: u8) -> bool {
    const REQ: u8 = SINGLE_A | SINGLE_B | SINGLE_RESULT;
    proved & SINGLE_EXACT != 0 || proved & REQ == REQ
}

fn single_fma_proved(proved: u8) -> bool {
    const REQ: u8 = SINGLE_A | SINGLE_B | SINGLE_C | SINGLE_RESULT;
    proved & SINGLE_EXACT != 0 || proved & REQ == REQ
}

fn float_op(op: FpOp) -> Float {
    match op {
        FpOp::Add => Float::Fadd,
        FpOp::Sub => Float::Fsub,
        FpOp::Mul => Float::Fmul,
        FpOp::Div => Float::Fdiv,
    }
}

#[derive(Clone, Copy)]
enum Unary {
    Not,
    Shift(ShiftImm, Arr, u32),
    ShuffleWords(u8),
    FpNeg(bool),
    FpAbs(bool),
    FpToInt(bool, u8),
    IntToFp32,
    Widen2sTo2d,
}

#[derive(Clone, Copy)]
enum Binary {
    Same(Same, Arr),
    Logic(Logic),
    UnsignedGt(Arr),
    Unpack(Permute, u8),
    Pack(Narrowing, u8),
    Fp(FpOp, bool),
}

#[derive(Clone, Copy)]
struct Memory {
    stream: Stream,
    count: usize,
}

enum OpCode {
    Load(Memory),
    Store(Memory),
    Broadcast(Value),
    BroadcastExactF32(Value),
    Constant(u128),
    Unary(Unary),
    Binary(Binary),
    FpFma { sub: bool, double: bool },
    CheckNormal(bool),
    CheckMulUnderflow { double: bool, div: bool },
    CheckIntRange { double: bool, rounding: u8 },
    Blend,
    Charge(i32),
    ClassTest(u8),
    LaneStore(Stream, u32),
    Publish(Value, u32, Type),
    Update(Value, u32, u32),
    Debit(i32),
}

struct Inst {
    op: OpCode,
    inputs: Vec<usize>,
    outputs: Vec<usize>,
}

#[derive(Clone, Copy)]
enum Step {
    Word(u32),
    Address(Stream, u32),
    Gp(Value, VReg, Arr),
    Vector(Value, VReg),
    FpBroadcast(Value, VReg, bool, bool),
    BailCond(Cond),
    BailNonZero(Reg),
    BailNonZeroX(Reg),
    Imm(u64),
    Charge(i32),
    LaneStore(Stream, u32, VReg),
    Publish(Value, u32, Type, VReg),
    Update(Value, u32, u32),
    Debit(i32),
}

const ADDRESS: Reg = Reg::new(16);
const SCALAR: Reg = Reg::new(16);

struct Program {
    shape: Shape,
    code: Vec<Step>,
}

impl BatchBody for Program {
    fn lower(&self, emitter: &mut BatchEmitter<'_, '_>) {
        for iteration in 0..=u32::from(self.shape.repeat()) {
            for &step in &self.code {
                match step {
                    Step::Word(word) => emitter.asm().emit(word),
                    Step::Address(stream, word) => {
                        let extra = if self.shape.repeat() {
                            iteration
                        } else if stream.stride < 0 {
                            u32::from(self.shape.lanes() - 1)
                        } else {
                            0
                        };
                        let addr = stream
                            .addr
                            .shifted(extra.wrapping_mul(stream.stride as u32));
                        emitter.address(addr);
                        emitter.asm().emit(word);
                    }
                    Step::Gp(value, dst, arr) => {
                        let source = emitter.invariant(value, SCALAR);
                        emitter.asm().emit(vdup(arr, dst, source));
                    }
                    Step::Vector(value, dst) => {
                        let src = emitter.vector_invariant(value, dst);
                        if self.shape == Shape::Packed(8) {
                            emitter.asm().emit(vdup_lane(Arr::D2, dst, src, 0));
                        } else if src != dst {
                            emitter.asm().emit(vmov(true, dst, src));
                        }
                    }
                    Step::FpBroadcast(value, dst, double, check_exact) => {
                        let src = emitter.vector_invariant(value, dst);
                        if double {
                            emitter.asm().emit(vdup_lane(Arr::D2, dst, src, 0));
                        } else if check_exact {
                            let tmp = VReg::new(28);
                            let gp2 = Reg::new(17);
                            emitter.asm().emit_all(&[
                                unary(crate::encode::Unary::Narrow, tmp, src),
                                unary(crate::encode::Unary::Widen, VReg::new(29), tmp),
                                fmov_from_v(Size::X, SCALAR, src),
                                fmov_from_v(Size::X, gp2, VReg::new(29)),
                                cmp(Size::X, SCALAR, gp2, Shift::Lsl, 0),
                            ]);
                            let bail = emitter.bail();
                            emitter.asm().b_cond(Cond::Ne, bail);
                            emitter.asm().emit(vdup_lane(Arr::S4, dst, tmp, 0));
                        } else {
                            emitter.asm().emit_all(&[
                                unary(crate::encode::Unary::Narrow, dst, src),
                                vdup_lane(Arr::S4, dst, dst, 0),
                            ]);
                        }
                    }
                    Step::BailCond(cond) => {
                        let bail = emitter.bail();
                        emitter.asm().b_cond(cond, bail);
                    }
                    Step::BailNonZero(reg) => {
                        let bail = emitter.bail();
                        emitter.asm().cbz(Size::W, true, reg, bail);
                    }
                    Step::BailNonZeroX(reg) => {
                        let bail = emitter.bail();
                        emitter.asm().cbz(Size::X, true, reg, bail);
                    }
                    Step::Imm(bits) => emitter.immediate(SCALAR, bits),
                    Step::Charge(cost) => emitter.adjust_charge(SCALAR, cost),
                    Step::LaneStore(stream, lane, value) => {
                        emitter
                            .address(stream.addr.shifted(lane.wrapping_mul(stream.stride as u32)));
                        let physical = lane ^ u32::from(stream.stride < 0);
                        let data = Reg::new(17);
                        emitter.asm().emit(vumov(3, data, value, physical));
                        emitter.asm().emit(str(Size::X, data, ADDRESS, 0));
                    }
                    Step::Publish(value, lane, ty, from) => emitter.publish(value, lane, ty, from),
                    Step::Update(value, keep, set) => emitter.update(value, keep, set),
                    Step::Debit(cost) => emitter.debit(cost),
                }
            }
        }
    }
}

impl LoopKernel for Vector {
    fn recognize(&self, func: &Func, loop_: &Loop) -> Option<Plan> {
        let defs = Defs::new(func);
        if loop_.trip.again == Cc::e || !loop_.streams.iter().any(|s| s.write()) {
            return None;
        }
        let groups = loop_.groups();
        if func.resident
            && loop_.streams.iter().any(|s| s.write() && s.width == 8)
            && let Some(plan) = conditional::recognize(func, loop_, &defs, &groups)
        {
            return Some(plan);
        }
        for group in &groups {
            let s = group[0].1;
            if group.len() > 4
                || s.stride != 0
                    && s.stride.unsigned_abs() != group.len() as u32 * u32::from(s.width)
            {
                return None;
            }
        }

        let paths = Paths::new(func, loop_)?;
        let values = Values::new(func, loop_, &defs, &paths)?;
        let has_fp = loop_.blocks.iter().any(|&b| {
            func.blocks[b.0 as usize].insts.iter().any(|&(r, op)| {
                r.is_some_and(|v| func.ty(v) == Type::F64)
                    || matches!(op, Op::X87Store { .. } | Op::X87Load { .. })
            })
        });
        let width = loop_.streams.iter().map(|s| s.width).max()?;
        let shapes = if has_fp {
            if !paths.selects.is_empty()
                || loop_
                    .streams
                    .iter()
                    .any(|s| s.write() && !matches!(s.width, 4 | 8))
            {
                return None;
            }
            vec![Shape::Float32, Shape::Float64]
        } else if width >= 8 {
            if !paths.selects.is_empty()
                || !matches!(width, 8 | 16)
                || groups.iter().any(|g| {
                    g[0].1.stride != 0 && (g[0].1.width != width || width == 16 && g.len() != 1)
                })
            {
                return None;
            }
            vec![Shape::Packed(width)]
        } else {
            vec![Shape::Scalar(1), Shape::Scalar(2), Shape::Scalar(4)]
        };
        for shape in shapes {
            if matches!(shape, Shape::Scalar(w) if w < width) {
                continue;
            }
            let mut builder = Builder {
                values: &values,
                shape,
                mapped: BTreeMap::new(),
                checked: BTreeSet::new(),
                insts: Vec::new(),
                next: 0,
                bindings: BTreeMap::new(),
            };
            if let Some(code) = builder.build(&groups) {
                let inputs = builder
                    .insts
                    .iter()
                    .filter_map(|i| match i.op {
                        OpCode::Broadcast(v) | OpCode::BroadcastExactF32(v) => Some(v),
                        _ => None,
                    })
                    .collect();
                return Some(Plan {
                    name: "V",
                    lanes: shape.lanes(),
                    inputs,
                    conditional: false,
                    body: Box::new(Program { shape, code }),
                });
            }
        }
        None
    }
}

/// The original scalar iteration can reconstitute an overwritten value if
/// none of its new bits depends on bits changed by earlier iterations.
struct Values<'a> {
    func: &'a Func,
    defs: &'a Defs,
    loop_: &'a Loop,
    paths: &'a Paths,
    stable: BTreeMap<Value, u32>,
}

impl<'a> Values<'a> {
    fn new(func: &'a Func, loop_: &'a Loop, defs: &'a Defs, paths: &'a Paths) -> Option<Self> {
        let mut values = Self {
            func,
            defs,
            loop_,
            paths,
            stable: BTreeMap::new(),
        };
        for &(p, a) in &loop_.carried {
            let mask = if p == a {
                u32::MAX
            } else if func.ty(p) == Type::F64 {
                // Loop-carried floating-point recurrence: not independent across lanes.
                return None;
            } else if loop_.ivs.iter().any(|iv| iv.base == p) {
                continue;
            } else {
                values.retained(p, a)
            };
            values.stable.insert(p, mask);
        }

        for &b in &loop_.blocks {
            let data = &func.blocks[b.0 as usize];
            if data.pcs.iter().any(|pc| func.calls.contains(pc)) {
                return None;
            }
            for &(_, op) in &data.insts {
                let allowed = match op {
                    Op::Int(IntOp::Shl | IntOp::Shr | IntOp::Sar, _, b) => {
                        defs.constant(b).is_some()
                    }
                    Op::Int(
                        IntOp::Add | IntOp::Sub | IntOp::And | IntOp::Or | IntOp::Xor | IntOp::Mul,
                        ..,
                    )
                    | Op::IntUnary(
                        IntUnary::Not
                        | IntUnary::ZeroExtend8
                        | IntUnary::ZeroExtend16
                        | IntUnary::SignExtend8
                        | IntUnary::SignExtend16,
                        _,
                    )
                    | Op::Flags {
                        kind:
                            FlagKind::Add
                            | FlagKind::Sub
                            | FlagKind::Increment
                            | FlagKind::Decrement
                            | FlagKind::Logic
                            | FlagKind::Shl
                            | FlagKind::Shr
                            | FlagKind::Sar
                            | FlagKind::MulSigned,
                        ..
                    }
                    | Op::Load { .. }
                    | Op::Store { .. }
                    | Op::PageCross { .. }
                    | Op::FrameDisjoint { .. }
                    | Op::Validate
                    | Op::Constant(..)
                    | Op::Test { .. }
                    | Op::Select { .. }
                    | Op::X87Load { .. }
                    | Op::X87Single(..)
                    | Op::Fp(..)
                    | Op::Fma {
                        op: FmaOp::Add | FmaOp::RevSub,
                        ..
                    }
                    | Op::FpUnary(FpUnary::Neg | FpUnary::Abs, _)
                    | Op::FromInt(_)
                    | Op::X87Store { .. }
                    | Op::X87StoreC1 { .. } => true,
                    Op::Bits { to, value } => to != Type::F64 && func.ty(value) != Type::F64,
                    Op::Vec(op, ..) => {
                        packed_binary(op, false).is_some() || packed_binary(op, true).is_some()
                    }
                    Op::VecUnary(VecUnary::Low64 | VecUnary::ShiftImm(_, 0..=2, _, _), _) => true,
                    _ => false,
                };
                if !allowed {
                    return None;
                }
            }
        }
        let End::Branch { taken, fall, .. } = &func.blocks[loop_.latch.0 as usize].end else {
            unreachable!()
        };
        let exit = if loop_.latch_taken { fall } else { taken };
        let mut observed = loop_.carried.iter().map(|&(_, a)| a).collect::<Vec<_>>();
        // The latch's fall path may be a direct record or the remainder of
        // the form. Every edge argument on that path is a scalar result.
        observed.extend(&exit.args);
        let mut pending = vec![exit.block];
        let mut seen = vec![false; func.blocks.len()];
        while let Some(b) = pending.pop() {
            let i = b.0 as usize;
            if loop_.blocks.contains(&b) || std::mem::replace(&mut seen[i], true) {
                continue;
            }
            let data = &func.blocks[i];
            for (_, op) in &data.insts {
                observed.extend(op.uses());
            }
            if let Some(head) = data.head {
                observed.extend(func.records[head.record.0 as usize].uses());
            }
            match data.end {
                End::Exit(id) => observed.extend(func.records[id.0 as usize].uses()),
                End::Branch { cond, .. } => observed.push(cond),
                _ => {}
            }
            for edge in data.end.targets() {
                observed.extend(&edge.args);
                pending.push(edge.block);
            }
        }
        for v in observed {
            if !values.independent(v, u32::MAX) {
                return None;
            }
        }
        Some(values)
    }

    fn retained(&self, p: Value, a: Value) -> u32 {
        if p == a {
            return u32::MAX;
        }
        let Some(Op::Int(IntOp::Or, l, r)) = self.defs.op(a) else {
            return 0;
        };
        for (old, fresh) in [(l, r), (r, l)] {
            if let Some(Op::Int(IntOp::And, x, mask)) = self.defs.op(old)
                && let Some(mask) = self.defs.constant(mask)
            {
                let retained = self.retained(p, x) & mask as u32 & !self.may_set(fresh);
                if retained != 0 {
                    return retained;
                }
            }
        }
        0
    }

    fn may_set(&self, v: Value) -> u32 {
        match self.defs.op(v) {
            Some(Op::Constant(_, n)) => n as u32,
            Some(Op::Load { width: 1, .. } | Op::IntUnary(IntUnary::ZeroExtend8, _)) => 255,
            Some(Op::Load { width: 2, .. } | Op::IntUnary(IntUnary::ZeroExtend16, _)) => 65535,
            Some(Op::Int(IntOp::And, a, b)) => self.may_set(a) & self.may_set(b),
            Some(Op::Int(IntOp::Or | IntOp::Xor, a, b)) => self.may_set(a) | self.may_set(b),
            Some(Op::Int(op @ (IntOp::Shl | IntOp::Shr), a, b)) => {
                self.defs.constant(b).map_or(u32::MAX, |n| {
                    if op == IntOp::Shl {
                        self.may_set(a).wrapping_shl(n as u32)
                    } else {
                        self.may_set(a).wrapping_shr(n as u32)
                    }
                })
            }
            _ => u32::MAX,
        }
    }

    fn independent(&self, v: Value, bits: u32) -> bool {
        if bits == 0 || self.defs.invariant(&self.loop_.blocks, v) {
            return true;
        }
        if let Some(&stable) = self.stable.get(&v) {
            return bits & !stable == 0;
        }
        if self.loop_.ivs.iter().any(|iv| iv.base == v) {
            return true;
        }
        if let Some(&(cond, a, b)) = self.paths.selects.get(&v) {
            return self.independent(cond, u32::MAX)
                && self.independent(a, bits)
                && self.independent(b, bits);
        }
        match self.defs.op(v) {
            Some(Op::Load { .. }) => true,
            Some(Op::Int(IntOp::And, a, b)) => {
                self.independent(a, bits & self.may_set(b))
                    && self.independent(b, bits & self.may_set(a))
            }
            Some(Op::Int(IntOp::Or | IntOp::Xor, a, b)) => {
                self.independent(a, bits) && self.independent(b, bits)
            }
            Some(Op::Int(IntOp::Shl, a, b)) if self.defs.constant(b).is_some() => {
                self.independent(a, bits >> (self.defs.constant(b).unwrap() as u32 & 31))
            }
            Some(Op::Int(IntOp::Shr | IntOp::Sar, a, b)) if self.defs.constant(b).is_some() => {
                let n = self.defs.constant(b).unwrap() as u32 & 31;
                let high = if matches!(self.defs.op(v), Some(Op::Int(IntOp::Sar, ..)))
                    && bits.leading_zeros() < n
                {
                    1 << 31
                } else {
                    0
                };
                self.independent(a, (bits << n) | high)
            }
            Some(Op::IntUnary(IntUnary::ZeroExtend8 | IntUnary::SignExtend8, a)) => {
                self.independent(a, bits & 255 | (u32::from(bits > 255) * 128))
            }
            Some(Op::IntUnary(IntUnary::ZeroExtend16 | IntUnary::SignExtend16, a)) => {
                self.independent(a, bits & 65535 | (u32::from(bits > 65535) * 32768))
            }
            Some(op) => op.uses().all(|v| self.independent(v, u32::MAX)),
            None => false,
        }
    }
}

/// A pure branch diamond supplies SSA selects to the vector expression
/// builder. Its retirement is tracked independently of the selected values.
#[derive(Default)]
struct Paths {
    selects: BTreeMap<Value, (Value, Value, Value)>,
    fixed: i32,
    choices: Vec<(Value, i32)>,
}

impl Paths {
    fn new(func: &Func, loop_: &Loop) -> Option<Self> {
        let mut paths = Self::default();
        let mut block = loop_.head;
        while block != loop_.latch {
            match &func.blocks[block.0 as usize].end {
                End::Jump(t) => {
                    paths.fixed += t.charge;
                    block = t.block;
                }
                End::Branch { cond, taken, fall } => {
                    fn chain<'a>(func: &'a Func, mut edge: &'a Target) -> Vec<&'a Target> {
                        let mut result = Vec::new();
                        loop {
                            result.push(edge);
                            let End::Jump(next) = &func.blocks[edge.block.0 as usize].end else {
                                return result;
                            };
                            edge = next;
                        }
                    }
                    let left = chain(func, taken);
                    let right = chain(func, fall);
                    let (l, r) = left.iter().enumerate().find_map(|(l, a)| {
                        right
                            .iter()
                            .position(|b| b.block == a.block)
                            .map(|r| (l, r))
                    })?;
                    if left[..l].iter().chain(&right[..r]).any(|e| {
                        func.blocks[e.block.0 as usize]
                            .insts
                            .iter()
                            .any(|(_, op)| !op.pure())
                    }) {
                        return None;
                    }
                    block = left[l].block;
                    let params = &func.blocks[block.0 as usize].params;
                    if params.is_empty() {
                        return None;
                    }
                    for ((&p, &a), &b) in params.iter().zip(&left[l].args).zip(&right[r].args) {
                        paths.selects.insert(p, (*cond, a, b));
                    }
                    let taken = left[..=l].iter().map(|t| t.charge).sum::<i32>();
                    let fall = right[..=r].iter().map(|t| t.charge).sum::<i32>();
                    paths.fixed += fall;
                    if taken != fall {
                        paths.choices.push((*cond, taken - fall));
                    }
                }
                _ => return None,
            }
        }
        Some(paths)
    }
}

struct Comparison {
    operands: Option<(Value, Value)>,
    op: Binary,
    left: usize,
    right: usize,
    inverted: bool,
}

struct Builder<'a> {
    values: &'a Values<'a>,
    shape: Shape,
    mapped: BTreeMap<(Value, u32), usize>,
    checked: BTreeSet<usize>,
    insts: Vec<Inst>,
    next: usize,
    bindings: BTreeMap<Value, Value>,
}

impl Builder<'_> {
    fn resolve(&self, value: Value) -> Value {
        self.bindings.get(&value).copied().unwrap_or(value)
    }

    fn read_inputs(&mut self, groups: &[Vec<(usize, Stream)>]) -> Option<()> {
        for group in groups {
            let stream = group[0].1;
            if stream.write()
                || self.shape == Shape::Packed(8)
                    && group
                        .iter()
                        .all(|&(_, s)| self.values.loop_.covering_read(s).is_some())
            {
                continue;
            }
            self.memory_shape(stream, group.len())?;
            let memory = Memory {
                stream,
                count: group.len(),
            };
            let range = self.next..self.next + memory.count;
            self.next = range.end;
            for (&(_, s), id) in group.iter().zip(range.clone()) {
                let Access::Load(v) = s.access else {
                    unreachable!()
                };
                self.mapped.insert((v, u32::MAX), id);
            }
            self.insts.push(Inst {
                op: OpCode::Load(memory),
                inputs: vec![],
                outputs: range.collect(),
            });
        }
        Some(())
    }

    fn build(&mut self, groups: &[Vec<(usize, Stream)>]) -> Option<Vec<Step>> {
        self.read_inputs(groups)?;
        // Emit the entire read set and all computations + pre-store lane
        // validations (`A64-OPT-10`) before any store commits.
        let mut pending_stores = Vec::new();
        for group in groups {
            let stream = group[0].1;
            if !stream.write() {
                continue;
            }
            self.memory_shape(stream, group.len())?;
            let mut inputs = Vec::new();
            for &(_, s) in group {
                let Access::Store(v) = s.access else {
                    unreachable!()
                };
                let value = match self.shape {
                    Shape::Scalar(_) => self.scalar(v, low_mask(s.width))?,
                    Shape::Packed(_) => self.packed(v)?,
                    Shape::Float32 | Shape::Float64 => self.float_store(v, s.width)?,
                };
                inputs.push(value);
            }
            pending_stores.push((
                Memory {
                    stream,
                    count: group.len(),
                },
                inputs,
            ));
        }
        if self.shape.is_float() {
            // Every potentially-failing FP op in the loop body must be covered
            // by our validated data path so no exception can be skipped.
            for &b in &self.values.loop_.blocks {
                for &(res, op) in &self.values.func.blocks[b.0 as usize].insts {
                    if op.can_fail()
                        && !matches!(
                            op,
                            Op::Load { .. }
                                | Op::Store { .. }
                                | Op::PageCross { .. }
                                | Op::FrameDisjoint { .. }
                        )
                    {
                        let v = res?;
                        if !self.mapped.contains_key(&(v, u32::MAX)) {
                            return None;
                        }
                    }
                }
            }
        }
        for (memory, inputs) in pending_stores {
            self.insts.push(Inst {
                op: OpCode::Store(memory),
                inputs,
                outputs: vec![],
            });
        }
        if self.values.paths.fixed != 0 {
            self.insts.push(Inst {
                op: OpCode::Charge(self.values.paths.fixed),
                inputs: vec![],
                outputs: vec![],
            });
        }
        for &(test, charge) in &self.values.paths.choices {
            let cmp = self.compare(test)?;
            let mut mask = self.binary(cmp.op, cmp.left, cmp.right);
            if cmp.inverted {
                mask = self.emit(OpCode::Unary(Unary::Not), vec![mask]);
            }
            self.insts.push(Inst {
                op: OpCode::Charge(charge),
                inputs: vec![mask],
                outputs: vec![],
            });
        }
        self.allocate()
    }

    fn effect(&mut self, op: OpCode, inputs: Vec<usize>) {
        self.insts.push(Inst {
            op,
            inputs,
            outputs: Vec::new(),
        });
    }

    fn live_code(&mut self) -> Option<Vec<Step>> {
        let mut live = vec![false; self.next];
        self.insts = std::mem::take(&mut self.insts)
            .into_iter()
            .rev()
            .filter(|inst| {
                let needed = inst.outputs.is_empty() || inst.outputs.iter().any(|&v| live[v]);
                if needed {
                    for &v in &inst.inputs {
                        live[v] = true;
                    }
                }
                needed
            })
            .collect();
        self.insts.reverse();
        self.allocate()
    }

    fn memory_shape(&self, stream: Stream, count: usize) -> Option<()> {
        (count == 1 || u32::from(self.shape.lanes()) * u32::from(stream.width) >= 8).then_some(())
    }

    fn emit(&mut self, op: OpCode, inputs: Vec<usize>) -> usize {
        let id = self.next;
        self.next += 1;
        self.insts.push(Inst {
            op,
            inputs,
            outputs: vec![id],
        });
        id
    }

    fn constant(&mut self, n: u128) -> usize {
        self.emit(OpCode::Constant(n), vec![])
    }

    fn binary(&mut self, op: Binary, a: usize, b: usize) -> usize {
        self.emit(OpCode::Binary(op), vec![a, b])
    }

    fn shift(&mut self, op: ShiftImm, bits: u32, a: usize) -> usize {
        if bits == 0 {
            a
        } else {
            self.emit(
                OpCode::Unary(Unary::Shift(op, self.shape.integers(), bits)),
                vec![a],
            )
        }
    }

    fn scalar(&mut self, v: Value, mask: u32) -> Option<usize> {
        let v = self.resolve(v);
        if mask == 0 {
            return Some(self.constant(0));
        }
        if let Some(&id) = self.mapped.get(&(v, mask)) {
            return Some(id);
        }
        let id = self.scalar_op(v, mask)?;
        self.mapped.insert((v, mask), id);
        Some(id)
    }

    fn scalar_op(&mut self, v: Value, mask: u32) -> Option<usize> {
        let defs = self.values.defs;
        let arr = self.shape.integers();
        let bits = arr.bits();
        if let Some(n) = defs.constant(v) {
            return Some(self.constant(n));
        }
        if self.values.func.ty(v) != Type::I32 {
            return None;
        }
        if defs.invariant(&self.values.loop_.blocks, v)
            || self.values.stable.get(&v).is_some_and(|m| mask & !m == 0)
        {
            return Some(self.emit(OpCode::Broadcast(v), vec![]));
        }
        if let Some(&(cond, a, b)) = self.values.paths.selects.get(&v) {
            return self.select(cond, a, b, mask);
        }
        Some(match defs.op(v)? {
            Op::Load { .. } => {
                if let Some(&input) = self.mapped.get(&(v, u32::MAX)) {
                    return Some(input);
                }
                if self.shape != Shape::Packed(8) {
                    return None;
                }
                let narrow = self
                    .values
                    .loop_
                    .streams
                    .iter()
                    .find(|s| s.access == Access::Load(v))?;
                let (wide, shift) = self.values.loop_.covering_read(*narrow)?;
                let input = self.packed(wide)?;
                if shift == 0 {
                    input
                } else {
                    self.emit(
                        OpCode::Unary(Unary::Shift(ShiftImm::Ushr, Arr::D2, shift)),
                        vec![input],
                    )
                }
            }
            Op::Bits { value, .. } if self.shape == Shape::Packed(8) => self.packed(value)?,
            Op::Int(op, a, b) => match op {
                IntOp::And | IntOp::Or | IntOp::Xor => {
                    let (a, b, kind) = match op {
                        IntOp::And => (
                            self.scalar(a, mask & self.values.may_set(b))?,
                            self.scalar(b, mask & self.values.may_set(a))?,
                            Logic::And,
                        ),
                        IntOp::Or => (self.scalar(a, mask)?, self.scalar(b, mask)?, Logic::Orr),
                        _ => (self.scalar(a, mask)?, self.scalar(b, mask)?, Logic::Eor),
                    };
                    self.binary(Binary::Logic(kind), a, b)
                }
                IntOp::Add | IntOp::Sub | IntOp::Mul => {
                    let kind = match op {
                        IntOp::Add => Same::Add,
                        IntOp::Sub => Same::Sub,
                        _ => Same::Mul,
                    };
                    let demand = u32::MAX >> mask.leading_zeros();
                    let a = self.scalar(a, demand)?;
                    let b = self.scalar(b, demand)?;
                    self.binary(Binary::Same(kind, arr), a, b)
                }
                IntOp::Shl => {
                    let shift = defs.constant(b)? as u32 & 31;
                    if shift >= bits {
                        self.constant(0)
                    } else {
                        let a = self.scalar(a, mask >> shift)?;
                        self.shift(ShiftImm::Shl, shift, a)
                    }
                }
                IntOp::Shr | IntOp::Sar => {
                    let shift = defs.constant(b)? as u32 & 31;
                    let input = self.values.may_set(a);
                    let signed = op == IntOp::Sar && input >> 31 != 0;
                    if bits < 32 && ((input >> bits) != 0 || signed) {
                        return None;
                    }
                    let op = if signed {
                        ShiftImm::Sshr
                    } else {
                        ShiftImm::Ushr
                    };
                    let a = self.scalar(a, if signed { u32::MAX } else { mask << shift })?;
                    if shift >= bits {
                        self.constant(0)
                    } else {
                        self.shift(op, shift, a)
                    }
                }
                _ => return None,
            },
            Op::IntUnary(op, a) => match op {
                IntUnary::Not => {
                    let a = self.scalar(a, mask)?;
                    self.emit(OpCode::Unary(Unary::Not), vec![a])
                }
                IntUnary::ZeroExtend8
                | IntUnary::ZeroExtend16
                | IntUnary::SignExtend8
                | IntUnary::SignExtend16 => {
                    let width = if matches!(op, IntUnary::ZeroExtend8 | IntUnary::SignExtend8) {
                        1
                    } else {
                        2
                    };
                    let low = low_mask(width);
                    let signed = matches!(op, IntUnary::SignExtend8 | IntUnary::SignExtend16);
                    let a = self.scalar(
                        a,
                        mask & low
                            | if signed && mask & !low != 0 {
                                1 << (width * 8 - 1)
                            } else {
                                0
                            },
                    )?;
                    if mask & !low == 0 {
                        a
                    } else if signed {
                        let shift = bits - u32::from(width) * 8;
                        let a = self.shift(ShiftImm::Shl, shift, a);
                        self.shift(ShiftImm::Sshr, shift, a)
                    } else {
                        let b = self.constant(u128::from(low));
                        self.binary(Binary::Logic(Logic::And), a, b)
                    }
                }
                _ => return None,
            },
            Op::Select { cond, a, b } => self.select(cond, a, b, mask)?,
            _ => return None,
        })
    }

    fn compare(&mut self, test: Value) -> Option<Comparison> {
        let Op::Test { flags, cond } = self.values.defs.op(self.resolve(test))? else {
            return None;
        };
        let Op::Flags {
            kind: kind @ (FlagKind::Sub | FlagKind::Logic),
            width,
            mut a,
            mut b,
            ..
        } = self.values.defs.op(self.resolve(flags))?
        else {
            return None;
        };
        invert(cond)?;
        let signed = matches!(cond, Cc::l | Cc::le | Cc::g | Cc::ge);
        if kind == FlagKind::Logic
            && (self.shape != Shape::Packed(8) || !matches!(cond, Cc::e | Cc::ne))
        {
            return None;
        }
        let arr = self.shape.integers();
        if arr.bits() < u32::from(width) * 8 {
            let omitted = !low_mask((arr.bits() / 8) as u8);
            if signed || (self.values.may_set(a) | self.values.may_set(b)) & omitted != 0 {
                return None;
            }
        }
        if matches!(cond, Cc::l | Cc::ge | Cc::b | Cc::ae) {
            std::mem::swap(&mut a, &mut b);
        }
        let inverted = matches!(cond, Cc::ne | Cc::ge | Cc::le | Cc::ae | Cc::be);
        let compare_mask = low_mask(width);
        let left = self.scalar(a, compare_mask)?;
        let right = if kind == FlagKind::Logic {
            self.constant(0)
        } else {
            self.scalar(b, compare_mask)?
        };
        let shift = arr.bits().saturating_sub(u32::from(width) * 8);
        let left = self.shift(ShiftImm::Shl, shift, left);
        let right = self.shift(ShiftImm::Shl, shift, right);
        let op = if matches!(cond, Cc::e | Cc::ne) {
            Binary::Same(Same::Cmeq, arr)
        } else if signed {
            Binary::Same(Same::Cmgt, arr)
        } else {
            Binary::UnsignedGt(arr)
        };
        // Narrow guest compares shift their operands, so those vector
        // operands cannot also supply the value of a min/max.
        Some(Comparison {
            operands: (shift == 0).then_some((a, b)),
            op,
            left,
            right,
            inverted,
        })
    }

    fn select(&mut self, test: Value, mut t: Value, mut f: Value, mask: u32) -> Option<usize> {
        let Comparison {
            operands,
            op,
            left,
            right,
            inverted,
        } = self.compare(test)?;
        if inverted {
            std::mem::swap(&mut t, &mut f);
        }
        let arr = self.shape.arr();
        if let Some((a, b)) = operands
            && ((t, f) == (a, b) || (t, f) == (b, a))
        {
            let op = match (op, t == a) {
                (Binary::Same(Same::Cmgt, _), true) => Some(Same::Smax),
                (Binary::Same(Same::Cmgt, _), false) => Some(Same::Smin),
                (Binary::UnsignedGt(_), true) => Some(Same::Umax),
                (Binary::UnsignedGt(_), false) => Some(Same::Umin),
                _ => None,
            };
            if let Some(op) = op {
                return Some(self.binary(Binary::Same(op, arr), left, right));
            }
        }
        let cond = self.binary(op, left, right);
        let t = self.scalar(t, mask)?;
        let f = self.scalar(f, mask)?;
        Some(self.emit(OpCode::Blend, vec![cond, t, f]))
    }

    fn packed(&mut self, v: Value) -> Option<usize> {
        let v = self.resolve(v);
        if let Some(&id) = self.mapped.get(&(v, u32::MAX)) {
            return Some(id);
        }
        let full = self.shape == Shape::Packed(16);
        let defs = self.values.defs;
        let id = if let Some(bits) = defs.constant(v) {
            self.constant(bits)
        } else if self.values.func.ty(v) != Type::V128 {
            return None;
        } else if defs.invariant(&self.values.loop_.blocks, v)
            || self.values.stable.get(&v) == Some(&u32::MAX)
        {
            self.emit(OpCode::Broadcast(v), vec![])
        } else {
            match defs.op(v)? {
                Op::VecUnary(VecUnary::Low64, a) if !full => self.packed(a)?,
                Op::Bits { value, .. } if !full => {
                    let input = self.scalar(value, u32::MAX)?;
                    let low = self.constant(0xffff_ffff);
                    self.binary(Binary::Logic(Logic::And), input, low)
                }
                Op::VecUnary(VecUnary::Shuffle(1, imm, false), a) if !full => {
                    let a = self.packed(a)?;
                    self.emit(OpCode::Unary(Unary::ShuffleWords(imm)), vec![a])
                }
                Op::Load { .. } if !full => self.scalar(v, u32::MAX)?,
                Op::VecUnary(VecUnary::ShiftImm(op, log, n, q), a) if q == full && log <= 2 => {
                    let arr = Arr::new(u32::from(log), true);
                    let n = u32::from(n);
                    if n == 0 {
                        self.packed(a)?
                    } else if n >= arr.bits() && !matches!(op, ShiftImm::Sshr) {
                        self.constant(0)
                    } else {
                        let a = self.packed(a)?;
                        self.emit(
                            OpCode::Unary(Unary::Shift(op, arr, n.min(arr.bits()))),
                            vec![a],
                        )
                    }
                }
                Op::Vec(op, a, b) => {
                    let kind = packed_binary(op, full)?;
                    let mut a = self.packed(a)?;
                    let mut b = self.packed(b)?;
                    if matches!(op, VecOp::AndNot(_)) {
                        std::mem::swap(&mut a, &mut b);
                    }
                    self.binary(kind, a, b)
                }
                _ => return None,
            }
        };
        self.mapped.insert((v, u32::MAX), id);
        Some(id)
    }

    fn float_store(&mut self, v: Value, width: u8) -> Option<usize> {
        let v = self.resolve(v);
        if let Some(&id) = self.mapped.get(&(v, u32::MAX)) {
            return Some(id);
        }
        let Op::X87Store {
            value,
            width: sw,
            rounding,
            integer,
            proved,
        } = self.values.defs.op(v)?
        else {
            return None;
        };
        if sw != width {
            return None;
        }
        let double = self.shape == Shape::Float64;
        let id = if integer {
            if width != 4 {
                return None;
            }
            let src = self.float_val(value, !proved)?;
            if !proved {
                self.effect(OpCode::CheckIntRange { double, rounding }, vec![src]);
            }
            self.emit(OpCode::Unary(Unary::FpToInt(double, rounding)), vec![src])
        } else {
            let expected_width = if double { 8 } else { 4 };
            if width != expected_width {
                return None;
            }
            let src = self.float_val(value, !proved)?;
            if !proved {
                self.effect(OpCode::CheckNormal(double), vec![src]);
            }
            src
        };
        self.mapped.insert((v, u32::MAX), id);
        Some(id)
    }

    fn float_val(&mut self, v: Value, need_check: bool) -> Option<usize> {
        let v = self.resolve(v);
        let double = self.shape == Shape::Float64;
        if let Some(&id) = self.mapped.get(&(v, u32::MAX)) {
            if need_check && self.checked.insert(id) {
                self.effect(OpCode::CheckNormal(double), vec![id]);
            }
            return Some(id);
        }
        let defs = self.values.defs;
        if let Some(bits) = defs.constant(v) {
            let f = f64::from_bits(bits as u64);
            if !double && f64::from(f as f32).to_bits() != f.to_bits() {
                return None;
            }
            if need_check && (!f.is_finite() || (f != 0.0 && f.is_subnormal())) {
                return None;
            }
            let packed = if double {
                bits & u128::from(u64::MAX)
            } else {
                u128::from((f as f32).to_bits())
            };
            let id = self.constant(packed);
            self.mapped.insert((v, u32::MAX), id);
            return Some(id);
        }
        if defs.invariant(&self.values.loop_.blocks, v)
            || self.values.stable.get(&v) == Some(&u32::MAX)
        {
            let id = if !double && need_check {
                self.emit(OpCode::BroadcastExactF32(v), vec![])
            } else {
                self.emit(OpCode::Broadcast(v), vec![])
            };
            if need_check && self.checked.insert(id) {
                self.effect(OpCode::CheckNormal(double), vec![id]);
            }
            self.mapped.insert((v, u32::MAX), id);
            return Some(id);
        }
        let id = match defs.op(v)? {
            Op::X87Load {
                bits,
                single,
                proved,
                ..
            } => {
                if !double && !single {
                    return None;
                }
                let raw = *self.mapped.get(&(bits, u32::MAX))?;
                if double && single {
                    // 4-byte f32 stream loaded as 2×f32 into low 64 bits; only
                    // admit proved f32->f64 loads so subnormal f32 cannot hide.
                    if !proved {
                        return None;
                    }
                    self.emit(OpCode::Unary(Unary::Widen2sTo2d), vec![raw])
                } else {
                    if !proved {
                        self.effect(OpCode::CheckNormal(double), vec![raw]);
                    }
                    raw
                }
            }
            Op::FromInt(a) => {
                if double {
                    return None;
                }
                // Zero-extended u8/u16 stream loads are exact in binary32.
                let is_small_unsigned = match defs.op(a) {
                    Some(Op::Load { width: 1 | 2, .. }) => true,
                    Some(Op::IntUnary(IntUnary::ZeroExtend8 | IntUnary::ZeroExtend16, inner)) => {
                        self.mapped.contains_key(&(inner, u32::MAX))
                    }
                    _ => false,
                };
                if !is_small_unsigned {
                    return None;
                }
                let src_val = match defs.op(a) {
                    Some(Op::IntUnary(IntUnary::ZeroExtend8 | IntUnary::ZeroExtend16, inner)) => {
                        inner
                    }
                    _ => a,
                };
                let int_vec = *self.mapped.get(&(src_val, u32::MAX))?;
                self.emit(OpCode::Unary(Unary::IntToFp32), vec![int_vec])
            }
            Op::X87Single(op, a, b, proved) if !double => {
                let proved_all = single_proved(proved);
                let a_exact = proved & (SINGLE_A | SINGLE_EXACT) != 0;
                let b_exact = proved & (SINGLE_B | SINGLE_EXACT) != 0;
                let av = self.float_val(a, !a_exact)?;
                let bv = self.float_val(b, !b_exact)?;
                let res = self.binary(Binary::Fp(op, false), av, bv);
                if !proved_all {
                    self.effect(OpCode::CheckNormal(false), vec![res]);
                    if matches!(op, FpOp::Mul | FpOp::Div) {
                        self.effect(
                            OpCode::CheckMulUnderflow {
                                double: false,
                                div: op == FpOp::Div,
                            },
                            vec![res, av, bv],
                        );
                    }
                }
                res
            }
            Op::Fp(op, a, b, proved) if double => {
                let av = self.float_val(a, !proved)?;
                let bv = self.float_val(b, !proved)?;
                let res = self.binary(Binary::Fp(op, true), av, bv);
                if !proved {
                    self.effect(OpCode::CheckNormal(true), vec![res]);
                    if matches!(op, FpOp::Mul | FpOp::Div) {
                        self.effect(
                            OpCode::CheckMulUnderflow {
                                double: true,
                                div: op == FpOp::Div,
                            },
                            vec![res, av, bv],
                        );
                    }
                }
                res
            }
            Op::Fma {
                op: fma_op @ (FmaOp::Add | FmaOp::RevSub),
                a,
                b,
                c,
                single,
                proved,
            } if single == !double
                && if single {
                    single_fma_proved(proved)
                } else {
                    proved & SINGLE_RESULT != 0
                } =>
            {
                let av = self.float_val(a, false)?;
                let bv = self.float_val(b, false)?;
                let cv = self.float_val(c, false)?;
                self.emit(
                    OpCode::FpFma {
                        sub: fma_op == FmaOp::RevSub,
                        double,
                    },
                    vec![av, bv, cv],
                )
            }
            Op::FpUnary(op @ (FpUnary::Neg | FpUnary::Abs), a) => {
                let av = self.float_val(a, need_check)?;
                self.emit(
                    OpCode::Unary(if op == FpUnary::Neg {
                        Unary::FpNeg(double)
                    } else {
                        Unary::FpAbs(double)
                    }),
                    vec![av],
                )
            }
            _ => return None,
        };
        self.mapped.insert((v, u32::MAX), id);
        Some(id)
    }

    fn allocate(&self) -> Option<Vec<Step>> {
        let mut last = vec![0; self.next];
        for (i, inst) in self.insts.iter().enumerate() {
            for &v in inst.inputs.iter().chain(&inst.outputs) {
                last[v] = i;
            }
        }
        let mut owners = [None; 12];
        let mut locations = vec![VReg::new(0); self.next];
        let mut code = Vec::new();
        for (i, inst) in self.insts.iter().enumerate() {
            for owner in &mut owners {
                if owner.is_some_and(|v| last[v] <= i) {
                    *owner = None;
                }
            }
            for &v in &inst.outputs {
                let slot = owners.iter().position(Option::is_none)?;
                owners[slot] = Some(v);
                locations[v] = VReg::new(16 + slot as u8);
            }
            let target = || locations[inst.outputs[0]];
            let read = |n: usize| locations[inst.inputs[n]];
            match inst.op {
                OpCode::Load(memory) => {
                    let regs = inst
                        .outputs
                        .iter()
                        .map(|&v| locations[v])
                        .collect::<Vec<_>>();
                    self.memory(&mut code, memory, &regs);
                }
                OpCode::Store(memory) => {
                    let regs = inst
                        .inputs
                        .iter()
                        .map(|&v| locations[v])
                        .collect::<Vec<_>>();
                    self.memory(&mut code, memory, &regs);
                }
                OpCode::Broadcast(v) => match self.shape {
                    Shape::Scalar(_) => {
                        code.push(Step::Gp(v, target(), self.shape.arr()));
                    }
                    Shape::Packed(_) if self.values.func.ty(v) == Type::I32 => {
                        code.push(Step::Gp(v, target(), Arr::S4));
                    }
                    Shape::Packed(_) => code.push(Step::Vector(v, target())),
                    Shape::Float32 => code.push(Step::FpBroadcast(v, target(), false, false)),
                    Shape::Float64 => code.push(Step::FpBroadcast(v, target(), true, false)),
                },
                OpCode::BroadcastExactF32(v) => {
                    code.push(Step::FpBroadcast(v, target(), false, true));
                }
                OpCode::Constant(bits) => {
                    if bits == 0 {
                        code.push(Step::Word(vzero(true, target())));
                    } else {
                        code.push(Step::Imm(bits as u64));
                        code.push(Step::Word(vdup(self.shape.arr(), target(), SCALAR)));
                        if self.shape == Shape::Packed(16) {
                            let high = (bits >> 64) as u64;
                            if high != bits as u64 {
                                code.push(Step::Imm(high));
                                code.push(Step::Word(vins_general(3, target(), 1, SCALAR)));
                            }
                        }
                    }
                }
                OpCode::Unary(op) => match op {
                    Unary::Not => code.push(Step::Word(vnot(true, target(), read(0)))),
                    Unary::Shift(op, arr, n) => {
                        code.push(Step::Word(vshift(op, arr, target(), read(0), n)));
                    }
                    Unary::ShuffleWords(imm) => {
                        let mut index = 0u64;
                        for word in 0..4 {
                            let byte = u64::from(2 * (imm >> (2 * word) & 3));
                            index |= (byte | ((byte + 1) << 8)) << (16 * word);
                        }
                        let table = VReg::new(28);
                        code.push(Step::Imm(index));
                        code.push(Step::Word(fmov_to_v(Size::X, table, SCALAR)));
                        code.push(Step::Imm(index + 0x0808_0808_0808_0808));
                        code.push(Step::Word(vins_general(3, table, 1, SCALAR)));
                        code.push(Step::Word(
                            0x4e00_0000
                                | (table.number() << 16)
                                | (read(0).number() << 5)
                                | target().number(),
                        ));
                    }
                    Unary::FpNeg(double) | Unary::FpAbs(double) => {
                        let tmp = VReg::new(28);
                        let logic = if matches!(op, Unary::FpNeg(_)) {
                            Logic::Eor
                        } else {
                            Logic::Bic
                        };
                        if double {
                            code.push(Step::Word(movz(Size::X, SCALAR, 0x8000, 48)));
                            code.push(Step::Word(vdup(Arr::D2, tmp, SCALAR)));
                        } else {
                            code.push(Step::Word(movz(Size::W, SCALAR, 0x8000, 16)));
                            code.push(Step::Word(vdup(Arr::S4, tmp, SCALAR)));
                        }
                        code.push(Step::Word(vlogic(logic, true, target(), read(0), tmp)));
                    }
                    Unary::FpToInt(double, rounding) => {
                        code.push(Step::Word(vfcvt_int_mode(
                            double,
                            rounding,
                            target(),
                            read(0),
                        )));
                    }
                    Unary::IntToFp32 => {
                        code.push(Step::Word(vfromint(target(), read(0))));
                    }
                    Unary::Widen2sTo2d => {
                        code.push(Step::Word(vwiden(target(), read(0))));
                    }
                },
                OpCode::Binary(op) => {
                    let x = VReg::new(28);
                    let y = VReg::new(29);
                    let word = match op {
                        Binary::Same(op, arr) => vsame(op, arr, target(), read(0), read(1)),
                        Binary::Logic(op) => vlogic(op, true, target(), read(0), read(1)),
                        Binary::UnsignedGt(arr) => {
                            vsame(Same::Cmgt, arr, target(), read(0), read(1)) | 1 << 29
                        }
                        Binary::Unpack(op, log) => {
                            let arr = Arr::new(u32::from(log), true);
                            code.push(Step::Word(vpermute(
                                Permute::Zip1,
                                arr,
                                x,
                                read(0),
                                read(1),
                            )));
                            code.push(Step::Word(vpermute(
                                Permute::Zip2,
                                arr,
                                y,
                                read(0),
                                read(1),
                            )));
                            vpermute(op, Arr::D2, target(), x, y)
                        }
                        Binary::Pack(op, log) => {
                            let narrow = Arr::new(u32::from(log), false);
                            code.push(Step::Word(vnarrow(op, narrow, x, read(0))));
                            code.push(Step::Word(vnarrow(op, narrow, y, read(1))));
                            vpermute(Permute::Zip1, Arr::S4, target(), x, y)
                        }
                        Binary::Fp(fp, double) => {
                            if double {
                                vfloat_2d(float_op(fp), target(), read(0), read(1))
                            } else {
                                vfloat(float_op(fp), true, target(), read(0), read(1))
                            }
                        }
                    };
                    code.push(Step::Word(word));
                }
                OpCode::FpFma { sub, double } => {
                    let t = target();
                    let (ra, rb, rc) = (read(0), read(1), read(2));
                    if t == ra || t == rb {
                        let tmp = VReg::new(28);
                        code.push(Step::Word(vmov(true, tmp, rc)));
                        code.push(Step::Word(vfmla(double, sub, tmp, ra, rb)));
                        code.push(Step::Word(vmov(true, t, tmp)));
                    } else {
                        if t != rc {
                            code.push(Step::Word(vmov(true, t, rc)));
                        }
                        code.push(Step::Word(vfmla(double, sub, t, ra, rb)));
                    }
                }
                OpCode::CheckNormal(double) => {
                    let src = read(0);
                    let gp2 = Reg::new(17);
                    if !double {
                        // Normal (1..=254) or ±0.0 across all 4 f32 lanes.
                        let (t1, t2, t3) = (VReg::new(28), VReg::new(29), VReg::new(30));
                        code.push(Step::Word(vshift(ShiftImm::Shl, Arr::S4, t1, src, 1)));
                        code.push(Step::Word(vzero(true, t2)));
                        code.push(Step::Word(vsame(Same::Cmeq, Arr::S4, t2, t1, t2)));
                        code.push(Step::Word(movz(Size::W, SCALAR, 0x0100, 16)));
                        code.push(Step::Word(vdup(Arr::S4, t3, SCALAR)));
                        code.push(Step::Word(vsame(Same::Sub, Arr::S4, t1, t1, t3)));
                        code.push(Step::Word(vlogic(Logic::Bic, true, t1, t1, t2)));
                        code.push(Step::Word(umaxv(t1, t1)));
                        code.push(Step::Word(fmov_from_v(Size::W, SCALAR, t1)));
                        code.push(Step::Word(movz(Size::W, gp2, 0xfe00, 16)));
                        code.push(Step::Word(cmp(Size::W, SCALAR, gp2, Shift::Lsl, 0)));
                        code.push(Step::BailCond(Cond::Hs));
                    } else {
                        // Normal (1..=2046) or ±0.0 across both f64 lanes.
                        for lane in 0..2u32 {
                            code.push(Step::Word(vumov(3, SCALAR, src, lane)));
                            code.push(Step::Word(lsl_imm(Size::X, SCALAR, SCALAR, 1)));
                            code.push(Step::Word(lsr_imm(Size::X, gp2, SCALAR, 53)));
                            code.push(Step::Word(sub_imm(Size::W, gp2, gp2, 1, false)));
                            code.push(Step::Word(cmp_imm(Size::X, SCALAR, 0, false)));
                            code.push(Step::Word(csel(Size::W, gp2, Reg::ZR, gp2, Cond::Eq)));
                            code.push(Step::Word(cmp_imm(Size::W, gp2, 2046, false)));
                            code.push(Step::BailCond(Cond::Hs));
                        }
                    }
                }
                OpCode::CheckMulUnderflow { double, div } => {
                    // Bail if any lane underflowed to ±0.0 from nonzero input(s).
                    let (res, a, b) = (read(0), read(1), read(2));
                    let (t1, t2, t3) = (VReg::new(28), VReg::new(29), VReg::new(30));
                    let arr = if double { Arr::D2 } else { Arr::S4 };
                    code.push(Step::Word(vzero(true, t3)));
                    code.push(Step::Word(vshift(ShiftImm::Shl, arr, t1, res, 1)));
                    code.push(Step::Word(vsame(Same::Cmeq, arr, t1, t1, t3)));
                    code.push(Step::Word(vshift(ShiftImm::Shl, arr, t2, a, 1)));
                    code.push(Step::Word(vsame(Same::Cmeq, arr, t2, t2, t3)));
                    code.push(Step::Word(vlogic(Logic::Bic, true, t1, t1, t2)));
                    if !div {
                        code.push(Step::Word(vshift(ShiftImm::Shl, arr, t2, b, 1)));
                        code.push(Step::Word(vsame(Same::Cmeq, arr, t2, t2, t3)));
                        code.push(Step::Word(vlogic(Logic::Bic, true, t1, t1, t2)));
                    }
                    if double {
                        let gp2 = Reg::new(17);
                        code.push(Step::Word(vumov(3, SCALAR, t1, 0)));
                        code.push(Step::Word(vumov(3, gp2, t1, 1)));
                        code.push(Step::Word(orr(Size::X, SCALAR, SCALAR, gp2, Shift::Lsl, 0)));
                        code.push(Step::BailNonZeroX(SCALAR));
                    } else {
                        code.push(Step::Word(umaxv(t1, t1)));
                        code.push(Step::Word(fmov_from_v(Size::W, SCALAR, t1)));
                        code.push(Step::BailNonZero(SCALAR));
                    }
                }
                OpCode::CheckIntRange { double, rounding } => {
                    let src = read(0);
                    let gp2 = Reg::new(17);
                    if !double {
                        let tmp = VReg::new(28);
                        code.push(Step::Word(vshift(ShiftImm::Shl, Arr::S4, tmp, src, 1)));
                        code.push(Step::Word(umaxv(tmp, tmp)));
                        code.push(Step::Word(fmov_from_v(Size::W, SCALAR, tmp)));
                        code.push(Step::Word(movz(Size::W, gp2, 0x9e00, 16)));
                        code.push(Step::Word(cmp(Size::W, SCALAR, gp2, Shift::Lsl, 0)));
                        code.push(Step::BailCond(Cond::Hs));
                    } else {
                        let hi_imm = if rounding == 3 { 0x41e0 } else { 0x41df };
                        code.push(Step::Word(movz(Size::W, gp2, hi_imm, 16)));
                        for lane in 0..2u32 {
                            code.push(Step::Word(vumov(3, SCALAR, src, lane)));
                            code.push(Step::Word(ubfx(Size::X, SCALAR, SCALAR, 32, 31)));
                            code.push(Step::Word(cmp(Size::W, SCALAR, gp2, Shift::Lsl, 0)));
                            code.push(Step::BailCond(Cond::Hs));
                        }
                    }
                }
                OpCode::ClassTest(test) => {
                    let r = Reg::new(test);
                    let tmp = VReg::new(28);
                    code.push(Step::Word(vpermute(
                        Permute::Uzp1,
                        Arr::S4,
                        tmp,
                        read(0),
                        read(0),
                    )));
                    code.push(Step::Word(vumov(3, r, tmp, 0)));
                }
                OpCode::LaneStore(stream, lane) => {
                    code.push(Step::LaneStore(stream, lane, read(0)))
                }
                OpCode::Publish(v, lane, ty) => code.push(Step::Publish(v, lane, ty, read(0))),
                OpCode::Update(v, keep, set) => code.push(Step::Update(v, keep, set)),
                OpCode::Debit(cost) => code.push(Step::Debit(cost)),
                OpCode::Charge(cost) => {
                    if inst.inputs.is_empty() {
                        let lanes = if self.shape.repeat() {
                            1
                        } else {
                            self.shape.lanes()
                        };
                        code.push(Step::Imm(u64::from(lanes)));
                    } else {
                        let arr = self.shape.arr();
                        let tmp = VReg::new(28);
                        code.push(Step::Word(
                            0x4E31_B800
                                | (arr.log2() << 22)
                                | (read(0).number() << 5)
                                | tmp.number(),
                        ));
                        code.push(Step::Word(vumov(arr.log2(), SCALAR, tmp, 0)));
                        code.push(Step::Word(sbfx(Size::X, SCALAR, SCALAR, 0, arr.bits())));
                        code.push(Step::Word(sub(
                            Size::X,
                            SCALAR,
                            Reg::new(31),
                            SCALAR,
                            Shift::Lsl,
                            0,
                        )));
                    }
                    code.push(Step::Charge(cost));
                }
                OpCode::Blend => {
                    let tmp = VReg::new(28);
                    code.push(Step::Word(vmov(true, tmp, read(0))));
                    code.push(Step::Word(vlogic(Logic::Bsl, true, tmp, read(1), read(2))));
                    code.push(Step::Word(vmov(true, target(), tmp)));
                }
            }
        }
        Some(code)
    }

    fn memory(&self, code: &mut Vec<Step>, memory: Memory, regs: &[VReg]) {
        let s = memory.stream;
        let write = s.write();
        let mut log = s.width.trailing_zeros();
        if s.stride == 0 {
            let dst = regs[0];
            let size = match s.width {
                1 => VMem::B,
                2 => VMem::H,
                4 => VMem::S,
                8 => VMem::D,
                16 => VMem::Q,
                _ => unreachable!(),
            };
            code.push(Step::Address(s, vldr(size, dst, ADDRESS, 0)));
            if s.width < 16 {
                code.push(Step::Word(vdup_lane(self.shape.arr(), dst, dst, 0)));
            }
            return;
        }
        let (work, bytes) = match self.shape {
            Shape::Scalar(w) => (w.trailing_zeros(), s.width * self.shape.lanes()),
            Shape::Packed(_) => (log, 16),
            Shape::Float32 => (2, s.width * 4),
            Shape::Float64 => (if write { 3 } else { log }, s.width * 2),
        };
        let consecutive = regs
            .iter()
            .enumerate()
            .all(|(i, r)| r.number() == regs[0].number() + i as u32);
        let temporary = work != log && (write || memory.count != 1) || !consecutive;
        let first = if temporary { VReg::new(28) } else { regs[0] };
        let transfer = |first| {
            if memory.count == 1 {
                let size = match bytes {
                    4 => VMem::S,
                    8 => VMem::D,
                    16 => VMem::Q,
                    _ => unreachable!(),
                };
                if write {
                    vstr(size, first, ADDRESS, 0)
                } else {
                    vldr(size, first, ADDRESS, 0)
                }
            } else {
                let op = match memory.count {
                    2 => 0x8000,
                    3 => 0x4000,
                    4 => 0,
                    _ => unreachable!(),
                };
                0x0C00_0000
                    | (u32::from(bytes == 16) << 30)
                    | (u32::from(!write) << 22)
                    | op
                    | (log << 10)
                    | (ADDRESS.number() << 5)
                    | first.number()
            }
        };
        let instruction = transfer(first);
        if !write {
            code.push(Step::Address(s, instruction));
        }
        let elem = log;
        for (i, &target) in regs.iter().enumerate() {
            let mem = VReg::new(first.number() as u8 + i as u8);
            log = elem;
            if write {
                let mut src = target;
                for width in (log..work).rev() {
                    code.push(Step::Word(vnarrow(
                        Narrowing::Xtn,
                        Arr::new(width, false),
                        mem,
                        src,
                    )));
                    src = mem;
                }
                if src != mem {
                    code.push(Step::Word(vmov(true, mem, src)));
                }
            } else {
                let mut src = mem;
                while log < work {
                    // USHLL with shift zero, over the low half of the source.
                    let word =
                        0x2F00_A400 | ((8 << log) << 16) | (src.number() << 5) | target.number();
                    code.push(Step::Word(word));
                    src = target;
                    log += 1;
                }
                if src != target {
                    code.push(Step::Word(vmov(true, target, src)));
                }
            }
        }
        if write {
            code.push(Step::Address(s, instruction));
        }
    }
}

fn packed_binary(op: VecOp, full: bool) -> Option<Binary> {
    Some(match op {
        VecOp::Unpack(op @ (Permute::Zip1 | Permute::Zip2), log, false) if !full && log < 2 => {
            Binary::Unpack(op, log)
        }
        VecOp::Pack(op, log, false) if !full => Binary::Pack(op, log),
        VecOp::And(q) if q == full => Binary::Logic(Logic::And),
        VecOp::AndNot(q) if q == full => Binary::Logic(Logic::Bic),
        VecOp::Or(q) if q == full => Binary::Logic(Logic::Orr),
        VecOp::Xor(q) if q == full => Binary::Logic(Logic::Eor),
        VecOp::Same(op, log, q)
            if q == full && log <= 2 && !matches!(op, Same::Addp | Same::Sshl | Same::Ushl) =>
        {
            Binary::Same(op, Arr::new(u32::from(log), true))
        }
        _ => return None,
    })
}

fn low_mask(width: u8) -> u32 {
    u32::MAX >> (32 - u32::from(width) * 8)
}
