//! Propagation removes frame traffic and mode tests; liveness keeps only values
//! read by execution or recovery. Floating operations remain until validation.
mod fp_range;
mod head_flags;
use super::{
    BTreeMap, Block, ESP, End, Facts, FlagKind, Func, Head, IntOp, IntUnary, IrRecord, Location, M,
    MAX_PLAIN_FACTS, Op, Recipe, RecordId, Source, Target, Type, Value, VecOp, VecUnary,
    disjoint_ranges, facts,
};
use crate::translate::front::flags::FlagSet;
use adv_cpu_core::integer::{self, CF, OF};
use adv_cpu_core::memory::PAGE_SIZE;

pub(super) struct Resident<'a> {
    pub refused: &'a dyn Fn(u32) -> bool,
    pub regions: &'a BTreeMap<RecordId, RecordId>,
    pub frame_guards: &'a std::collections::BTreeSet<RecordId>,
    pub frame_range: Option<(i32, i32)>,
    pub frame_reads: &'a BTreeMap<u32, Vec<(i32, u8)>>,
    pub plain_sites: &'a mut Vec<u32>,
}

pub(super) fn run(
    func: &mut Func,
    facts: &dyn Facts,
    loads: &BTreeMap<Value, usize>,
    stores: &BTreeMap<RecordId, usize>,
    written_frame: &std::collections::BTreeSet<i32>,
    frame_hoist_safe: bool,
    mut resident: Option<&mut Resident<'_>>,
) {
    if let Some(i) = func.entry.iter().position(|s| *s == Source::Eflags) {
        let base = func.blocks[0].params[i];
        for block in &mut func.blocks {
            for (_, op) in &mut block.insts {
                if let Op::Flags {
                    kind:
                        FlagKind::Add
                        | FlagKind::Sub
                        | FlagKind::Logic
                        | FlagKind::Negate
                        | FlagKind::UnorderedCompare
                        | FlagKind::OrderedCompare,
                    prior,
                    ..
                } = op
                {
                    *prior = Some(base);
                }
            }
        }
    }
    let settle = |func: &mut Func| loop {
        let mut changed = propagate(func);
        changed |= reachable(func);
        if !changed {
            break;
        }
    };
    settle(func);
    let demand = resident.as_ref().map(|_| {
        split_resident_checkpoints(func);
        head_flags::prepare(func)
    });
    // A head's arrival may be another head's parameter, which settling
    // replaces with the entry's flags.
    while entry_flags(func, demand.as_deref()) {
        settle(func);
    }
    hoist(func, facts, loads, stores, resident.as_deref_mut());
    while propagate(func) {}
    if let Some(r) = &resident {
        store_guards(func, facts, loads, stores, r);
        for block in &func.blocks {
            if let Some(&pc) = block.pcs.first() {
                func.cold_pcs
                    .extend(block.insts.iter().filter_map(|&(value, op)| {
                        value
                            .filter(|_| op.can_fail() && !matches!(op, Op::Load { .. }))
                            .map(|value| (value, pc))
                    }));
            }
        }
    }
    if hoist_frame_loads(func, written_frame, frame_hoist_safe) {
        while propagate(func) {}
    }
    if fp_range::run(func) {
        while propagate(func) {}
    }
    join(func);
    group_stores(func, facts, loads, stores, resident.is_some());
    sse_inputs(func);
    dead(func);
    validations(func);
}

fn map_op(op: &mut Op, aliases: &[Value]) {
    let f = |v: &mut Value| *v = resolve(*v, aliases);
    match op {
        Op::Constant(..) | Op::Validate | Op::ScopeWritten(_) => {}
        Op::SseInput { a, b, .. } => {
            f(a);
            if let Some(b) = b {
                f(b);
            }
        }
        Op::Snapshot(a)
        | Op::IntUnary(_, a)
        | Op::Bits { value: a, .. }
        | Op::FpUnary(_, a)
        | Op::VecUnary(_, a)
        | Op::VecToInt(_, a)
        | Op::FromInt(a)
        | Op::ToInt { value: a, .. }
        | Op::X87Store { value: a, .. }
        | Op::Load { addr: a, .. }
        | Op::PlainPage { addr: a, .. }
        | Op::PageCross { addr: a, .. }
        | Op::Host(a)
        | Op::FrameLoad { base: a, .. }
        | Op::X87Load { bits: a, .. }
        | Op::Test { flags: a, .. } => f(a),
        Op::Int(_, a, b)
        | Op::Fp(_, a, b, _)
        | Op::X87Single(_, a, b, _)
        | Op::Fp32(_, a, b)
        | Op::SseFp64(_, a, b)
        | Op::Vec(_, a, b)
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
        } => {
            f(a);
            f(b);
        }
        Op::Flags { a, b, prior, .. } => {
            f(a);
            f(b);
            if let Some(p) = prior {
                f(p);
            }
        }
        Op::Select { cond, a, b }
        | Op::Remainder {
            quotient: cond,
            dividend: a,
            divisor: b,
        } => {
            f(cond);
            f(a);
            f(b);
        }
    }
}

fn map_record(record: &mut IrRecord, aliases: &[Value]) {
    substitute(record, |v| resolve(v, aliases));
}

fn substitute(record: &mut IrRecord, map: impl Fn(Value) -> Value) {
    for group in &mut record.flag_groups {
        *group = group.map(&mut &map);
    }
    let f = |v: &mut Value| *v = map(*v);
    for loc in record
        .regs
        .iter_mut()
        .chain([&mut record.eflags, &mut record.mmx_meta])
        .chain(record.xmm.iter_mut())
        .chain(record.mmx.iter_mut())
        .chain(record.x87.iter_mut().flat_map(|x| {
            x.stack
                .iter_mut()
                .chain([&mut x.c1, &mut x.ip, &mut x.opcode])
        }))
        .flatten()
        .chain(record.frame.iter_mut().map(|(_, _, loc)| loc))
    {
        match loc {
            Location::At(v) | Location::Recipe(Recipe::Eflags(v)) => f(v),
            Location::Recipe(Recipe::C1 { a, b, .. }) => {
                f(a);
                f(b);
            }
            Location::Constant(_) => {}
        }
    }
}

/// A64-FLAG-3: a Flags parameter becomes the entry EFLAGS when every arrival
/// takes each observed flag, and every flag besides the six status flags,
/// from the entry EFLAGS or from the parameter itself. A charged arrival first
/// reaches a head of its own whose record keeps that arrival's exact flags,
/// since its checkpoint may leave there.
fn entry_flags(func: &mut Func, demand: Option<&[FlagSet]>) -> bool {
    let Some(i) = func.entry.iter().position(|s| *s == Source::Eflags) else {
        return false;
    };
    let base = func.blocks[0].params[i];
    let mut defs = vec![None; func.types.len()];
    for block in &func.blocks {
        for &(r, op) in &block.insts {
            if let Some(r) = r {
                defs[r.0 as usize] = Some(op);
            }
        }
    }
    let observed = observed(func, &defs, demand);
    let from_entry = |mut v: Value, param: Value, bits: FlagSet| loop {
        if v == base || v == param {
            return true;
        }
        let Some(Op::Flags {
            kind,
            prior: Some(prior),
            ..
        }) = defs[v.0 as usize]
        else {
            return false;
        };
        if bits.minus(kept(kind)).any(FlagSet::ALL) {
            return false;
        }
        v = prior;
    };
    let mut changed = false;
    for b in 1..func.blocks.len() {
        let Some(head) = func.blocks[b].head else {
            continue;
        };
        let params = func.blocks[b].params.clone();
        for (p, &param) in params.iter().enumerate() {
            let arrivals: Vec<Value> = func
                .blocks
                .iter()
                .flat_map(|block| block.end.targets())
                .filter(|t| t.block.0 as usize == b)
                .map(|t| t.args[p])
                .collect();
            if func.ty(param) != Type::Flags
                || arrivals.iter().all(|&v| v == base)
                || !arrivals
                    .iter()
                    .all(|&v| from_entry(v, param, observed[param.0 as usize]))
            {
                continue;
            }
            changed = true;
            let mut edges = Vec::new();
            for (s, block) in func.blocks.iter_mut().enumerate() {
                for (k, target) in targets_mut(&mut block.end).into_iter().enumerate() {
                    if target.block.0 as usize == b && target.charge > 0 {
                        edges.push((s, k, target.args.clone()));
                    }
                    if target.block.0 as usize == b {
                        target.args[p] = base;
                    }
                }
            }
            for (s, k, args) in edges {
                split_checkpoint(func, b, head, &params, s, k, &args);
            }
        }
    }
    changed
}

fn split_resident_checkpoints(func: &mut Func) {
    let edges: Vec<_> = func
        .blocks
        .iter()
        .enumerate()
        .flat_map(|(from, block)| {
            block
                .end
                .targets()
                .enumerate()
                .filter(|(_, t)| t.charge > 0)
                .map(move |(k, t)| (from, k, t.clone()))
        })
        .collect();
    for (from, k, edge) in edges {
        let b = edge.block.0 as usize;
        let data = &func.blocks[b];
        let head = data.head.unwrap();
        let params = data.params.clone();
        split_checkpoint(func, b, head, &params, from, k, &edge.args);
    }
}

fn split_checkpoint(
    func: &mut Func,
    b: usize,
    head: Head,
    params: &[Value],
    from: usize,
    k: usize,
    args: &[Value],
) {
    let mut record = func.records[head.record.0 as usize].clone();
    substitute(&mut record, |v| {
        params.iter().position(|&q| q == v).map_or(v, |j| args[j])
    });
    let record = func.record(record);
    let split = func.block(Some(Head { record }));
    let mut ends = targets_mut(&mut func.blocks[from].end);
    let target = &mut ends[k];
    let through = std::mem::take(&mut target.args);
    target.block = split;
    func.blocks[split.0 as usize].end = End::Jump(Target {
        block: Block(b as u32),
        args: through,
        charge: 0,
    });
}

/// The status flags of each Flags value that tests, exits and checkpoints
/// may see. A region start's record observes nothing by itself: recovery
/// there continues with the same uses.
fn observed(func: &Func, defs: &[Option<Op>], demand: Option<&[FlagSet]>) -> Vec<FlagSet> {
    let mut seen = vec![FlagSet::NONE; func.types.len()];
    if let Some(demand) = demand {
        seen[..demand.len()].copy_from_slice(demand);
    }
    let mut charged = vec![false; func.blocks.len()];
    for block in &func.blocks {
        for t in block.end.targets() {
            charged[t.block.0 as usize] |= t.charge > 0;
        }
    }
    let flags = |record: RecordId| match func.records[record.0 as usize].eflags {
        Some(Location::Recipe(Recipe::Eflags(v)) | Location::At(v)) => Some(v),
        _ => None,
    };
    for (b, block) in func.blocks.iter().enumerate() {
        for &(_, op) in &block.insts {
            if let Op::Test { flags, cond } = op {
                seen[flags.0 as usize] = seen[flags.0 as usize].or(FlagSet::of(cond));
            }
        }
        if demand.is_none()
            && let End::Exit(record) = block.end
            && let Some(v) = flags(record)
        {
            seen[v.0 as usize] = FlagSet::ALL;
        }
        if demand.is_none()
            && let Some(head) = block.head
            && charged[b]
            && let Some(v) = flags(head.record)
            && !block.params.contains(&v)
        {
            seen[v.0 as usize] = FlagSet::ALL;
        }
        for t in block
            .end
            .targets()
            .filter(|t| demand.is_none() && t.charge > 0)
        {
            for &v in &t.args {
                seen[v.0 as usize] = FlagSet::ALL;
            }
        }
    }
    let join = |seen: &mut [FlagSet], v: Value, bits: FlagSet| {
        let new = seen[v.0 as usize].or(bits);
        std::mem::replace(&mut seen[v.0 as usize], new) != new
    };
    loop {
        let mut changed = false;
        for (v, def) in defs.iter().enumerate() {
            if let Some(Op::Flags {
                kind,
                prior: Some(prior),
                ..
            }) = *def
            {
                let read = match kind {
                    FlagKind::AddCarry | FlagKind::SubBorrow => FlagSet::CF,
                    FlagKind::Shl
                    | FlagKind::Shr
                    | FlagKind::Sar
                    | FlagKind::Rol
                    | FlagKind::Ror => seen[v],
                    _ => FlagSet::NONE,
                };
                let passed = seen[v].minus(seen[v].minus(kept(kind)));
                changed |= join(&mut seen, prior, read.or(passed));
            }
        }
        for block in &func.blocks {
            for t in block.end.targets().filter(|t| t.charge <= 0) {
                let params = &func.blocks[t.block.0 as usize].params;
                for (&arg, &param) in t.args.iter().zip(params) {
                    let bits = seen[param.0 as usize];
                    changed |= join(&mut seen, arg, bits);
                }
            }
        }
        if !changed {
            return seen;
        }
    }
}

/// The status flags a kind leaves as its prior had them.
fn kept(kind: FlagKind) -> FlagSet {
    match kind {
        FlagKind::Increment | FlagKind::Decrement => FlagSet::CF,
        FlagKind::CarryBit => FlagSet::ALL.minus(FlagSet::CF),
        FlagKind::MulSigned | FlagKind::MulUnsigned | FlagKind::Rol | FlagKind::Ror => {
            FlagSet::ALL.minus(FlagSet::CF.or(FlagSet::OF))
        }
        _ => FlagSet::NONE,
    }
}

fn targets_mut(end: &mut End) -> Vec<&mut Target> {
    match end {
        End::Jump(t) => vec![t],
        End::Branch { taken, fall, .. } => vec![taken, fall],
        _ => Vec::new(),
    }
}

fn resolve(mut value: Value, aliases: &[Value]) -> Value {
    while aliases[value.0 as usize] != value {
        value = aliases[value.0 as usize];
    }
    value
}

fn propagate(func: &mut Func) -> bool {
    let n = func.types.len();
    let mut alias: Vec<Value> = (0..n).map(|i| Value(i as u32)).collect();
    let mut defs = vec![None; n];
    for block in &func.blocks {
        for &(r, op) in &block.insts {
            if let Some(r) = r {
                defs[r.0 as usize] = Some(op);
            }
        }
    }
    let mut changed = false;
    for b in 1..func.blocks.len() {
        for (p, &param) in func.blocks[b].params.iter().enumerate() {
            let arrivals: Vec<Value> = func
                .blocks
                .iter()
                .flat_map(|b| b.end.targets())
                .filter(|t| t.block.0 as usize == b)
                .map(|t| resolve(t.args[p], &alias))
                .filter(|&v| v != param)
                .collect();
            if let Some(&first) = arrivals.first() {
                let same = arrivals.iter().all(|&v| v == first);
                if same {
                    alias[param.0 as usize] = first;
                    changed = true;
                }
            }
        }
    }
    for (block_index, block) in func.blocks.iter_mut().enumerate() {
        let mut numbered: Vec<(Op, Value)> = Vec::new();
        for (result, op) in &mut block.insts {
            map_op(op, &alias);
            let Some(r) = *result else {
                continue;
            };
            // Constants go right, where immediates encode.
            if let Op::Int(kind, a, b) = *op
                && matches!(
                    kind,
                    IntOp::Add | IntOp::And | IntOp::Or | IntOp::Xor | IntOp::Mul
                )
                && constant(&defs, a).is_some()
                && constant(&defs, b).is_none()
            {
                *op = Op::Int(kind, b, a);
                changed = true;
            }
            if let Some(v) = identity(*op, &defs, &func.types) {
                let v = resolve(v, &alias);
                if r != v {
                    alias[r.0 as usize] = v;
                    changed = true;
                    continue;
                }
            }
            if let Some(widened) = sign_widened(*op, &defs, &func.types) {
                *op = widened;
                changed = true;
            }
            if let Some(bits) = fold(*op, &defs, &func.types) {
                let constant = Op::Constant(func.types[r.0 as usize], bits);
                if *op != constant {
                    *op = constant;
                    changed = true;
                }
            }
            defs[r.0 as usize] = Some(*op);
            if op.pure()
                || block_index == 0
                    && matches!(
                        op,
                        Op::Load {
                            clock: false,
                            scope: None,
                            ..
                        }
                    )
            {
                if let Some((_, other)) = numbered.iter().find(|(other, _)| match (*other, *op) {
                    (
                        Op::Load {
                            ty: a,
                            width: aw,
                            addr: av,
                            checked: ac,
                            ..
                        },
                        Op::Load {
                            ty: b,
                            width: bw,
                            addr: bv,
                            checked: bc,
                            ..
                        },
                    ) => (a, aw, av, ac) == (b, bw, bv, bc),
                    _ => other == op,
                }) {
                    alias[r.0 as usize] = *other;
                    changed = true;
                } else {
                    numbered.push((*op, r));
                }
            }
        }
    }
    let mut remove = Vec::new();
    for (b, block) in func.blocks.iter_mut().enumerate() {
        let indices: Vec<usize> = block
            .params
            .iter()
            .enumerate()
            .filter_map(|(i, &p)| (resolve(p, &alias) != p).then_some(i))
            .collect();
        for &i in indices.iter().rev() {
            block.params.remove(i);
        }
        if !indices.is_empty() {
            remove.push((Block(b as u32), indices));
        }
        block
            .insts
            .retain(|(r, _)| r.is_none_or(|r| resolve(r, &alias) == r));
        for (_, op) in &mut block.insts {
            map_op(op, &alias);
        }
        for target in match &mut block.end {
            End::Jump(t) => vec![t],
            End::Branch { taken, fall, .. } => vec![taken, fall],
            _ => Vec::new(),
        } {
            for v in &mut target.args {
                *v = resolve(*v, &alias);
            }
        }
        if let End::Branch { cond, .. } = &mut block.end {
            *cond = resolve(*cond, &alias);
        }
    }
    for block in &mut func.blocks {
        for target in match &mut block.end {
            End::Jump(t) => vec![t],
            End::Branch { taken, fall, .. } => vec![taken, fall],
            _ => Vec::new(),
        } {
            if let Some((_, indices)) = remove.iter().find(|(b, _)| *b == target.block) {
                for &i in indices.iter().rev() {
                    target.args.remove(i);
                }
            }
        }
        if let End::Branch { cond, taken, fall } = &block.end
            && let Some(Op::Constant(_, bits)) = defs[cond.0 as usize]
        {
            block.end = End::Jump(if bits != 0 {
                taken.clone()
            } else {
                fall.clone()
            });
            changed = true;
        }
    }
    for record in &mut func.records {
        map_record(record, &alias);
    }
    changed
}

fn constant(defs: &[Option<Op>], v: Value) -> Option<u128> {
    if let Some(Op::Constant(_, bits)) = defs[v.0 as usize] {
        Some(bits)
    } else {
        None
    }
}

/// How many low bits of an integer value may be set: the rest are zero.
fn significant(defs: &[Option<Op>], v: Value, depth: u8) -> u32 {
    let bits = |v| {
        if depth == 0 {
            64
        } else {
            significant(defs, v, depth - 1)
        }
    };
    match defs[v.0 as usize] {
        Some(Op::Constant(_, c)) => 128 - c.leading_zeros(),
        Some(Op::Load {
            ty: Type::I32 | Type::I64,
            width,
            ..
        }) => u32::from(width) * 8,
        Some(Op::IntUnary(IntUnary::ZeroExtend8, _)) => 8,
        Some(Op::IntUnary(IntUnary::ZeroExtend16, _)) => 16,
        Some(Op::IntUnary(IntUnary::Widen, a)) => bits(a).min(32),
        Some(Op::Int(IntOp::And, a, b)) => bits(a).min(bits(b)),
        Some(Op::Int(IntOp::Or | IntOp::Xor, a, b)) => bits(a).max(bits(b)),
        Some(Op::Int(IntOp::Shr, a, b)) => match constant(defs, b) {
            Some(k) if k < 32 => bits(a).saturating_sub(k as u32),
            _ => 64,
        },
        _ => 64,
    }
}

fn identity(op: Op, defs: &[Option<Op>], types: &[Type]) -> Option<Value> {
    // A mask or zero extension that keeps every bit its operand may set.
    let keeps = |a: Value, mask: u128| {
        let bits = significant(defs, a, 8);
        bits < 64 && mask & ((1 << bits) - 1) == (1 << bits) - 1
    };
    let size = |ty: Type| match ty {
        Type::I32 => 4,
        Type::V128 => 16,
        _ => 8,
    };
    let bits_of = |v: Value| match defs[v.0 as usize] {
        Some(Op::Bits { value, .. }) => Some(value),
        _ => None,
    };
    // The I64 whose low (`high` false) or high dword `v` zero-extends.
    let half = |v: Value, high: bool| {
        let mut whole = bits_of(bits_of(v)?)?;
        if high {
            let Some(Op::Int(IntOp::Shr, x, k)) = defs[whole.0 as usize] else {
                return None;
            };
            (constant(defs, k) == Some(32)).then_some(())?;
            whole = x;
        }
        (types[v.0 as usize] == Type::I64
            && types[bits_of(v)?.0 as usize] == Type::I32
            && types[whole.0 as usize] == Type::I64)
            .then_some(whole)
    };
    // `low` | `shifted` rejoins the dwords of one I64 when `shifted` moves
    // the high dword up by 32.
    let joined = |low: Value, shifted: Value| {
        let Some(Op::Int(IntOp::Shl, high, k)) = defs[shifted.0 as usize] else {
            return None;
        };
        let whole = half(low, false)?;
        (constant(defs, k) == Some(32) && half(high, true)? == whole).then_some(whole)
    };
    match op {
        Op::Int(IntOp::And, a, b) if constant(defs, b).is_some_and(|m| keeps(a, m)) => Some(a),
        Op::Int(IntOp::And, a, b) if constant(defs, a).is_some_and(|m| keeps(b, m)) => Some(b),
        Op::IntUnary(IntUnary::ZeroExtend8, a) if keeps(a, 0xff) => Some(a),
        Op::IntUnary(IntUnary::ZeroExtend16, a) if keeps(a, 0xffff) => Some(a),
        Op::Int(
            IntOp::Add | IntOp::Sub | IntOp::Or | IntOp::Xor | IntOp::Shl | IntOp::Shr | IntOp::Sar,
            a,
            b,
        ) if constant(defs, b) == Some(0) => Some(a),
        Op::Int(IntOp::Add | IntOp::Or | IntOp::Xor, a, b) if constant(defs, a) == Some(0) => {
            Some(b)
        }
        Op::Int(IntOp::And | IntOp::Or, a, b) if a == b => Some(a),
        Op::Vec(VecOp::And(true) | VecOp::Or(true), a, b) if a == b => Some(a),
        Op::Int(IntOp::Mul, a, b) if constant(defs, b) == Some(1) => Some(a),
        Op::Int(IntOp::Mul, a, b) if constant(defs, a) == Some(1) => Some(b),
        Op::Select { cond, a, b } => {
            if a == b {
                Some(a)
            } else {
                constant(defs, cond).map(|c| if c != 0 { a } else { b })
            }
        }
        Op::Int(IntOp::Or, a, b) => joined(a, b).or_else(|| joined(b, a)),
        // A round trip through a type at least as wide loses no bits.
        Op::Bits { to, value } => {
            let x = bits_of(value)?;
            (types[x.0 as usize] == to && size(types[value.0 as usize]) >= size(to)).then_some(x)
        }
        _ => None,
    }
}

/// An I64 joined from an I32 and its sign, as CDQ before IDIV builds it,
/// is that I32 sign-extended.
fn sign_widened(op: Op, defs: &[Option<Op>], types: &[Type]) -> Option<Op> {
    let Op::Int(IntOp::Or, a, b) = op else {
        return None;
    };
    let def = |v: Value| defs[v.0 as usize];
    let widened = |low: Value, shifted: Value| {
        let Some(Op::Bits { value: x, .. }) = def(low) else {
            return None;
        };
        let Some(Op::Int(IntOp::Shl, high, k)) = def(shifted) else {
            return None;
        };
        let Some(Op::Bits { value: sign, .. }) = def(high) else {
            return None;
        };
        let Some(Op::Int(IntOp::Sar, y, n)) = def(sign) else {
            return None;
        };
        (types[low.0 as usize] == Type::I64
            && types[high.0 as usize] == Type::I64
            && types[x.0 as usize] == Type::I32
            && y == x
            && constant(defs, k) == Some(32)
            && constant(defs, n) == Some(31))
        .then_some(Op::IntUnary(IntUnary::SignWiden, x))
    };
    widened(a, b).or_else(|| widened(b, a))
}

fn fold(op: Op, defs: &[Option<Op>], types: &[Type]) -> Option<u128> {
    let c = |v| constant(defs, v);
    let truncate = |v: u128, ty| {
        v & match ty {
            Type::I32 | Type::Flags => u128::from(u32::MAX),
            Type::I64 | Type::F64 => u128::from(u64::MAX),
            Type::V128 => u128::MAX,
        }
    };
    Some(match op {
        Op::Bits { to, value } => truncate(c(value)?, to),
        Op::IntUnary(op, v) => {
            let x = c(v)?;
            truncate(
                match op {
                    IntUnary::Not => !x,
                    IntUnary::Neg => 0u128.wrapping_sub(x),
                    IntUnary::ZeroExtend8 => x & 255,
                    IntUnary::ZeroExtend16 => x & 65535,
                    IntUnary::SignExtend8 => x as u8 as i8 as i32 as u32 as u128,
                    IntUnary::SignExtend16 => x as u16 as i16 as i32 as u32 as u128,
                    IntUnary::Low32 => x & u128::from(u32::MAX),
                    IntUnary::High32 => x >> 32,
                    IntUnary::Widen => x & u128::from(u32::MAX),
                    IntUnary::ByteSwap => u128::from((x as u32).swap_bytes()),
                    IntUnary::SignWiden => u128::from(x as u32 as i32 as i64 as u64),
                },
                if matches!(op, IntUnary::Low32 | IntUnary::High32) {
                    Type::I32
                } else if matches!(op, IntUnary::Widen | IntUnary::SignWiden) {
                    Type::I64
                } else {
                    types[v.0 as usize]
                },
            )
        }
        Op::Int(op, a, b) => {
            let ty = types[a.0 as usize];
            if a == b && matches!(op, IntOp::Sub | IntOp::Xor) {
                return Some(0);
            }
            let (a, b) = (c(a)?, c(b)?);
            let shift = (b & if ty == Type::I64 { 63 } else { 31 }) as u32;
            truncate(
                match op {
                    IntOp::Add => a.wrapping_add(b),
                    IntOp::Sub => a.wrapping_sub(b),
                    IntOp::And => a & b,
                    IntOp::Or => a | b,
                    IntOp::Xor => a ^ b,
                    IntOp::Mul => a.wrapping_mul(b),
                    IntOp::Shl => a << shift,
                    IntOp::Shr => a >> shift,
                    IntOp::Ror if ty == Type::I64 => u128::from((a as u64).rotate_right(shift)),
                    IntOp::Ror => u128::from((a as u32).rotate_right(shift)),
                    IntOp::Sar => {
                        if ty == Type::I64 {
                            (a as u64 as i64 >> shift) as u64 as u128
                        } else {
                            (a as u32 as i32 >> shift) as u32 as u128
                        }
                    }
                    _ => return None,
                },
                ty,
            )
        }
        Op::Vec(
            VecOp::Xor(_) | VecOp::AndNot(_) | VecOp::Same(crate::encode::Same::Sub, _, _),
            a,
            b,
        ) if a == b => 0,
        Op::VecUnary(VecUnary::Low64, a) => c(a)? & u128::from(u64::MAX),
        Op::Test { flags, cond } => {
            let bits = if let Some(bits) = c(flags) {
                bits as u32
            } else {
                let Op::Flags {
                    kind,
                    width,
                    a,
                    b,
                    prior,
                } = defs[flags.0 as usize]?
                else {
                    return None;
                };
                if !matches!(
                    kind,
                    FlagKind::Add | FlagKind::Sub | FlagKind::Logic | FlagKind::Negate
                ) {
                    return None;
                }
                flags_constant(
                    kind,
                    width,
                    c(a)? as u32,
                    c(b)? as u32,
                    prior.and_then(c).unwrap_or(0) as u32,
                )?
            };
            u128::from(integer::condition_true(cond, bits))
        }
        Op::Flags {
            kind,
            width,
            a,
            b,
            prior,
        } => {
            let prior = match prior {
                Some(p) => c(p)? as u32,
                None => 0,
            };
            u128::from(flags_constant(
                kind,
                width,
                c(a)? as u32,
                c(b)? as u32,
                prior,
            )?)
        }
        Op::Div {
            signed,
            dividend,
            divisor,
            ..
        } => {
            let dividend = c(dividend)? as u64;
            let divisor = c(divisor)? as u32;
            if divisor == 0 {
                return None;
            }
            if signed {
                let a = i128::from(dividend as i64);
                let b = i128::from(divisor as i32);
                let q = a / b;
                if q < i128::from(i32::MIN) || q > i128::from(i32::MAX) {
                    return None;
                }
                q as i32 as u32 as u128
            } else {
                let q = dividend / u64::from(divisor);
                if q > u64::from(u32::MAX) {
                    return None;
                }
                u128::from(q)
            }
        }
        Op::Remainder {
            quotient,
            dividend,
            divisor,
        } => u128::from(
            (c(dividend)? as u32)
                .wrapping_sub((c(quotient)? as u32).wrapping_mul(c(divisor)? as u32)),
        ),
        _ => return None,
    })
}

fn flags_constant(kind: FlagKind, width: u8, x: u32, y: u32, mut out: u32) -> Option<u32> {
    match kind {
        FlagKind::Add | FlagKind::AddCarry | FlagKind::Sub | FlagKind::SubBorrow => {
            let carry = u32::from(
                matches!(kind, FlagKind::AddCarry | FlagKind::SubBorrow) && out & CF != 0,
            );
            integer::arithmetic(
                &mut out,
                x,
                y,
                carry,
                width,
                matches!(kind, FlagKind::Sub | FlagKind::SubBorrow),
            );
        }
        FlagKind::Logic => {
            integer::binary(&mut out, M::Or, x, 0, width);
        }
        FlagKind::Increment | FlagKind::Decrement | FlagKind::Negate => {
            integer::unary(
                &mut out,
                match kind {
                    FlagKind::Increment => M::Inc,
                    FlagKind::Decrement => M::Dec,
                    _ => M::Neg,
                },
                x,
                width,
            );
        }
        // A count of zero keeps the flags.
        FlagKind::Shl | FlagKind::Shr | FlagKind::Sar if y & 31 == 0 => {}
        FlagKind::Shl | FlagKind::Shr | FlagKind::Sar => {
            integer::shift(
                &mut out,
                match kind {
                    FlagKind::Shl => M::Shl,
                    FlagKind::Shr => M::Shr,
                    _ => M::Sar,
                },
                x,
                y & 31,
                width,
            );
        }
        FlagKind::MulSigned => {
            let product = integer::signed(x, width) * integer::signed(y, width);
            let overflow = product != integer::signed(product as u32 & integer::mask(width), width);
            out = out & !(CF | OF) | if overflow { CF | OF } else { 0 };
        }
        _ => return None,
    }
    Some(out)
}

fn reachable(func: &mut Func) -> bool {
    let mut live = vec![false; func.blocks.len()];
    let mut work = vec![0];
    while let Some(b) = work.pop() {
        if std::mem::replace(&mut live[b], true) {
            continue;
        }
        work.extend(func.blocks[b].end.targets().map(|t| t.block.0 as usize));
    }
    if live.iter().all(|&v| v) {
        return false;
    }
    let mut ids = vec![0; live.len()];
    let mut n = 0;
    for (i, &live) in live.iter().enumerate() {
        if live {
            ids[i] = n;
            n += 1;
        }
    }
    if let Some(head) = &mut func.head_exit {
        head.0 = ids[head.0 as usize];
    }
    let mut i = 0;
    func.blocks.retain(|_| {
        let keep = live[i];
        i += 1;
        keep
    });
    for block in &mut func.blocks {
        for target in match &mut block.end {
            End::Jump(t) => vec![t],
            End::Branch { taken, fall, .. } => vec![taken, fall],
            _ => Vec::new(),
        } {
            target.block = Block(ids[target.block.0 as usize]);
        }
    }
    true
}

fn dead(func: &mut Func) {
    let mut live = vec![false; func.types.len()];
    for block in &func.blocks {
        if let Some(head) = block.head {
            for v in func.records[head.record.0 as usize].uses() {
                live[v.0 as usize] = true;
            }
        }
        for (_, op) in &block.insts {
            if op.effect() {
                for v in op.uses() {
                    live[v.0 as usize] = true;
                }
            }
            if let Op::Store { after, .. } = op {
                for v in func.records[after.0 as usize].uses() {
                    live[v.0 as usize] = true;
                }
            }
        }
        match block.end {
            End::Branch { cond, .. } => live[cond.0 as usize] = true,
            End::Exit(r) => {
                for v in func.records[r.0 as usize].uses() {
                    live[v.0 as usize] = true;
                }
            }
            _ => {}
        }
    }
    loop {
        let mut changed = false;
        for block in &func.blocks {
            for (r, op) in &block.insts {
                if r.is_some_and(|r| live[r.0 as usize]) {
                    for v in op.uses() {
                        if !live[v.0 as usize] {
                            live[v.0 as usize] = true;
                            changed = true;
                        }
                    }
                }
            }
            for t in block.end.targets() {
                for (&p, &a) in func.blocks[t.block.0 as usize].params.iter().zip(&t.args) {
                    if live[p.0 as usize] && !live[a.0 as usize] {
                        live[a.0 as usize] = true;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    let removed: Vec<Vec<usize>> = func
        .blocks
        .iter()
        .map(|b| {
            b.params
                .iter()
                .enumerate()
                .filter_map(|(i, p)| (!live[p.0 as usize]).then_some(i))
                .collect()
        })
        .collect();
    let entry = &removed[0];
    for &i in entry.iter().rev() {
        func.entry.remove(i);
    }
    for (b, block) in func.blocks.iter_mut().enumerate() {
        for &i in removed[b].iter().rev() {
            block.params.remove(i);
        }
        block
            .insts
            .retain(|(r, op)| r.is_some_and(|r| live[r.0 as usize]) || op.effect());
        for t in match &mut block.end {
            End::Jump(t) => vec![t],
            End::Branch { taken, fall, .. } => vec![taken, fall],
            _ => Vec::new(),
        } {
            for &i in removed[t.block.0 as usize].iter().rev() {
                t.args.remove(i);
            }
        }
    }
}

/// A lane produced by validated arithmetic cannot be subnormal. Track which
/// lanes retain that property through copies, insertions and block parameters.
fn sse_inputs(func: &mut Func) {
    if !func.blocks.iter().any(|block| {
        block
            .insts
            .iter()
            .any(|(_, op)| matches!(op, Op::SseInput { single: false, .. }))
    }) {
        return;
    }

    let mut clean = vec![3u8; func.types.len()];
    for &p in &func.blocks[0].params {
        clean[p.0 as usize] = 0;
    }
    loop {
        let mut changed = false;
        for block in &func.blocks {
            for &(result, op) in &block.insts {
                let Some(result) = result else { continue };
                let lanes = |v: Value| clean[v.0 as usize];
                let mask = match op {
                    Op::Fp(..)
                    | Op::SseFp64(..)
                    | Op::FromInt(_)
                    | Op::Vec(VecOp::LessF64, ..)
                    | Op::X87Load { single: true, .. }
                    | Op::X87Load { denormal: true, .. } => 3,
                    Op::Constant(_, bits) => {
                        let clean = |shift| !f64::from_bits((bits >> shift) as u64).is_subnormal();
                        u8::from(clean(0)) | u8::from(clean(64)) << 1
                    }
                    Op::Bits { to, value }
                        if !matches!(to, Type::I32) && func.ty(value) != Type::I32 =>
                    {
                        if to == Type::V128 && func.ty(value) == Type::V128 {
                            lanes(value)
                        } else {
                            lanes(value) & 1 | 2
                        }
                    }
                    Op::Vec(VecOp::Insert64(high, from_high), a, b) => {
                        let bit = 1 << u8::from(high);
                        lanes(a) & !bit | ((lanes(b) >> u8::from(from_high) & 1) * bit)
                    }
                    Op::VecUnary(VecUnary::Low64, a) => lanes(a) & 1 | 2,
                    Op::VecUnary(VecUnary::High64, a) => lanes(a) >> 1 | 2,
                    Op::Select { a, b, .. } => lanes(a) & lanes(b),
                    _ => 0,
                };
                let known = &mut clean[result.0 as usize];
                changed |= *known != mask;
                *known = mask;
            }
            for t in block.end.targets() {
                for (&p, &a) in func.blocks[t.block.0 as usize].params.iter().zip(&t.args) {
                    let mask = clean[p.0 as usize] & clean[a.0 as usize];
                    changed |= clean[p.0 as usize] != mask;
                    clean[p.0 as usize] = mask;
                }
            }
        }
        if !changed {
            break;
        }
    }

    for block in &mut func.blocks {
        let mut checked = BTreeMap::<Value, u8>::new();
        block.insts.retain_mut(|(_, op)| {
            // A clean double lane can still contain a single subnormal.
            let Op::SseInput {
                a,
                b,
                packed,
                single: false,
                pc,
            } = *op
            else {
                return true;
            };
            let mask = if packed { 3 } else { 1 };
            let mut needs_check = |v: &Value| {
                let known = checked.entry(*v).or_insert(clean[v.0 as usize]);
                let needed = *known & mask != mask;
                *known |= mask;
                needed
            };
            let mut inputs = [Some(a), b].into_iter().flatten().filter(&mut needs_check);
            let Some(a) = inputs.next() else { return false };
            *op = Op::SseInput {
                a,
                b: inputs.next(),
                packed,
                single: false,
                pc,
            };
            true
        });
    }
}

fn validations(func: &mut Func) {
    let heads: Vec<bool> = func.blocks.iter().map(|b| b.head.is_some()).collect();
    let end_validates =
        |end: &End| matches!(end, End::Exit(_)) || end.targets().any(|t| heads[t.block.0 as usize]);
    let mut pending = vec![false; func.blocks.len()];
    loop {
        let mut changed = false;
        for (b, block) in func.blocks.iter().enumerate() {
            let mut p = pending[b];
            for (_, op) in &block.insts {
                p = if matches!(op, Op::Validate | Op::Store { .. } | Op::FrameStore { .. })
                    || op.region_guard().is_some()
                {
                    false
                } else {
                    p || op.floating()
                };
            }
            if !end_validates(&block.end) {
                for t in block.end.targets() {
                    let to = t.block.0 as usize;
                    if !heads[to] && p && !pending[to] {
                        pending[to] = true;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    for (b, block) in func.blocks.iter_mut().enumerate() {
        let mut p = pending[b];
        let mut insts = Vec::new();
        for inst in std::mem::take(&mut block.insts) {
            if p && (matches!(inst.1, Op::Store { .. } | Op::FrameStore { .. })
                || inst.1.region_guard().is_some())
            {
                insts.push((None, Op::Validate));
                p = false;
            }
            p |= inst.1.floating();
            insts.push(inst);
        }
        if p && end_validates(&block.end) {
            insts.push((None, Op::Validate));
        }
        block.insts = insts;
    }
}

#[derive(Clone, Copy)]
struct PlainRead {
    base: Option<Value>,
    start: u32,
    bytes: u32,
    addr: Value,
    pc: u32,
}

impl PlainRead {
    fn merged(mut self, other: Self) -> Option<Self> {
        if self.base != other.base {
            return None;
        }
        let forward = other.start.wrapping_sub(self.start);
        let backward = self.start.wrapping_sub(other.start);
        if forward <= PAGE_SIZE as u32 - other.bytes {
            self.bytes = self.bytes.max(forward + other.bytes);
        } else if backward <= PAGE_SIZE as u32 - self.bytes {
            self.bytes = other.bytes.max(backward + self.bytes);
            self.start = other.start;
            self.addr = other.addr;
        } else {
            return None;
        }
        Some(self)
    }
}

/// Moves loads that read invariant memory, and the operations their
/// addresses need, out of loops into block 0. Only blocks that run on every
/// iteration give them up: hoisting from a side path would hold a register
/// across the loop for code that rarely runs. Other operations stay where
/// they are used, so they hold no register across the loop either. Nothing
/// moves when the loads would not fit in registers beside what loop heads
/// carry.
fn hoist(
    func: &mut Func,
    facts: &dyn Facts,
    loads: &BTreeMap<Value, usize>,
    stores: &BTreeMap<RecordId, usize>,
    mut resident: Option<&mut Resident<'_>>,
) {
    let every = per_iteration(func, resident.is_some());
    let mut available = vec![false; func.types.len()];
    for (&p, source) in func.blocks[0].params.iter().zip(&func.entry) {
        available[p.0 as usize] = resident.is_none() || matches!(source, Source::Reg(_));
    }
    for &(r, op) in &func.blocks[0].insts {
        if let Some(r) = r
            && (resident.is_none() || op.pure() && op.uses().all(|v| available[v.0 as usize]))
        {
            available[r.0 as usize] = true;
        }
    }
    let defs = definitions(func);
    let writes: Vec<_> = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter_map(|&(_, op)| match op {
            Op::Store {
                addr, width, after, ..
            } => Some((addr, width, stores[&after])),
            _ => None,
        })
        .collect();
    let frame_commits = func.blocks.iter().any(|b| {
        b.insts
            .iter()
            .any(|(_, op)| matches!(op, Op::FrameStore { .. }))
    });
    let invariant = |r: Value, op: Op| {
        let Op::Load {
            addr,
            width,
            clock: false,
            scope: None,
            ..
        } = op
        else {
            return false;
        };
        loads.get(&r).is_some_and(|&i| {
            facts.invariant_read(i)
                || resident.is_some()
                    && !frame_commits
                    && writes.iter().all(|&(store, bytes, site)| {
                        relation(&defs, store, bytes, addr, width) == Some(true)
                            || facts.disjoint_effects(site, i)
                    })
        })
    };
    // What could move, then what hoisted loads need.
    let mut movable = available.clone();
    let mut ops = BTreeMap::new();
    loop {
        let mut changed = false;
        for (b, block) in func.blocks.iter().enumerate().skip(1) {
            for &(r, op) in &block.insts {
                if let Some(r) = r
                    && every[b]
                    && !movable[r.0 as usize]
                    && (op.pure() || resident.is_none() && invariant(r, op))
                    && op.uses().all(|v| movable[v.0 as usize])
                {
                    movable[r.0 as usize] = true;
                    ops.insert(r, op);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut spans = Vec::<PlainRead>::new();
    if let Some(resident) = &resident {
        let mut remaining = MAX_PLAIN_FACTS - resident.plain_sites.len();
        for (b, block) in func.blocks.iter().enumerate().skip(1) {
            let mut before_effect = every[b];
            for &(r, op) in &block.insts {
                if let (
                    Some(r),
                    Op::Load {
                        addr, width, pc, ..
                    },
                ) = (r, op)
                    && before_effect
                    && invariant(r, op)
                    && movable[addr.0 as usize]
                {
                    let (base, start) = address(&defs, addr);
                    let span = PlainRead {
                        base,
                        start,
                        bytes: u32::from(width),
                        addr,
                        pc,
                    };
                    // The first site identifies the entire span when entry refuses it.
                    let refused = (resident.refused)(pc);
                    if let Some((known, merged)) = spans
                        .iter_mut()
                        .find_map(|known| known.merged(span).map(|merged| (known, merged)))
                    {
                        if (resident.refused)(known.pc) {
                            *known = merged;
                        } else if !refused {
                            *known = merged;
                            ops.insert(r, op);
                        }
                    } else if refused || remaining != 0 {
                        spans.push(span);
                        if !refused {
                            remaining -= 1;
                            ops.insert(r, op);
                        }
                    }
                }
                before_effect &= !hoist_effect(op);
            }
        }
    }
    let mut needed = vec![false; func.types.len()];
    let mut work: Vec<Value> = ops
        .iter()
        .filter(|(_, op)| matches!(op, Op::Load { .. }))
        .map(|(&r, _)| r)
        .collect();
    // A hoisted value holds a register across every loop: where loop heads
    // already carry more than the registers hold, it would only spill, and
    // its reload would cost what the load costs.
    let general = |v: &Value| matches!(func.ty(*v), Type::I32 | Type::I64 | Type::Flags);
    let carried = func
        .blocks
        .iter()
        .filter(|b| b.head.is_some())
        .map(|b| b.params.iter().filter(|v| general(v)).count())
        .max()
        .unwrap_or(0);
    if carried + work.iter().filter(|v| general(v)).count() > super::cells::CARRIED {
        return;
    }
    while let Some(r) = work.pop() {
        if std::mem::replace(&mut needed[r.0 as usize], true) {
            continue;
        }
        work.extend(ops[&r].uses().filter(|v| ops.contains_key(v)));
    }
    let mut reused = Vec::new();
    let entry_tail = if resident.is_some() {
        let entry = &mut func.blocks[0].insts;
        let start = entry
            .iter()
            .position(|&(r, op)| {
                !matches!(op, Op::PlainPage { .. }) && r.is_none_or(|r| !available[r.0 as usize])
            })
            .unwrap_or(entry.len());
        entry.split_off(start)
    } else {
        Vec::new()
    };
    loop {
        let mut moved = Vec::new();
        for block in &mut func.blocks[1..] {
            block.insts.retain(|&(r, op)| match r {
                Some(r) if needed[r.0 as usize] && op.uses().all(|v| available[v.0 as usize]) => {
                    if resident.is_some() && matches!(op, Op::Load { .. }) {
                        reused.push((Some(r), op));
                    } else {
                        moved.push((Some(r), op));
                    }
                    available[r.0 as usize] = true;
                    false
                }
                _ => true,
            });
        }
        if moved.is_empty() {
            break;
        }
        func.blocks[0].insts.extend(moved);
    }
    if let Some(resident) = &mut resident {
        for span in spans
            .into_iter()
            .filter(|span| !(resident.refused)(span.pc))
        {
            func.push(
                Block(0),
                Op::PlainPage {
                    addr: span.addr,
                    bytes: span.bytes,
                    write: false,
                },
            );
            resident.plain_sites.push(span.pc);
        }
        func.blocks[0].insts.extend(reused);
        func.blocks[0].insts.extend(entry_tail);
    }
}

fn hoist_effect(op: Op) -> bool {
    op.effect()
        && !matches!(
            op,
            Op::Load {
                checked: false,
                clock: false,
                ..
            } | Op::PlainPage { .. }
        )
}

fn apply_aliases(func: &mut Func, alias: &[Value]) {
    for block in &mut func.blocks {
        for (_, op) in &mut block.insts {
            map_op(op, alias);
        }
        if let End::Branch { cond, .. } = &mut block.end {
            *cond = resolve(*cond, alias);
        }
        for target in targets_mut(&mut block.end) {
            for arg in &mut target.args {
                *arg = resolve(*arg, alias);
            }
        }
    }
    for record in &mut func.records {
        map_record(record, alias);
    }
}

fn dominators(func: &Func) -> Vec<Vec<u64>> {
    let count = func.blocks.len();
    let words = count.div_ceil(64);
    let mut predecessors = vec![Vec::new(); count];
    for (b, data) in func.blocks.iter().enumerate() {
        for t in data.end.targets() {
            predecessors[t.block.0 as usize].push(b);
        }
    }
    let mut dom = vec![vec![u64::MAX; words]; count];
    dom[0] = vec![0; words];
    dom[0][0] = 1;
    loop {
        let mut changed = false;
        for b in 1..count {
            let mut set = vec![u64::MAX; words];
            for &p in &predecessors[b] {
                for (s, d) in set.iter_mut().zip(&dom[p]) {
                    *s &= d;
                }
            }
            set[b / 64] |= 1 << (b % 64);
            if set != dom[b] {
                dom[b] = set;
                changed = true;
            }
        }
        if !changed {
            return dom;
        }
    }
}

/// Hoist unit-invariant `X87Load`s (from `Block(0)` values or invariant
/// `FrameLoad`s) and direct `F64` frame loads out of loops into `Block(0)`,
/// budgeting hoisted `F64` values against allocatable vector registers rather
/// than `CARRIED` GPRs.
fn hoist_frame_loads(
    func: &mut Func,
    written_frame: &std::collections::BTreeSet<i32>,
    frame_hoist_safe: bool,
) -> bool {
    if func.blocks.len() <= 1 {
        return false;
    }
    let dom = dominators(func);
    let has = |set: &[u64], b: usize| set[b / 64] >> (b % 64) & 1 != 0;
    let count = func.blocks.len();
    let mut every_iter = vec![false; count];
    let mut has_backedge = false;
    for (latch, data) in func.blocks.iter().enumerate() {
        for head in data.end.targets().map(|t| t.block.0 as usize) {
            if !has(&dom[latch], head) {
                continue;
            }
            has_backedge = true;
            for b in 0..count {
                every_iter[b] |= has(&dom[b], head) && has(&dom[latch], b);
            }
        }
    }
    if !has_backedge {
        return false;
    }
    let exits: Vec<usize> = func
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(b, data)| matches!(data.end, End::Exit(_)).then_some(b))
        .collect();
    let runs_on_entry = |b: usize| exits.iter().all(|&e| has(&dom[e], b));

    let insert_pos = func.blocks[0]
        .insts
        .iter()
        .position(|(_, op)| matches!(op, Op::FrameStore { .. } | Op::Store { .. }))
        .unwrap_or(func.blocks[0].insts.len());
    let last_plain = func.blocks[0]
        .insts
        .iter()
        .rposition(|(_, op)| matches!(op, Op::PlainPage { .. }))
        .map_or(0, |i| i + 1);
    if insert_pos < last_plain {
        return false;
    }
    let mut block0_defs = vec![false; func.types.len()];
    for &p in &func.blocks[0].params {
        block0_defs[p.0 as usize] = true;
    }
    for &(r, _) in &func.blocks[0].insts[..insert_pos] {
        if let Some(r) = r {
            block0_defs[r.0 as usize] = true;
        }
    }

    let can_hoist_frame =
        frame_hoist_safe && (!func.resident || func.entry.contains(&Source::Reg(ESP)));
    let mut written = written_frame.clone();
    if can_hoist_frame {
        for block in &func.blocks {
            for &(_, op) in &block.insts {
                if let Op::FrameStore { offset, value, .. } = op {
                    let width = if func.ty(value) == Type::I32 { 4 } else { 8 };
                    written.extend(offset..offset + width);
                }
            }
        }
    }

    let mut use_count = vec![0usize; func.types.len()];
    for block in &func.blocks {
        for &(_, op) in &block.insts {
            for v in op.uses() {
                use_count[v.0 as usize] += 1;
            }
        }
        if let End::Branch { cond, .. } = block.end {
            use_count[cond.0 as usize] += 1;
        }
        for t in block.end.targets() {
            for &v in &t.args {
                use_count[v.0 as usize] += 1;
            }
        }
    }
    for record in &func.records {
        for v in record.uses() {
            use_count[v.0 as usize] += 1;
        }
    }

    // Map eligible invariant FrameLoads in blocks 1.. to their op.
    let mut frame_loads = BTreeMap::<Value, (Type, Value, i32)>::new();
    if can_hoist_frame {
        for (b, block) in func.blocks.iter().enumerate().skip(1) {
            if !every_iter[b] {
                continue;
            }
            for &(r, op) in &block.insts {
                if let (Some(r), Op::FrameLoad { ty, base, offset }) = (r, op)
                    && block0_defs[base.0 as usize]
                {
                    let width = if ty == Type::I32 { 4 } else { 8 };
                    if (offset..offset + width).all(|o| !written.contains(&o)) {
                        frame_loads.insert(r, (ty, base, offset));
                    }
                }
            }
        }
    }

    // Collect eligible X87Load users of invariant FrameLoads and of Block(0) values.
    let mut x87_users = BTreeMap::<Value, Vec<(Value, bool, bool, bool)>>::new();
    let mut block0_x87 = Vec::<(Value, Value, bool, bool, bool)>::new();
    for (b, block) in func.blocks.iter().enumerate().skip(1) {
        if !every_iter[b] {
            continue;
        }
        for &(r, op) in &block.insts {
            if let (
                Some(r),
                Op::X87Load {
                    bits,
                    denormal,
                    single,
                    proved,
                },
            ) = (r, op)
                && (proved || runs_on_entry(b))
            {
                if block0_defs[bits.0 as usize] {
                    block0_x87.push((r, bits, denormal, single, proved));
                } else if frame_loads.contains_key(&bits) {
                    x87_users
                        .entry(bits)
                        .or_default()
                        .push((r, denormal, single, proved));
                }
            }
        }
    }

    if block0_x87.is_empty() && frame_loads.is_empty() {
        return false;
    }

    let has_packed_sse = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .any(|(_, op)| matches!(op, Op::SseInput { packed: true, .. }));
    let vec_budget: usize = if has_packed_sse { 18 } else { 22 };
    let carried_vec = func
        .blocks
        .iter()
        .skip(1)
        .filter(|b| b.head.is_some())
        .map(|b| {
            b.params
                .iter()
                .filter(|v| {
                    matches!(func.ty(**v), Type::F64 | Type::V128) && use_count[v.0 as usize] > 0
                })
                .count()
        })
        .max()
        .unwrap_or(0);
    let mut remaining_vecs = vec_budget.saturating_sub(carried_vec);
    if remaining_vecs == 0 {
        return false;
    }

    let mut hoisted_fl = BTreeMap::<(u8, Value, i32), Value>::new();
    let mut hoisted_xl = BTreeMap::<(Value, bool, bool), Value>::new();
    let mut alias: Vec<Value> = (0..func.types.len()).map(|i| Value(i as u32)).collect();
    let mut moved = Vec::<(Option<Value>, Op)>::new();
    let mut remove = std::collections::BTreeSet::<Value>::new();

    // First hoist X87Loads whose raw bits are already defined in Block(0)
    // (such as Source::Frame parameters or hoisted Loads).
    for (xl, bits, denormal, single, proved) in block0_x87 {
        let xkey = (bits, denormal, single);
        if let Some(&existing) = hoisted_xl.get(&xkey) {
            alias[xl.0 as usize] = existing;
            remove.insert(xl);
        } else if remaining_vecs > 0 {
            remaining_vecs -= 1;
            hoisted_xl.insert(xkey, xl);
            moved.push((
                Some(xl),
                Op::X87Load {
                    bits,
                    denormal,
                    single,
                    proved,
                },
            ));
            remove.insert(xl);
        }
    }

    // Next hoist invariant FrameLoad (+ X87Load) pairs from blocks 1...
    for (b, block) in func.blocks.iter().enumerate().skip(1) {
        if !every_iter[b] {
            continue;
        }
        for &(r, op) in &block.insts {
            let Some(fl) = r else { continue };
            let Op::FrameLoad { ty, base, offset } = op else {
                continue;
            };
            if !frame_loads.contains_key(&fl) {
                continue;
            }
            let key = (ty as u8, base, offset);
            if ty == Type::F64 {
                if let Some(&existing) = hoisted_fl.get(&key) {
                    alias[fl.0 as usize] = existing;
                    remove.insert(fl);
                } else if remaining_vecs > 0 {
                    remaining_vecs -= 1;
                    hoisted_fl.insert(key, fl);
                    moved.push((Some(fl), op));
                    remove.insert(fl);
                }
                continue;
            }
            let Some(users) = x87_users.get(&fl) else {
                continue;
            };
            if users.len() != use_count[fl.0 as usize] {
                continue;
            }
            // Count how many new vector registers all users of this FrameLoad need.
            let existing_fl = hoisted_fl.get(&key).copied();
            let mut new_keys = std::collections::BTreeSet::new();
            for &(_, denormal, single, _) in users {
                if existing_fl.is_none_or(|cfl| !hoisted_xl.contains_key(&(cfl, denormal, single)))
                {
                    new_keys.insert((denormal, single));
                }
            }
            if new_keys.len() > remaining_vecs {
                continue;
            }
            let canon_fl = if let Some(existing) = existing_fl {
                remove.insert(fl);
                existing
            } else {
                hoisted_fl.insert(key, fl);
                moved.push((Some(fl), op));
                remove.insert(fl);
                fl
            };
            for &(xl, denormal, single, proved) in users {
                let xkey = (canon_fl, denormal, single);
                if let Some(&existing) = hoisted_xl.get(&xkey) {
                    alias[xl.0 as usize] = existing;
                    remove.insert(xl);
                } else {
                    remaining_vecs -= 1;
                    hoisted_xl.insert(xkey, xl);
                    moved.push((
                        Some(xl),
                        Op::X87Load {
                            bits: canon_fl,
                            denormal,
                            single,
                            proved,
                        },
                    ));
                    remove.insert(xl);
                }
            }
        }
    }

    if moved.is_empty() && remove.is_empty() {
        return false;
    }
    func.blocks[0].insts.splice(insert_pos..insert_pos, moved);
    for block in &mut func.blocks[1..] {
        block
            .insts
            .retain(|(r, _)| r.is_none_or(|r| !remove.contains(&r)));
    }
    apply_aliases(func, &alias);
    true
}

/// Blocks that run on every iteration of a loop that encloses them: the
/// loop head dominates them and they dominate one of its backedges.
fn per_iteration(func: &Func, resident: bool) -> Vec<bool> {
    let count = func.blocks.len();
    let has = |set: &[u64], b: usize| set[b / 64] >> (b % 64) & 1 != 0;
    let dominators = dominators(func);
    if resident {
        let mut required = Vec::new();
        let mut has_backedge = false;
        for (b, block) in func.blocks.iter().enumerate() {
            let backedge = block
                .end
                .targets()
                .any(|t| has(&dominators[b], t.block.0 as usize));
            has_backedge |= backedge;
            if backedge || block.insts.iter().any(|&(_, op)| hoist_effect(op)) {
                required.push(b);
            }
        }
        return (0..count)
            .map(|b| has_backedge && required.iter().all(|&end| has(&dominators[end], b)))
            .collect();
    }
    let mut every = vec![false; count];
    for (latch, data) in func.blocks.iter().enumerate() {
        for head in data.end.targets().map(|t| t.block.0 as usize) {
            if !has(&dominators[latch], head) {
                continue;
            }
            for b in 0..count {
                every[b] |= has(&dominators[b], head) && has(&dominators[latch], b);
            }
        }
    }
    every
}

fn join(func: &mut Func) {
    loop {
        let mut preds = vec![0; func.blocks.len()];
        for b in &func.blocks {
            for t in b.end.targets() {
                preds[t.block.0 as usize] += 1;
            }
        }
        let pair = func.blocks.iter().enumerate().find_map(|(i, b)| {
            let End::Jump(t) = &b.end else {
                return None;
            };
            let j = t.block.0 as usize;
            (j != 0
                && j != i
                && t.charge == 0
                && preds[j] == 1
                && func.blocks[j].head.is_none()
                && func.blocks[j].params.is_empty())
            .then_some((i, j))
        });
        let Some((a, b)) = pair else {
            break;
        };
        let insts = std::mem::take(&mut func.blocks[b].insts);
        let pcs = std::mem::take(&mut func.blocks[b].pcs);
        let end = std::mem::replace(&mut func.blocks[b].end, End::Unset);
        func.blocks[a].insts.extend(insts);
        func.blocks[a].pcs.extend(pcs);
        func.blocks[a].end = end;
        reachable(func);
    }
}

fn group_stores(
    func: &mut Func,
    facts: &dyn Facts,
    loads: &BTreeMap<Value, usize>,
    stores: &BTreeMap<RecordId, usize>,
    resident: bool,
) {
    let defs = definitions(func);
    for block in &mut func.blocks {
        let mut pending = Vec::new();
        let mut output = Vec::new();
        for inst in std::mem::take(&mut block.insts) {
            if matches!(inst.1, Op::Store { .. }) {
                if output
                    .last()
                    .is_some_and(|(_, op): &(Option<Value>, Op)| op.region_guard().is_some())
                {
                    output.push(inst);
                } else {
                    pending.push(inst);
                }
                continue;
            }
            let cross = match inst {
                (
                    Some(r),
                    Op::Load {
                        addr,
                        width,
                        checked,
                        clock: false,
                        ..
                    },
                ) => loads.get(&r).is_some_and(|&load| {
                    pending.iter().all(|(_, op)| {
                        let Op::Store {
                            after,
                            addr: store,
                            width: bytes,
                            checked: store_checked,
                            ..
                        } = *op
                        else {
                            unreachable!()
                        };
                        (!resident || !checked && !store_checked)
                            && (facts.disjoint_effects(stores[&after], load)
                                || resident
                                    && relation(&defs, store, bytes, addr, width) == Some(true))
                    })
                }),
                (
                    _,
                    Op::Div { .. }
                    | Op::SseInput { .. }
                    | Op::Validate
                    | Op::FrameStore { .. }
                    | Op::PageCross { .. },
                ) => false,
                (_, op) => !resident || !op.can_fail() && !matches!(op, Op::FrameLoad { .. }),
            };
            if !cross {
                output.append(&mut pending);
            }
            output.push(inst);
        }
        output.append(&mut pending);
        block.insts = output;
    }
}

fn definitions(func: &Func) -> Vec<Option<Op>> {
    let mut defs = vec![None; func.types.len()];
    for &(r, op) in func.blocks.iter().flat_map(|b| &b.insts) {
        if let Some(r) = r {
            defs[r.0 as usize] = Some(op);
        }
    }
    defs
}

/// An SSA base plus a constant displacement in guest address arithmetic.
fn address(defs: &[Option<Op>], mut addr: Value) -> (Option<Value>, u32) {
    let mut offset = 0u32;
    loop {
        match defs[addr.0 as usize] {
            Some(Op::Constant(Type::I32, n)) => return (None, offset.wrapping_add(n as u32)),
            Some(Op::Int(IntOp::Add, base, c)) if constant(defs, c).is_some() => {
                offset = offset.wrapping_add(constant(defs, c).unwrap() as u32);
                addr = base;
            }
            _ => return (Some(addr), offset),
        }
    }
}

fn relation(defs: &[Option<Op>], a: Value, aw: u8, b: Value, bw: u8) -> Option<bool> {
    let (ab, a) = address(defs, a);
    let (bb, b) = address(defs, b);
    (ab == bb).then(|| facts::disjoint(a, aw, b, bw))
}

/// A partial store must not change a load that replay from the region would read.
fn store_guards(
    func: &mut Func,
    facts: &dyn Facts,
    loads: &BTreeMap<Value, usize>,
    stores: &BTreeMap<RecordId, usize>,
    resident: &Resident<'_>,
) {
    let defs = definitions(func);
    let esp = func
        .entry
        .iter()
        .position(|&s| s == Source::Reg(ESP))
        .map(|i| func.blocks[0].params[i]);
    let mut incoming = vec![std::collections::BTreeSet::new(); func.blocks.len()];
    let mut work: Vec<_> = (1..func.blocks.len()).rev().collect();
    let load = |r: Option<Value>, op: Op| match (r, op) {
        (Some(r), Op::Load { addr, width, .. }) => {
            Some((address(&defs, addr), width, loads.get(&r).copied()))
        }
        _ => None,
    };
    let frame = |eip: Option<u32>| {
        eip.and_then(|pc| resident.frame_reads.get(&pc))
            .into_iter()
            .flatten()
            .map(|&(offset, width)| ((esp, offset as u32), width, None))
    };
    while let Some(b) = work.pop() {
        let block = &func.blocks[b];
        let mut read = incoming[b].clone();
        read.extend(frame(block.pcs.first().copied()));
        for &(r, op) in &block.insts {
            if matches!(op, Op::Store { .. }) {
                read.clear();
            }
            read.extend(load(r, op));
        }
        for t in block.end.targets() {
            let t = t.block.0 as usize;
            if func.blocks[t].head.is_none() && !read.is_subset(&incoming[t]) {
                incoming[t].extend(&read);
                work.push(t);
            }
        }
    }
    for (b, block) in func.blocks.iter_mut().enumerate().skip(1) {
        let mut read = std::mem::take(&mut incoming[b]);
        read.extend(frame(block.pcs.first().copied()));
        let insts = std::mem::take(&mut block.insts);
        for (r, op) in insts {
            if let Op::Store {
                addr, width, after, ..
            } = op
            {
                let page = PAGE_SIZE as u32;
                let (base, offset) = address(&defs, addr);
                let record = resident.regions[&after];
                let mut guarded_frame = false;
                if resident.frame_guards.contains(&after)
                    && let (Some(esp), Some((lo, hi))) = (esp, resident.frame_range)
                    && (base != Some(esp)
                        || !disjoint_ranges(
                            offset,
                            u32::from(width),
                            lo as u32,
                            (i64::from(hi) - i64::from(lo)) as u32,
                        ))
                {
                    block.insts.push((
                        None,
                        Op::FrameDisjoint {
                            addr,
                            width,
                            base: esp,
                            lo,
                            hi,
                            record,
                        },
                    ));
                    guarded_frame = true;
                }
                if width > 1
                    && (base.is_some() || offset % page > page - u32::from(width))
                    && read.iter().any(|&((source, at), size, site)| {
                        if guarded_frame && site.is_none() {
                            return false;
                        }
                        !(source == base && facts::disjoint(offset, width, at, size)
                            || site.is_some_and(|i| facts.disjoint_effects(stores[&after], i)))
                    })
                {
                    block.insts.push((
                        None,
                        Op::PageCross {
                            addr,
                            width,
                            record,
                        },
                    ));
                }
                read.clear();
            }
            read.extend(load(r, op));
            block.insts.push((r, op));
        }
    }
}
