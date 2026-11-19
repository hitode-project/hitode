//! Replace an independent integer data path with a vector of guest iterations.
//! Compute only the low bits observed by stores. All architectural state is
//! established by the original scalar iteration following the batch region.
//! A read shared by every lane still participates in the span guards.
//! MMX values occupy one D lane per guest iteration. Integer operands use
//! the low two S lanes and cross into or out of that layout with a permute.

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

    fn scalar_arr(self) -> Arr {
        if self == Self::Packed(8) {
            Arr::S4
        } else {
            self.arr()
        }
    }
}

#[derive(Clone, Copy)]
enum Unary {
    Not,
    Shift(ShiftImm, Arr, u32),
    Column(u32),
    SplatWord(u8),
}

#[derive(Clone, Copy)]
enum Binary {
    Same(Same, Arr),
    Logic(Logic),
    UnsignedGt(Arr),
    Permute(Permute, Arr),
    Unpack(Permute, u8),
    Pack(Narrowing, u8),
    Table,
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
    Constant(u128, Arr, bool),
    Unary(Unary),
    Binary(Binary),
    Blend,
    Charge(i32),
    Mask(u8),
    LaneStore(Stream, u8),
    FixedCharge(i32),
    Publish(Value, u8),
    PublishBits(Value, u32, u32),
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
    LaneStore(Stream, u8, VReg),
    Mask(u8, VReg),
    FixedCharge(i32),
    Publish(Value, u8, VReg),
    PublishBits(Value, u32, u32),
}

const ADDRESS: Reg = Reg::new(16);
const SCALAR: Reg = Reg::new(16);

struct Program {
    shape: Shape,
    code: Vec<Step>,
}

impl BatchBody for Program {
    fn lower(&self, emitter: &mut BatchEmitter<'_, '_>) -> Option<ClassCosts> {
        self.lower_steps(emitter, |_, _| {});
        None
    }
}

impl Program {
    fn lower_steps(
        &self,
        emitter: &mut BatchEmitter<'_, '_>,
        mut mask: impl FnMut(&mut BatchEmitter<'_, '_>, u8),
    ) {
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
                    Step::Mask(test, src) => {
                        emitter.asm().emit(vumov(3, Reg::new(test), src, 0));
                        mask(emitter, test);
                    }
                    Step::LaneStore(s, lane, value) => {
                        let iteration = if s.stride < 0 { 1 - lane } else { lane };
                        let addr = s
                            .addr
                            .shifted(u32::from(iteration).wrapping_mul(s.stride as u32));
                        emitter.address(addr);
                        // ST1 {vt.d}[lane], [address].
                        emitter.asm().emit(
                            0x0D00_8400
                                | u32::from(lane) << 30
                                | ADDRESS.number() << 5
                                | value.number(),
                        );
                    }
                    Step::FixedCharge(cost) => emitter.fixed_charge(cost),
                    Step::Publish(value, lane, src) => emitter.publish_lane(value, lane, src),
                    Step::PublishBits(value, keep, set) => emitter.publish_bits(value, keep, set),
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
        let width = loop_.streams.iter().map(|s| s.width).max()?;
        let mmx = width == 8
            || func.resident
                && width == 4
                && loop_.blocks.iter().any(|b| {
                    func.blocks[b.0 as usize].insts.iter().any(
                |&(_, op)| matches!(op, Op::Vec(kind, ..) if packed_binary(kind, false).is_some()),
            )
                });
        let groups = loop_.groups();
        for group in &groups {
            let s = group[0].1;
            if group.len() > 4
                || s.stride != 0
                    && s.stride.unsigned_abs() != group.len() as u32 * u32::from(s.width)
                    && !(mmx && group.iter().all(|&(_, s)| covered_load(loop_, s).is_some()))
            {
                return None;
            }
        }

        let Some(paths) = Paths::new(func, loop_) else {
            return (func.resident && mmx)
                .then_some(())
                .and_then(|()| conditional(func, loop_, &defs, &groups));
        };
        let values = Values::new(func, loop_, &defs, &paths, &[])?;
        let mut shapes = vec![Shape::Scalar(1), Shape::Scalar(2), Shape::Scalar(4)];
        if mmx {
            if groups
                .iter()
                .any(|g| !matches!(g[0].1.width, 4 | 8) || !func.resident && g[0].1.width != width)
            {
                return None;
            }
            shapes = vec![Shape::Packed(8)];
        } else if width >= 8 {
            if !paths.selects.is_empty()
                || width != 16
                || groups.iter().any(|g| g[0].1.width != 16 || g.len() != 1)
            {
                return None;
            }
            shapes = vec![Shape::Packed(16)];
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
                    conditional_stores: false,
                    body: Box::new(Program { shape, code }),
                });
            }
        }
        None
    }
}

pub(super) fn covered_load(loop_: &Loop, narrow: Stream) -> Option<(Value, u32)> {
    if narrow.width != 4 || narrow.write() {
        return None;
    }
    loop_.streams.iter().find_map(|wide| {
        let Access::Load(v) = wide.access else {
            return None;
        };
        let column = narrow.addr.offset.wrapping_sub(wide.addr.offset);
        (wide.width == 8
            && wide.stride == narrow.stride
            && wide.addr.base == narrow.addr.base
            && matches!(column, 0 | 4))
        .then_some((v, column / 4))
    })
}

struct StoreArm {
    store: Option<Stream>,
    charge: i32,
    arguments: BTreeMap<Value, Value>,
    path: Vec<(Block, usize)>,
}

impl StoreArm {
    fn arrival(&mut self, func: &Func, target: &Target) {
        for (&param, &arg) in func.blocks[target.block.0 as usize]
            .params
            .iter()
            .zip(&target.args)
        {
            self.arguments.insert(param, self.value(arg));
        }
        self.charge += target.charge;
    }

    fn value(&self, value: Value) -> Value {
        self.arguments.get(&value).copied().unwrap_or(value)
    }
}

enum StoreTree {
    Arm(usize),
    Branch(u8, Box<Self>, Box<Self>),
}

impl StoreTree {
    fn collect(
        func: &Func,
        loop_: &Loop,
        defs: &Defs,
        mut block: Block,
        mut arm: StoreArm,
        tests: &mut Vec<Value>,
        arms: &mut Vec<StoreArm>,
    ) -> Option<Self> {
        loop {
            let data = &func.blocks[block.0 as usize];
            for &(_, op) in &data.insts {
                match op {
                    Op::Load { .. } if arm.store.is_some() => return None,
                    Op::Store {
                        value, addr, width, ..
                    } => {
                        if arm.store.is_some() || block == loop_.latch || width != 8 {
                            return None;
                        }
                        let affine = defs.affine(addr);
                        arm.store = loop_.streams.iter().copied().find(|s| {
                            s.addr == affine && s.width == width && s.access == Access::Store(value)
                        });
                        arm.store?;
                    }
                    _ => {}
                }
            }
            if block == loop_.latch {
                let leaf = arms.len();
                arms.push(arm);
                return Some(Self::Arm(leaf));
            }
            match &data.end {
                End::Jump(target) => {
                    arm.path.push((block, 0));
                    arm.arrival(func, target);
                    block = target.block;
                }
                End::Branch { cond, taken, fall } => {
                    if tests.len() == 3 || arm.store.is_some() {
                        return None;
                    }
                    let test = tests.len() as u8;
                    tests.push(*cond);
                    let follow =
                        |edge: usize, target: &Target, tests: &mut Vec<_>, arms: &mut Vec<_>| {
                            let mut next = StoreArm {
                                store: None,
                                charge: arm.charge,
                                arguments: arm.arguments.clone(),
                                path: arm.path.clone(),
                            };
                            next.path.push((block, edge));
                            next.arrival(func, target);
                            Self::collect(func, loop_, defs, target.block, next, tests, arms)
                        };
                    return Some(Self::Branch(
                        test,
                        Box::new(follow(0, taken, tests, arms)?),
                        Box::new(follow(1, fall, tests, arms)?),
                    ));
                }
                _ => return None,
            }
        }
    }

    fn dispatch(
        &self,
        emitter: &mut BatchEmitter<'_, '_>,
        lane: u8,
        targets: &[crate::asm::Label],
    ) {
        match self {
            Self::Arm(arm) => emitter.asm().b(targets[*arm]),
            Self::Branch(test, taken, fall) => {
                let label = emitter.asm().label();
                emitter
                    .asm()
                    .tbz(false, Reg::new(*test), u32::from(lane) * 32, label);
                taken.dispatch(emitter, lane, targets);
                emitter.asm().bind(label);
                fall.dispatch(emitter, lane, targets);
            }
        }
    }
}

struct ConditionalProgram {
    tree: StoreTree,
    tests: Program,
    cases: Vec<Vec<(Program, BatchClass)>>,
    early_skip: Option<(bool, usize)>,
    arms: Vec<Vec<(Block, usize)>>,
    depth: Vec<u32>,
}

impl BatchBody for ConditionalProgram {
    fn lower(&self, emitter: &mut BatchEmitter<'_, '_>) -> Option<ClassCosts> {
        let done = emitter.asm().label();
        let first = self
            .cases
            .iter()
            .map(|_| emitter.asm().label())
            .collect::<Vec<_>>();
        let cases = self
            .cases
            .iter()
            .map(|row| {
                row.iter()
                    .map(|_| emitter.asm().label())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let begin = emitter.asm().offset();
        let mut skip_words = 0;
        self.tests.lower_steps(emitter, |emitter, test| {
            if test == 0
                && let Some((taken, arm)) = self.early_skip
            {
                let dst = cases[arm][arm];
                if taken {
                    emitter
                        .asm()
                        .emit(adds_imm(Size::X, Reg::ZR, Reg::new(test), 1, false));
                    emitter.asm().b_cond(Cond::Eq, dst);
                } else {
                    emitter.asm().cbz(Size::X, false, Reg::new(test), dst);
                }
                skip_words = ((emitter.asm().offset() - begin) / 4) as u32;
            }
        });
        let test_words = ((emitter.asm().offset() - begin) / 4) as u32;
        self.tree.dispatch(emitter, 0, &first);
        for (&label, row) in first.iter().zip(&cases) {
            emitter.asm().bind(label);
            self.tree.dispatch(emitter, 1, row);
        }
        let mut batch = Vec::new();
        for (a, (labels, row)) in cases.into_iter().zip(&self.cases).enumerate() {
            for (b, (label, (body, class))) in labels.into_iter().zip(row).enumerate() {
                emitter.asm().bind(label);
                emitter.count_class(*class);
                let body_start = emitter.asm().offset();
                body.lower_steps(emitter, |_, _| {});
                emitter.asm().b(done);
                let prologue = if a == b && self.early_skip.is_some_and(|(_, arm)| arm == a) {
                    skip_words
                } else {
                    test_words + self.depth[a] + self.depth[b] + 2
                };
                batch.push(prologue + ((emitter.asm().offset() - body_start) / 4) as u32);
            }
        }
        emitter.asm().bind(done);
        Some(ClassCosts {
            scalar: self.arms.clone(),
            batch,
        })
    }
}

fn conditional(
    func: &Func,
    loop_: &Loop,
    defs: &Defs,
    groups: &[Vec<(usize, Stream)>],
) -> Option<Plan> {
    let mut tests = Vec::new();
    let mut arms = Vec::new();
    let tree = StoreTree::collect(
        func,
        loop_,
        defs,
        loop_.head,
        StoreArm {
            store: None,
            charge: 0,
            arguments: BTreeMap::new(),
            path: Vec::new(),
        },
        &mut tests,
        &mut arms,
    )?;
    let StoreTree::Branch(_, taken, fall) = &tree else {
        return None;
    };
    let target = arms.iter().find_map(|a| a.store)?;
    if arms
        .iter()
        .filter_map(|a| a.store)
        .any(|s| s.addr != target.addr || s.stride != target.stride)
        || groups.iter().any(|g| !matches!(g[0].1.width, 4 | 8))
    {
        return None;
    }
    let early_skip = [(true, taken), (false, fall)]
        .into_iter()
        .find_map(|(takes, node)| {
            let StoreTree::Arm(i) = **node else {
                return None;
            };
            arms[i].store.is_none().then_some((takes, i))
        });
    let paths = Paths {
        conditional: true,
        ..Paths::default()
    };
    let values = Values::new(func, loop_, defs, &paths, &arms)?;
    let shape = Shape::Packed(8);
    let mut inputs = Vec::new();
    let mut build = |body: &mut dyn FnMut(&mut Builder<'_>) -> Option<()>| {
        let mut b = Builder {
            values: &values,
            shape,
            mapped: BTreeMap::new(),
            insts: Vec::new(),
            next: 0,
        };
        b.reads(groups)?;
        body(&mut b)?;
        b.prune_unused();
        inputs.extend(b.insts.iter().filter_map(|i| match i.op {
            OpCode::Broadcast(v) => Some(v),
            _ => None,
        }));
        Some(Program {
            shape,
            code: b.allocate()?,
        })
    };
    let tests = build(&mut |b| {
        for (i, &test) in tests.iter().enumerate() {
            let mask = b.mask(test)?;
            b.insts.push(Inst {
                op: OpCode::Mask(i as u8),
                inputs: vec![mask],
                outputs: vec![],
            });
        }
        Some(())
    })?;
    let mut cases = Vec::new();
    for (first, a) in arms.iter().enumerate() {
        let mut row = Vec::new();
        for (second, b) in arms.iter().enumerate() {
            let class = if first != second {
                BatchClass::Mixed
            } else if a.store.is_some() {
                BatchClass::Uniform
            } else {
                BatchClass::Skip
            };
            let body = build(&mut |builder| {
                if first == second {
                    if let Some(s) = a.store {
                        let value = builder.store_value(s)?;
                        builder.store(
                            OpCode::Store(Memory {
                                stream: s,
                                count: 1,
                            }),
                            value,
                        );
                    }
                } else {
                    let mut stores = Vec::new();
                    for (lane, arm) in [(0, a), (1, b)] {
                        if let Some(s) = arm.store {
                            stores.push((s, lane, builder.store_value(s)?));
                        }
                    }
                    if target.stride < 0 {
                        stores.reverse();
                    }
                    for (s, lane, value) in stores {
                        builder.store(OpCode::LaneStore(s, lane), value);
                    }
                }
                let charge = a.charge + b.charge;
                if charge != 0 {
                    builder.insts.push(Inst {
                        op: OpCode::FixedCharge(charge),
                        inputs: vec![],
                        outputs: vec![],
                    });
                }
                for &(param, returning, bits) in &values.published {
                    let order = if target.stride < 0 {
                        [(1, b), (0, a)]
                    } else {
                        [(0, a), (1, b)]
                    };
                    if bits {
                        let [(keep0, set0), (keep1, set1)] = order.map(|(_, arm)| {
                            bit_transform(defs, param, arm.value(returning)).unwrap()
                        });
                        let (keep, set) = (keep0 & keep1, (set0 & keep1) | set1);
                        if keep != u32::MAX || set != 0 {
                            builder.insts.push(Inst {
                                op: OpCode::PublishBits(param, keep, set),
                                inputs: vec![],
                                outputs: vec![],
                            });
                        }
                    } else if let Some((lane, value)) =
                        order.into_iter().rev().find_map(|(lane, arm)| {
                            let value = arm.value(returning);
                            (value != param).then_some((lane, value))
                        })
                    {
                        let value = if func.ty(param) == Type::I32 {
                            builder.scalar(value, u32::MAX)?
                        } else {
                            builder.packed(value)?
                        };
                        builder.store(OpCode::Publish(param, lane), value);
                    }
                }
                Some(())
            })?;
            row.push((body, class));
        }
        cases.push(row);
    }
    Some(Plan {
        name: "V",
        lanes: 2,
        inputs,
        conditional_stores: true,
        body: Box::new(ConditionalProgram {
            tree,
            tests,
            cases,
            early_skip,
            depth: arms
                .iter()
                .map(|arm| {
                    arm.path
                        .iter()
                        .filter(|&&(b, _)| {
                            matches!(func.blocks[b.0 as usize].end, End::Branch { .. })
                        })
                        .count() as u32
                })
                .collect(),
            arms: arms
                .into_iter()
                .map(|mut arm| {
                    arm.path
                        .push((loop_.latch, usize::from(!loop_.latch_taken)));
                    arm.path
                })
                .collect(),
        }),
    })
}

/// The original scalar iteration can reconstitute an overwritten value if
/// none of its new bits depends on bits changed by earlier iterations.
struct Values<'a> {
    func: &'a Func,
    defs: &'a Defs,
    loop_: &'a Loop,
    paths: &'a Paths,
    stable: BTreeMap<Value, u32>,
    published: Vec<(Value, Value, bool)>,
}

impl<'a> Values<'a> {
    fn new(
        func: &'a Func,
        loop_: &'a Loop,
        defs: &'a Defs,
        paths: &'a Paths,
        arms: &[StoreArm],
    ) -> Option<Self> {
        let mut values = Self {
            func,
            defs,
            loop_,
            paths,
            stable: BTreeMap::new(),
            published: Vec::new(),
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
                    Op::Vec(VecOp::Pack(..) | VecOp::Unpack(..), ..) if !func.resident => false,
                    Op::Vec(op, ..) => {
                        packed_binary(op, false).is_some() || packed_binary(op, true).is_some()
                    }
                    Op::VecUnary(VecUnary::Low64 | VecUnary::ShiftImm(_, 0..=2, _, _), _) => true,
                    Op::VecUnary(VecUnary::Shuffle(1, _, false), _) => func.resident,
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
        if !arms.is_empty() {
            for &(param, returning) in &loop_.carried {
                let ty = func.ty(param);
                if values.independent(returning, u32::MAX) || !matches!(ty, Type::I32 | Type::V128)
                {
                    continue;
                }
                let bits = ty == Type::I32
                    && arms
                        .iter()
                        .all(|arm| bit_transform(defs, param, arm.value(returning)).is_some());
                if bits
                    || arms.iter().all(|arm| {
                        let value = arm.value(returning);
                        value == param || values.independent(value, u32::MAX)
                    })
                {
                    values.published.push((param, returning, bits));
                }
            }
        }
        // The scalar tail sees the exact carried values published by a preceding batch.
        let stable = values.stable.clone();
        for &(param, _, _) in &values.published {
            values.stable.insert(param, u32::MAX);
        }
        for v in observed {
            if !values.independent(v, u32::MAX) {
                return None;
            }
        }
        values.stable = stable;
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
            None if self.paths.conditional
                && self.defs.blocks[v.0 as usize] != Some(self.loop_.head) =>
            {
                let mut incoming = self
                    .func
                    .blocks
                    .iter()
                    .flat_map(|b| b.end.targets())
                    .filter_map(|edge| {
                        let block = &self.func.blocks[edge.block.0 as usize];
                        block
                            .params
                            .iter()
                            .position(|&p| p == v)
                            .map(|i| edge.args[i])
                    })
                    .peekable();
                incoming.peek().is_some() && incoming.all(|arg| self.independent(arg, bits))
            }
            None => false,
        }
    }
}

fn bit_transform(defs: &Defs, param: Value, v: Value) -> Option<(u32, u32)> {
    if v == param {
        return Some((u32::MAX, 0));
    }
    if let Some(bits) = defs.constant(v) {
        return Some((0, bits as u32));
    }
    let Op::Int(kind @ (IntOp::And | IntOp::Or), a, b) = defs.op(v)? else {
        return None;
    };
    let (input, mask) = if let Some(mask) = defs.constant(b) {
        (a, mask as u32)
    } else {
        (b, defs.constant(a)? as u32)
    };
    let (keep, set) = bit_transform(defs, param, input)?;
    Some(if kind == IntOp::And {
        (keep & mask, set & mask)
    } else {
        (keep, set | mask)
    })
}

/// A pure branch diamond supplies SSA selects to the vector expression
/// builder. Its retirement is tracked independently of the selected values.
#[derive(Default)]
struct Paths {
    selects: BTreeMap<Value, (Value, Value, Value)>,
    fixed: i32,
    choices: Vec<(Value, i32)>,
    conditional: bool,
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
    fn reads(&mut self, groups: &[Vec<(usize, Stream)>]) -> Option<()> {
        let mut reads = groups
            .iter()
            .filter(|g| !g[0].1.write())
            .collect::<Vec<_>>();
        let mmx = self.shape == Shape::Packed(8);
        if mmx {
            reads.sort_by_key(|g| std::cmp::Reverse(g[0].1.width));
        }
        for group in reads {
            let stream = group[0].1;
            if mmx
                && let Some(covered) = group
                    .iter()
                    .map(|&(_, s)| covered_load(self.values.loop_, s))
                    .collect::<Option<Vec<_>>>()
            {
                for (&(_, s), (wide, column)) in group.iter().zip(covered) {
                    let Access::Load(v) = s.access else {
                        unreachable!()
                    };
                    let packed = self.packed(wide)?;
                    let id = if self.values.func.ty(v) == Type::V128 && column == 0 {
                        self.low_dwords(packed)
                    } else {
                        let scalar = self.emit(OpCode::Unary(Unary::Column(column)), vec![packed]);
                        if self.values.func.ty(v) == Type::V128 {
                            self.insert_dwords(scalar)
                        } else {
                            scalar
                        }
                    };
                    self.mapped.insert((v, u32::MAX), id);
                }
                continue;
            }
            self.memory_shape(stream, group.len())?;
            let memory = Memory {
                stream,
                count: group.len(),
            };
            let range = self.next..self.next + memory.count;
            self.next = range.end;
            self.insts.push(Inst {
                op: OpCode::Load(memory),
                inputs: vec![],
                outputs: range.clone().collect(),
            });
            for (&(_, s), id) in group.iter().zip(range) {
                let Access::Load(v) = s.access else {
                    unreachable!()
                };
                let id = if mmx && s.width == 4 && self.values.func.ty(v) == Type::V128 {
                    self.insert_dwords(id)
                } else {
                    id
                };
                self.mapped.insert((v, u32::MAX), id);
            }
        }
        Some(())
    }

    fn build(&mut self, groups: &[Vec<(usize, Stream)>]) -> Option<Vec<Step>> {
        self.reads(groups)?;
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
                inputs.push(self.store_value(s)?);
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

    fn store_value(&mut self, s: Stream) -> Option<usize> {
        let Access::Store(v) = s.access else {
            unreachable!()
        };
        let mmx = self.shape == Shape::Packed(8);
        if matches!(self.shape, Shape::Scalar(_)) || mmx && self.values.func.ty(v) == Type::I32 {
            if s.width > 4 {
                return None;
            }
            self.scalar(v, low_mask(s.width))
        } else {
            let packed = self.packed(v)?;
            Some(if mmx && s.width == 4 {
                self.emit(OpCode::Unary(Unary::Column(0)), vec![packed])
            } else {
                packed
            })
        }
    }

    fn store(&mut self, op: OpCode, value: usize) {
        self.insts.push(Inst {
            op,
            inputs: vec![value],
            outputs: vec![],
        });
    }

    fn prune_unused(&mut self) {
        let mut live = vec![false; self.next];
        let mut kept = Vec::new();
        for inst in self.insts.drain(..).rev() {
            if inst.outputs.is_empty() || inst.outputs.iter().any(|&v| live[v]) {
                for &v in &inst.inputs {
                    live[v] = true;
                }
                kept.push(inst);
            }
        }
        kept.reverse();
        self.insts = kept;
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
        self.emit(
            OpCode::Constant(n, self.shape.arr(), self.shape.repeat()),
            vec![],
        )
    }

    fn scalar_constant(&mut self, n: u128) -> usize {
        self.emit(OpCode::Constant(n, self.shape.scalar_arr(), false), vec![])
    }

    fn insert_dwords(&mut self, input: usize) -> usize {
        let zero = self.constant(0);
        self.binary(Binary::Permute(Permute::Zip1, Arr::S4), input, zero)
    }

    fn low_dwords(&mut self, input: usize) -> usize {
        let mask = self.constant(u128::from(u32::MAX));
        self.binary(Binary::Logic(Logic::And), input, mask)
    }

    fn binary(&mut self, op: Binary, a: usize, b: usize) -> usize {
        self.emit(OpCode::Binary(op), vec![a, b])
    }

    fn shift(&mut self, op: ShiftImm, bits: u32, a: usize) -> usize {
        if bits == 0 {
            a
        } else {
            self.emit(
                OpCode::Unary(Unary::Shift(op, self.shape.scalar_arr(), bits)),
                vec![a],
            )
        }
    }

    fn scalar(&mut self, v: Value, mask: u32) -> Option<usize> {
        if mask == 0 {
            return Some(self.scalar_constant(0));
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
        let arr = self.shape.scalar_arr();
        let bits = arr.bits();
        if let Some(n) = defs.constant(v) {
            return Some(self.scalar_constant(n));
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
            Op::Bits { value, .. } if self.shape == Shape::Packed(8) => {
                let a = self.packed(value)?;
                self.emit(OpCode::Unary(Unary::Column(0)), vec![a])
            }
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
                        self.scalar_constant(0)
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
                        self.scalar_constant(0)
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
                        let b = self.scalar_constant(u128::from(low));
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
            kind,
            width,
            mut a,
            mut b,
            ..
        } = self.values.defs.op(flags)?
        else {
            return None;
        };
        if self.values.paths.conditional
            && kind == FlagKind::Logic
            && width == 4
            && matches!(cond, Cc::e | Cc::ne)
        {
            let left = self.scalar(a, low_mask(width))?;
            let right = self.scalar_constant(0);
            return Some(Comparison {
                operands: None,
                op: Binary::Same(Same::Cmeq, self.shape.scalar_arr()),
                left,
                right,
                inverted: cond == Cc::ne,
            });
        }
        if kind != FlagKind::Sub {
            return None;
        }
        if matches!(cond, Cc::s | Cc::ns) {
            return None;
        }
        invert(cond)?;
        let signed = matches!(cond, Cc::l | Cc::le | Cc::g | Cc::ge);
        let arr = self.shape.scalar_arr();
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

    fn mask(&mut self, test: Value) -> Option<usize> {
        let cmp = self.compare(test)?;
        let mask = self.binary(cmp.op, cmp.left, cmp.right);
        Some(if cmp.inverted {
            self.emit(OpCode::Unary(Unary::Not), vec![mask])
        } else {
            mask
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
        let arr = self.shape.scalar_arr();
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
                Op::Bits { value, .. } if !full && self.values.func.ty(value) == Type::I32 => {
                    let input = self.scalar(value, u32::MAX)?;
                    self.insert_dwords(input)
                }
                Op::VecUnary(VecUnary::Shuffle(1, imm, false), a) if !full => {
                    let a = self.packed(a)?;
                    let lane = imm & 3;
                    if imm == lane * 0x55 {
                        self.emit(OpCode::Unary(Unary::SplatWord(lane)), vec![a])
                    } else {
                        let bytes = std::array::from_fn(|i| {
                            let word = i / 2;
                            let lane = imm >> (2 * (word % 4)) & 3;
                            (8 * (word / 4) + 2 * usize::from(lane) + i % 2) as u8
                        });
                        let index = self.emit(
                            OpCode::Constant(u128::from_le_bytes(bytes), Arr::D2, true),
                            vec![],
                        );
                        self.binary(Binary::Table, a, index)
                    }
                }
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
                OpCode::Broadcast(v) => {
                    if self.values.func.ty(v) == Type::I32 {
                        code.push(Step::Gp(v, target(), self.shape.scalar_arr()));
                    } else {
                        code.push(Step::Vector(v, target()));
                    }
                }
                OpCode::Constant(bits, arr, full) => {
                    if bits == 0 {
                        code.push(Step::Word(vzero(true, target())));
                    } else {
                        code.push(Step::Imm(bits as u64));
                        code.push(Step::Word(vdup(arr, target(), SCALAR)));
                        if full {
                            let high = (bits >> 64) as u64;
                            if high != bits as u64 {
                                code.push(Step::Imm(high));
                                code.push(Step::Word(vins_general(3, target(), 1, SCALAR)));
                            }
                        }
                    }
                }
                OpCode::Unary(op) => {
                    let word = match op {
                        Unary::Not => vnot(true, target(), read(0)),
                        Unary::Shift(op, arr, n) => vshift(op, arr, target(), read(0), n),
                        Unary::Column(column) => vpermute(
                            if column == 0 {
                                Permute::Uzp1
                            } else {
                                Permute::Uzp2
                            },
                            Arr::S4,
                            target(),
                            read(0),
                            read(0),
                        ),
                        Unary::SplatWord(lane) => {
                            let tmp = VReg::new(28);
                            code.push(Step::Word(vpermute(
                                if lane & 1 == 0 {
                                    Permute::Trn1
                                } else {
                                    Permute::Trn2
                                },
                                Arr::H8,
                                tmp,
                                read(0),
                                read(0),
                            )));
                            vpermute(
                                if lane & 2 == 0 {
                                    Permute::Trn1
                                } else {
                                    Permute::Trn2
                                },
                                Arr::S4,
                                target(),
                                tmp,
                                tmp,
                            )
                        }
                    };
                    code.push(Step::Word(word));
                }
                OpCode::Binary(op) => {
                    let word = match op {
                        Binary::Same(op, arr) => vsame(op, arr, target(), read(0), read(1)),
                        Binary::Logic(op) => vlogic(op, true, target(), read(0), read(1)),
                        Binary::UnsignedGt(arr) => {
                            vsame(Same::Cmgt, arr, target(), read(0), read(1)) | 1 << 29
                        }
                        Binary::Permute(op, arr) => vpermute(op, arr, target(), read(0), read(1)),
                        Binary::Unpack(op, log) => {
                            let [a, b] = [VReg::new(28), VReg::new(29)];
                            let column = if op == Permute::Zip1 {
                                Permute::Uzp1
                            } else {
                                Permute::Uzp2
                            };
                            for (i, tmp) in [a, b].into_iter().enumerate() {
                                code.push(Step::Word(vpermute(
                                    column,
                                    Arr::S4,
                                    tmp,
                                    read(i),
                                    read(i),
                                )));
                            }
                            vpermute(
                                Permute::Zip1,
                                Arr::new(u32::from(log), true),
                                target(),
                                a,
                                b,
                            )
                        }
                        Binary::Pack(op, log) => {
                            let [a, b] = [VReg::new(28), VReg::new(29)];
                            for (i, tmp) in [a, b].into_iter().enumerate() {
                                code.push(Step::Word(vnarrow(
                                    op,
                                    Arr::new(u32::from(log), false),
                                    tmp,
                                    read(i),
                                )));
                            }
                            vpermute(Permute::Zip1, Arr::S4, target(), a, b)
                        }
                        Binary::Table => {
                            // TBL with one table register.
                            0x4E00_0000
                                | read(1).number() << 16
                                | read(0).number() << 5
                                | target().number()
                        }
                    };
                    code.push(Step::Word(word));
                }
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
                OpCode::Mask(test) => code.push(Step::Mask(test, read(0))),
                OpCode::LaneStore(stream, lane) => {
                    code.push(Step::LaneStore(stream, lane, read(0)))
                }
                OpCode::FixedCharge(cost) => code.push(Step::FixedCharge(cost)),
                OpCode::Publish(value, lane) => code.push(Step::Publish(value, lane, read(0))),
                OpCode::PublishBits(value, keep, set) => {
                    code.push(Step::PublishBits(value, keep, set))
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
                let arr = if self.shape == Shape::Packed(8) && s.width == 4 {
                    self.shape.scalar_arr()
                } else {
                    self.shape.arr()
                };
                code.push(Step::Word(vdup_lane(arr, dst, dst, 0)));
            }
            return;
        }
        let (work, bytes) = match self.shape {
            Shape::Scalar(w) => (w.trailing_zeros(), s.width * self.shape.lanes()),
            Shape::Packed(8) => (log, s.width * self.shape.lanes()),
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
        VecOp::Unpack(op @ (Permute::Zip1 | Permute::Zip2), log, false) if !full && log < 3 => {
            Binary::Unpack(op, log)
        }
        VecOp::Pack(op, log, false) if !full && log < 3 => Binary::Pack(op, log),
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
