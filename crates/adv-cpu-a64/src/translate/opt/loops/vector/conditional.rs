//! Classify the two logical iterations before any of their stores. A case
//! computes only the arms that occurred and publishes each last writer.

use super::{
    Access, BatchBody, BatchEmitter, Builder, Defs, End, FlagKind, Func, IntOp, IntUnary, Loop,
    Memory, Op, OpCode, Paths, Plan, Program, Shape, Stream, Target, Type, Unary, Value, Values,
    VecUnary, packed_binary,
};
use crate::asm::Label;
use crate::encode::Reg;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
struct Arm {
    bindings: BTreeMap<Value, Value>,
    store: Option<Stream>,
    charge: i32,
    path: Vec<(super::super::Block, usize)>,
}

impl Arm {
    fn value(&self, value: Value) -> Value {
        self.bindings.get(&value).copied().unwrap_or(value)
    }

    fn follow(&mut self, func: &Func, at: super::super::Block, edge: usize, target: &Target) {
        self.path.push((at, edge));
        self.charge += target.charge;
        for (&param, &argument) in func.blocks[target.block.0 as usize]
            .params
            .iter()
            .zip(&target.args)
        {
            self.bindings.insert(param, self.value(argument));
        }
    }
}

enum Dispatch {
    Test(usize, Box<Self>, Box<Self>),
    Arm(usize),
}

impl Dispatch {
    fn emit(&self, emitter: &mut BatchEmitter<'_, '_>, lane: u32, cases: &[Label]) {
        match self {
            Self::Arm(arm) => emitter.asm().b(cases[*arm]),
            Self::Test(test, taken, fall) => {
                let no = emitter.asm().label();
                emitter
                    .asm()
                    .tbz(false, Reg::new(*test as u8), 32 * lane, no);
                taken.emit(emitter, lane, cases);
                emitter.asm().bind(no);
                fall.emit(emitter, lane, cases);
            }
        }
    }

    fn depth(&self, child: usize) -> Option<u32> {
        match self {
            Self::Arm(arm) => (*arm == child).then_some(1),
            Self::Test(_, taken, fall) => taken
                .depth(child)
                .or_else(|| fall.depth(child))
                .map(|n| n + 1),
        }
    }
}

struct Walk<'a> {
    func: &'a Func,
    lp: &'a Loop,
    defs: &'a Defs,
    tests: Vec<(Value, BTreeMap<Value, Value>)>,
    arms: Vec<Arm>,
}

impl Walk<'_> {
    fn walk(&mut self, mut at: super::super::Block, mut arm: Arm) -> Option<Dispatch> {
        loop {
            let block = &self.func.blocks[at.0 as usize];
            if block.pcs.iter().any(|pc| self.func.calls.contains(pc)) {
                return None;
            }
            for &(_, op) in &block.insts {
                match op {
                    Op::Load { .. } if arm.store.is_some() => return None,
                    Op::Store {
                        value,
                        addr,
                        width: 8,
                        ..
                    } if arm.store.is_none() => {
                        let addr = self.defs.affine(addr);
                        arm.store = self
                            .lp
                            .streams
                            .iter()
                            .copied()
                            .find(|s| s.addr == addr && s.access == Access::Store(value));
                        arm.store?;
                    }
                    Op::Store { .. } => return None,
                    Op::PageCross { .. } | Op::FrameDisjoint { .. } | Op::Validate => {}
                    Op::Load { .. } | Op::Select { .. } | Op::Constant(..) | Op::Test { .. } => {}
                    Op::Flags {
                        kind:
                            FlagKind::Add
                            | FlagKind::Sub
                            | FlagKind::Logic
                            | FlagKind::Increment
                            | FlagKind::Decrement
                            | FlagKind::Shr,
                        ..
                    } => {}
                    Op::Int(
                        IntOp::Add
                        | IntOp::Sub
                        | IntOp::And
                        | IntOp::Or
                        | IntOp::Xor
                        | IntOp::Shl
                        | IntOp::Shr
                        | IntOp::Sar
                        | IntOp::Mul,
                        ..,
                    ) => {}
                    Op::IntUnary(
                        IntUnary::Not
                        | IntUnary::ZeroExtend8
                        | IntUnary::ZeroExtend16
                        | IntUnary::SignExtend8
                        | IntUnary::SignExtend16,
                        _,
                    ) => {}
                    Op::Bits { to, value }
                        if to != Type::F64 && self.func.ty(value) != Type::F64 => {}
                    Op::Vec(op, ..) if packed_binary(op, false).is_some() => {}
                    Op::VecUnary(
                        VecUnary::Shuffle(1, _, false)
                        | VecUnary::Low64
                        | VecUnary::ShiftImm(_, 0..=2, _, false),
                        _,
                    ) => {}
                    _ => return None,
                }
            }
            if at == self.lp.latch {
                arm.path.push((at, usize::from(!self.lp.latch_taken)));
                let leaf = self.arms.len();
                self.arms.push(arm);
                return Some(Dispatch::Arm(leaf));
            }
            match &block.end {
                End::Jump(edge) => {
                    arm.follow(self.func, at, 0, edge);
                    at = edge.block;
                }
                End::Branch { cond, taken, fall }
                    if self.tests.len() < 3 && arm.store.is_none() =>
                {
                    let slot = self.tests.len();
                    self.tests.push((*cond, arm.bindings.clone()));
                    let mut yes = arm.clone();
                    yes.follow(self.func, at, 0, taken);
                    arm.follow(self.func, at, 1, fall);
                    return Some(Dispatch::Test(
                        slot,
                        Box::new(self.walk(taken.block, yes)?),
                        Box::new(self.walk(fall.block, arm)?),
                    ));
                }
                _ => return None,
            }
        }
    }
}

pub(in crate::translate::opt) type Path = Vec<(super::super::Block, usize)>;

struct Cases {
    tests: Program,
    dispatch: Dispatch,
    cases: Vec<Vec<Program>>,
    paths: Vec<Path>,
    backwards: bool,
}

impl BatchBody for Cases {
    fn lower(&self, emitter: &mut BatchEmitter<'_, '_>) {
        let test_start = emitter.asm().offset();
        self.tests.lower(emitter);
        let test_words = ((emitter.asm().offset() - test_start) / 4) as u32;
        let count = self.paths.len();
        let first = (0..count)
            .map(|_| emitter.asm().label())
            .collect::<Vec<_>>();
        let exit = emitter.asm().label();
        let lane = u32::from(self.backwards);
        self.dispatch.emit(emitter, lane, &first);
        for (left, row) in self.cases.iter().enumerate() {
            emitter.asm().bind(first[left]);
            let choices = (0..count)
                .map(|_| emitter.asm().label())
                .collect::<Vec<_>>();
            self.dispatch.emit(emitter, lane ^ 1, &choices);
            for (right, program) in row.iter().enumerate() {
                emitter.asm().bind(choices[right]);
                emitter.count_class((left * count + right) as u8);
                let body_start = emitter.asm().offset();
                program.lower(emitter);
                emitter.asm().b(exit);
                let body = ((emitter.asm().offset() - body_start) / 4) as u32;
                let tests =
                    self.dispatch.depth(left).unwrap() + self.dispatch.depth(right).unwrap();
                emitter.price_class(
                    self.paths[left].clone(),
                    self.paths[right].clone(),
                    test_words + tests + body,
                );
            }
        }
        emitter.asm().bind(exit);
    }
}

fn transform(defs: &Defs, arm: &Arm, value: Value, param: Value) -> Option<(u32, u32)> {
    let value = arm.value(value);
    if value == param {
        return Some((u32::MAX, 0));
    }
    if let Some(bits) = defs.constant(value) {
        return Some((0, bits as u32));
    }
    let Op::Int(op @ (IntOp::And | IntOp::Or), left, right) = defs.op(value)? else {
        return None;
    };
    let rhs = defs.constant(arm.value(right))? as u32;
    let (keep, set) = transform(defs, arm, left, param)?;
    Some(if op == IntOp::And {
        (keep & rhs, set & rhs)
    } else {
        (keep & !rhs, set | rhs)
    })
}

pub(super) fn recognize(
    func: &Func,
    lp: &Loop,
    defs: &Defs,
    groups: &[Vec<(usize, Stream)>],
) -> Option<Plan> {
    if lp.streams.iter().any(|s| s.width > 8)
        || lp
            .blocks
            .iter()
            .filter_map(|b| func.blocks[b.0 as usize].head)
            .any(|head| {
                func.records[head.record.0 as usize]
                    .xmm
                    .iter()
                    .any(Option::is_some)
            })
    {
        return None;
    }
    let mut walk = Walk {
        func,
        lp,
        defs,
        tests: Vec::new(),
        arms: Vec::new(),
    };
    let dispatch = walk.walk(lp.head, Arm::default())?;
    if walk.tests.is_empty() {
        return None;
    }
    let first = walk.arms.iter().find_map(|arm| arm.store)?;
    if first.stride.unsigned_abs() != 8
        || walk
            .arms
            .iter()
            .filter_map(|arm| arm.store)
            .any(|s| s.addr != first.addr || s.stride != first.stride)
    {
        return None;
    }
    let paths = Paths::default();
    let stable = lp
        .carried
        .iter()
        .filter_map(|&(param, returned)| {
            walk.arms
                .iter()
                .all(|arm| arm.value(returned) == param)
                .then_some((param, u32::MAX))
        })
        .collect();
    let values = Values {
        func,
        defs,
        loop_: lp,
        paths: &paths,
        stable,
    };
    let shape = Shape::Packed(8);
    let backwards = first.stride < 0;
    let mut inputs = Vec::new();
    let mut program = |build: &mut dyn FnMut(&mut Builder<'_>) -> Option<()>| {
        let mut b = Builder {
            values: &values,
            shape,
            mapped: BTreeMap::new(),
            insts: Vec::new(),
            next: 0,
            bindings: BTreeMap::new(),
        };
        b.read_inputs(groups)?;
        build(&mut b)?;
        let code = b.live_code()?;
        inputs.extend(b.insts.iter().filter_map(|i| match i.op {
            OpCode::Broadcast(v) => Some(v),
            _ => None,
        }));
        Some(Program { shape, code })
    };
    let tests = program(&mut |b| {
        let loads = b.mapped.clone();
        for (test, (value, bindings)) in walk.tests.iter().enumerate() {
            b.bindings.clone_from(bindings);
            let condition = b.compare(*value)?;
            let mut bits = b.binary(condition.op, condition.left, condition.right);
            if condition.inverted {
                bits = b.emit(OpCode::Unary(Unary::Not), vec![bits]);
            }
            b.effect(OpCode::ClassTest(test as u8), vec![bits]);
            b.mapped.clone_from(&loads);
        }
        Some(())
    })?;
    let mut cases = Vec::new();
    for (a, left) in walk.arms.iter().enumerate() {
        let mut row = Vec::new();
        for (b, right) in walk.arms.iter().enumerate() {
            row.push(program(&mut |builder| {
                let mut stores = Vec::new();
                let mut outputs = Vec::new();
                let mut publish = [Vec::new(), Vec::new()];
                for &(param, returned) in &lp.carried {
                    if lp.ivs.iter().any(|iv| iv.base == param) {
                        continue;
                    }
                    let ty = func.ty(param);
                    if ty == Type::I32
                        && let (Some((k0, s0)), Some((k1, s1))) = (
                            transform(defs, left, returned, param),
                            transform(defs, right, returned, param),
                        )
                    {
                        let (keep, set) = (k0 & k1, (s0 & k1) | s1);
                        if (keep, set) != (u32::MAX, 0) {
                            outputs.push((OpCode::Update(param, keep, set), Vec::new()));
                        }
                        continue;
                    }
                    let lane = usize::from(right.value(returned) != param);
                    let arm = [left, right][lane];
                    let value = arm.value(returned);
                    if value != param {
                        publish[lane].push((param, value, ty));
                    }
                }
                let loads = builder.mapped.clone();
                let first = usize::from(a == b);
                for (lane, arm) in [(0usize, left), (1, right)].into_iter().skip(first) {
                    builder.bindings.clone_from(&arm.bindings);
                    if let Some(s) = arm.store {
                        let Access::Store(value) = s.access else {
                            unreachable!()
                        };
                        let value = builder.packed(value)?;
                        if a == b {
                            stores.push((
                                OpCode::Store(Memory {
                                    stream: s,
                                    count: 1,
                                }),
                                value,
                            ));
                        } else {
                            stores.push((OpCode::LaneStore(s, lane as u32), value));
                        }
                    }
                    for &(param, value, ty) in &publish[lane] {
                        let value = match ty {
                            Type::I32 => builder.scalar(value, u32::MAX)?,
                            Type::V128 => builder.packed(value)?,
                            _ => return None,
                        };
                        let physical = lane as u32 ^ u32::from(backwards);
                        outputs.push((OpCode::Publish(param, physical, ty), vec![value]));
                    }
                    builder.mapped.clone_from(&loads);
                }
                for (op, value) in stores {
                    builder.effect(op, vec![value]);
                }
                for (op, values) in outputs {
                    builder.effect(op, values);
                }
                let debit = left.charge + right.charge;
                if debit != 0 {
                    builder.effect(OpCode::Debit(debit), Vec::new());
                }
                Some(())
            })?);
        }
        cases.push(row);
    }
    inputs.sort_unstable_by_key(|v| v.0);
    inputs.dedup();
    Some(Plan {
        name: "conditional",
        lanes: 2,
        inputs,
        conditional: true,
        body: Box::new(Cases {
            tests,
            dispatch,
            cases,
            backwards,
            paths: walk.arms.into_iter().map(|arm| arm.path).collect(),
        }),
    })
}
