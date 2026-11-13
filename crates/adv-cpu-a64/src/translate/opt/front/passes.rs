//! Propagation removes frame traffic and mode tests; liveness keeps only values
//! read by execution or recovery. Floating operations remain until validation.
use super::*;
use crate::translate::flags::FlagSet;
use adv_cpu_core::integer::{self, CF, OF};

pub(super) fn run(
    func: &mut Func,
    proof: &Proof,
    loads: &BTreeMap<Value, usize>,
    stores: &BTreeMap<RecordId, usize>,
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
                        | FlagKind::UnorderedCompare,
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
    // A head's arrival may be another head's parameter, which settling
    // replaces with the entry's flags.
    while entry_flags(func) {
        settle(func);
    }
    hoist(func, proof, loads);
    while propagate(func) {}
    join(func);
    group_stores(func, proof, loads, stores);
    dead(func);
    validations(func);
}

fn map_op(op: &mut Op, aliases: &[Value]) {
    let f = |v: &mut Value| *v = resolve(*v, aliases);
    match op {
        Op::Constant(..) | Op::Validate => {}
        Op::IntUnary(_, a)
        | Op::Bits { value: a, .. }
        | Op::FpUnary(_, a)
        | Op::FromInt(a)
        | Op::ToInt { value: a, .. }
        | Op::Load { addr: a, .. }
        | Op::Host(a)
        | Op::FrameLoad { base: a, .. }
        | Op::X87Load { bits: a, .. }
        | Op::Test { flags: a, .. } => f(a),
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
        Op::Select { cond, a, b } => {
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
    let f = |v: &mut Value| *v = map(*v);
    for loc in record
        .regs
        .iter_mut()
        .chain([&mut record.eflags])
        .chain(record.xmm.iter_mut())
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
fn entry_flags(func: &mut Func) -> bool {
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
    let observed = observed(func, &defs);
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
                let mut record = func.records[head.record.0 as usize].clone();
                substitute(&mut record, |v| {
                    params.iter().position(|&q| q == v).map_or(v, |j| args[j])
                });
                let record = func.record(record);
                let split = func.block(Some(Head { record }));
                let mut ends = targets_mut(&mut func.blocks[s].end);
                let target = &mut ends[k];
                let through = std::mem::take(&mut target.args);
                target.block = split;
                func.blocks[split.0 as usize].end = End::Jump(Target {
                    block: Block(b as u32),
                    args: through,
                    charge: 0,
                });
            }
        }
    }
    changed
}

/// The status flags of each Flags value that tests, exits and checkpoints
/// may see. A region start's record observes nothing by itself: recovery
/// there continues with the same uses.
fn observed(func: &Func, defs: &[Option<Op>]) -> Vec<FlagSet> {
    let mut seen = vec![FlagSet::NONE; func.types.len()];
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
        if let End::Exit(record) = block.end
            && let Some(v) = flags(record)
        {
            seen[v.0 as usize] = FlagSet::ALL;
        }
        if let Some(head) = block.head
            && charged[b]
            && let Some(v) = flags(head.record)
            && !block.params.contains(&v)
        {
            seen[v.0 as usize] = FlagSet::ALL;
        }
        for t in block.end.targets().filter(|t| t.charge > 0) {
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
                    FlagKind::Shl | FlagKind::Shr | FlagKind::Sar => seen[v],
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
            if let Some(v) = identity(*op, &defs) {
                let v = resolve(v, &alias);
                if r != v {
                    alias[r.0 as usize] = v;
                    changed = true;
                    continue;
                }
            }
            if let Some(bits) = fold(*op, &defs, &func.types) {
                let constant = Op::Constant(func.types[r.0 as usize], bits);
                if *op != constant {
                    *op = constant;
                    changed = true;
                }
            }
            defs[r.0 as usize] = Some(*op);
            if op.pure() || block_index == 0 && matches!(op, Op::Load { .. }) {
                if let Some((_, other)) = numbered.iter().find(|(other, _)| other == op) {
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

fn identity(op: Op, defs: &[Option<Op>]) -> Option<Value> {
    // A mask or zero extension that keeps every bit its operand may set.
    let keeps = |a: Value, mask: u128| {
        let bits = significant(defs, a, 8);
        bits < 64 && mask & ((1 << bits) - 1) == (1 << bits) - 1
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
        Op::Int(IntOp::Mul, a, b) if constant(defs, b) == Some(1) => Some(a),
        Op::Int(IntOp::Mul, a, b) if constant(defs, a) == Some(1) => Some(b),
        Op::Select { cond, a, b } => {
            if a == b {
                Some(a)
            } else {
                constant(defs, cond).map(|c| if c != 0 { a } else { b })
            }
        }
        _ => None,
    }
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
                },
                if matches!(op, IntUnary::Low32 | IntUnary::High32) {
                    Type::I32
                } else if op == IntUnary::Widen {
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
            remainder,
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
                (if remainder { a % b } else { q }) as i32 as u32 as u128
            } else {
                let q = dividend / u64::from(divisor);
                if q > u64::from(u32::MAX) {
                    return None;
                }
                u128::from(if remainder {
                    dividend % u64::from(divisor)
                } else {
                    q
                })
            }
        }
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

fn validations(func: &mut Func) {
    let mut pending = vec![false; func.blocks.len()];
    loop {
        let mut changed = false;
        for (b, block) in func.blocks.iter().enumerate() {
            let mut p = pending[b];
            for (_, op) in &block.insts {
                p = if matches!(op, Op::Validate | Op::Store { .. } | Op::FrameStore { .. }) {
                    false
                } else {
                    p || op.floating()
                };
            }
            for t in block.end.targets() {
                let to = t.block.0 as usize;
                if func.blocks[to].head.is_none() && p && !pending[to] {
                    pending[to] = true;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let heads: Vec<bool> = func.blocks.iter().map(|b| b.head.is_some()).collect();
    for (b, block) in func.blocks.iter_mut().enumerate() {
        let mut p = pending[b];
        let mut insts = Vec::new();
        for inst in std::mem::take(&mut block.insts) {
            if matches!(inst.1, Op::Store { .. } | Op::FrameStore { .. }) && p {
                insts.push((None, Op::Validate));
                p = false;
            }
            p |= inst.1.floating();
            insts.push(inst);
        }
        if p && (matches!(block.end, End::Exit(_))
            || block.end.targets().any(|t| heads[t.block.0 as usize]))
        {
            insts.push((None, Op::Validate));
        }
        block.insts = insts;
    }
}

/// Moves loads that read invariant memory, and the operations their
/// addresses need, out of loops into block 0. Only blocks that run on every
/// iteration give them up: hoisting from a side path would hold a register
/// across the loop for code that rarely runs. Other operations stay where
/// they are used, so they hold no register across the loop either. Nothing
/// moves when the loads would not fit in registers beside what loop heads
/// carry.
fn hoist(func: &mut Func, proof: &Proof, loads: &BTreeMap<Value, usize>) {
    let every = per_iteration(func);
    let mut available = vec![false; func.types.len()];
    for &p in &func.blocks[0].params {
        available[p.0 as usize] = true;
    }
    for &(r, _) in &func.blocks[0].insts {
        if let Some(r) = r {
            available[r.0 as usize] = true;
        }
    }
    let invariant = |r: Value, op: Op| {
        op.pure()
            || matches!(op, Op::Load { .. })
                && loads.get(&r).is_some_and(|&i| proof.invariant_read(i))
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
                    && invariant(r, op)
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
    loop {
        let mut moved = Vec::new();
        for block in &mut func.blocks[1..] {
            block.insts.retain(|&(r, op)| match r {
                Some(r) if needed[r.0 as usize] && op.uses().all(|v| available[v.0 as usize]) => {
                    moved.push((Some(r), op));
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
}

/// Blocks that run on every iteration of a loop that encloses them: the
/// loop head dominates them and they dominate one of its backedges.
fn per_iteration(func: &Func) -> Vec<bool> {
    let count = func.blocks.len();
    let words = count.div_ceil(64);
    let mut predecessors = vec![Vec::new(); count];
    for (b, data) in func.blocks.iter().enumerate() {
        for t in data.end.targets() {
            predecessors[t.block.0 as usize].push(b);
        }
    }
    let has = |set: &[u64], b: usize| set[b / 64] >> (b % 64) & 1 != 0;
    let mut dominators = vec![vec![u64::MAX; words]; count];
    dominators[0] = vec![0; words];
    dominators[0][0] = 1;
    loop {
        let mut changed = false;
        for b in 1..count {
            let mut set = vec![u64::MAX; words];
            for &p in &predecessors[b] {
                for (s, d) in set.iter_mut().zip(&dominators[p]) {
                    *s &= d;
                }
            }
            set[b / 64] |= 1 << (b % 64);
            if set != dominators[b] {
                dominators[b] = set;
                changed = true;
            }
        }
        if !changed {
            break;
        }
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
        let end = std::mem::replace(&mut func.blocks[b].end, End::Unset);
        func.blocks[a].insts.extend(insts);
        func.blocks[a].end = end;
        reachable(func);
    }
}

fn group_stores(
    func: &mut Func,
    proof: &Proof,
    loads: &BTreeMap<Value, usize>,
    stores: &BTreeMap<RecordId, usize>,
) {
    for block in &mut func.blocks {
        let mut pending = Vec::new();
        let mut output = Vec::new();
        for inst in std::mem::take(&mut block.insts) {
            if matches!(inst.1, Op::Store { .. }) {
                pending.push(inst);
                continue;
            }
            let cross = match inst {
                (Some(r), Op::Load { .. }) => loads.get(&r).is_some_and(|&load| {
                    pending.iter().all(|(_, op)| {
                        let Op::Store { after, .. } = op else {
                            unreachable!()
                        };
                        proof.disjoint_effects(stores[after], load)
                    })
                }),
                (_, Op::Div { .. } | Op::Validate | Op::FrameStore { .. }) => false,
                _ => true,
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
