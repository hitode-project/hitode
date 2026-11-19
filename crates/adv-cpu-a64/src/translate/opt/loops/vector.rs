//! Replace an independent integer data path with a vector of guest iterations.
//! Compute only the low bits observed by stores. All architectural state is
//! established by the original scalar iteration following the batch region.
//! A read shared by every lane still participates in the span guards.

use std::collections::BTreeMap;

use super::*;
use crate::encode::*;

pub(in crate::translate::opt) struct Vector;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Scalar(u8),
    Packed(u8),
}

impl Shape {
    fn lanes(self) -> u8 {
        match self {
            Self::Scalar(width) => 16 / width,
            Self::Packed(_) => 2,
        }
    }

    fn arr(self) -> Arr {
        match self {
            Self::Scalar(width) => Arr::new(width.trailing_zeros(), true),
            Self::Packed(_) => Arr::D2,
        }
    }

    fn repeat(self) -> bool {
        self == Self::Packed(16)
    }
}

#[derive(Clone, Copy)]
enum Unary {
    Not,
    Shift(ShiftImm, Arr, u32),
}

#[derive(Clone, Copy)]
enum Binary {
    Same(Same, Arr),
    Logic(Logic),
    UnsignedGt(Arr),
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
    Constant(u128),
    Unary(Unary),
    Binary(Binary),
    Blend,
    Charge(i32),
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
    Imm(u64),
    Charge(i32),
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
                    Step::Imm(bits) => emitter.immediate(SCALAR, bits),
                    Step::Charge(cost) => emitter.adjust_charge(SCALAR, cost),
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
        let width = loop_.streams.iter().map(|s| s.width).max()?;
        let mut shapes = vec![Shape::Scalar(1), Shape::Scalar(2), Shape::Scalar(4)];
        if width >= 8 {
            if !paths.selects.is_empty()
                || !matches!(width, 8 | 16)
                || groups
                    .iter()
                    .any(|g| g[0].1.width != width || width == 16 && g.len() != 1)
            {
                return None;
            }
            shapes = vec![Shape::Packed(width)];
        }
        for shape in shapes {
            if matches!(shape, Shape::Scalar(w) if w < width) {
                continue;
            }
            let mut builder = Builder {
                values: &values,
                shape,
                mapped: BTreeMap::new(),
                insts: Vec::new(),
                next: 0,
            };
            if let Some(code) = builder.build(&groups) {
                let inputs = builder
                    .insts
                    .iter()
                    .filter_map(|i| match i.op {
                        OpCode::Broadcast(v) => Some(v),
                        _ => None,
                    })
                    .collect();
                return Some(Plan {
                    name: "V",
                    lanes: shape.lanes(),
                    inputs,
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
            for &(result, op) in &data.insts {
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
                    | Op::Select { .. } => true,
                    Op::Bits { to, value } => to != Type::F64 && func.ty(value) != Type::F64,
                    Op::Vec(op, ..) => {
                        packed_binary(op, false).is_some() || packed_binary(op, true).is_some()
                    }
                    Op::VecUnary(VecUnary::Low64 | VecUnary::ShiftImm(_, 0..=2, _, _), _) => true,
                    _ => false,
                };
                if !allowed || result.is_some_and(|v| func.ty(v) == Type::F64) {
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
    insts: Vec<Inst>,
    next: usize,
}

impl Builder<'_> {
    fn build(&mut self, groups: &[Vec<(usize, Stream)>]) -> Option<Vec<Step>> {
        for group in groups {
            let stream = group[0].1;
            if stream.write() {
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
        // Emit the entire read set before stores. The alias guard permits
        // identical in-place streams only when their scalar load is first.
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
                let value = if matches!(self.shape, Shape::Scalar(_)) {
                    self.scalar(v, low_mask(s.width))?
                } else {
                    self.packed(v)?
                };
                inputs.push(value);
            }
            self.insts.push(Inst {
                op: OpCode::Store(Memory {
                    stream,
                    count: group.len(),
                }),
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
                OpCode::Unary(Unary::Shift(op, self.shape.arr(), bits)),
                vec![a],
            )
        }
    }

    fn scalar(&mut self, v: Value, mask: u32) -> Option<usize> {
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
        let arr = self.shape.arr();
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
            Op::Load { .. } => *self.mapped.get(&(v, u32::MAX))?,
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
        let Op::Test { flags, cond } = self.values.defs.op(test)? else {
            return None;
        };
        let Op::Flags {
            kind: FlagKind::Sub,
            width,
            mut a,
            mut b,
            ..
        } = self.values.defs.op(flags)?
        else {
            return None;
        };
        if matches!(cond, Cc::s | Cc::ns) {
            return None;
        }
        invert(cond)?;
        let signed = matches!(cond, Cc::l | Cc::le | Cc::g | Cc::ge);
        let arr = self.shape.arr();
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
        let right = self.scalar(b, compare_mask)?;
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
                    Shape::Packed(_) => code.push(Step::Vector(v, target())),
                },
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
                OpCode::Unary(op) => code.push(Step::Word(match op {
                    Unary::Not => vnot(true, target(), read(0)),
                    Unary::Shift(op, arr, n) => vshift(op, arr, target(), read(0), n),
                })),
                OpCode::Binary(op) => code.push(Step::Word(match op {
                    Binary::Same(op, arr) => vsame(op, arr, target(), read(0), read(1)),
                    Binary::Logic(op) => vlogic(op, true, target(), read(0), read(1)),
                    Binary::UnsignedGt(arr) => {
                        vsame(Same::Cmgt, arr, target(), read(0), read(1)) | 1 << 29
                    }
                })),
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
            let size = match s.width {
                1 => VMem::B,
                2 => VMem::H,
                4 => VMem::S,
                8 => VMem::D,
                16 => VMem::Q,
                _ => unreachable!(),
            };
            let dst = regs[0];
            code.push(Step::Address(s, vldr(size, dst, ADDRESS, 0)));
            if size != VMem::Q {
                code.push(Step::Word(vdup_lane(self.shape.arr(), dst, dst, 0)));
            }
            return;
        }
        let (work, bytes) = match self.shape {
            Shape::Scalar(w) => (w.trailing_zeros(), s.width * self.shape.lanes()),
            Shape::Packed(_) => (log, 16),
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
