//! Propagation removes frame traffic and mode tests; liveness keeps only values
//! read by execution or recovery. Floating operations remain until validation.
use super::*;
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
    loop {
        let mut changed = propagate(func);
        changed |= reachable(func);
        if !changed {
            break;
        }
    }
    hoist(func, proof, loads);
    while propagate(func) {}
    join(func);
    group_stores(func, proof, loads, stores);
    prune_records(func);
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
    let f = |v: &mut Value| *v = resolve(*v, aliases);
    for loc in record
        .regs
        .iter_mut()
        .chain([&mut record.eflags])
        .chain(record.xmm.iter_mut())
        .chain(record.x87.stack.iter_mut())
        .chain([
            &mut record.x87.c1,
            &mut record.x87.ip,
            &mut record.x87.opcode,
        ])
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
            if !op.can_fail() && !op.floating() || block_index == 0 && matches!(op, Op::Load { .. })
            {
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

fn identity(op: Op, defs: &[Option<Op>]) -> Option<Value> {
    match op {
        Op::Int(
            IntOp::Add | IntOp::Sub | IntOp::Or | IntOp::Xor | IntOp::Shl | IntOp::Shr | IntOp::Sar,
            a,
            b,
        ) if constant(defs, b) == Some(0) => Some(a),
        Op::Int(IntOp::Mul, a, b) if constant(defs, b) == Some(1) => Some(a),
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
            if op.can_fail() || op.floating() {
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
        block.insts.retain(|(r, op)| {
            r.is_some_and(|r| live[r.0 as usize]) || op.can_fail() || op.floating()
        });
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
                p = if matches!(op, Op::Validate | Op::Store { .. }) {
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
            if matches!(inst.1, Op::Store { .. }) && p {
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

fn prune_records(func: &mut Func) {
    use std::collections::BTreeSet;
    let mut incoming = vec![BTreeSet::<Value>::new(); func.blocks.len()];
    let mut after = vec![Vec::new(); func.blocks.len()];
    loop {
        let mut changed = false;
        for (b, block) in func.blocks.iter().enumerate().rev() {
            let mut live = BTreeSet::new();
            for t in block.end.targets() {
                for &v in &incoming[t.block.0 as usize] {
                    if let Some(i) = func.blocks[t.block.0 as usize]
                        .params
                        .iter()
                        .position(|&p| p == v)
                    {
                        live.insert(t.args[i]);
                    } else {
                        live.insert(v);
                    }
                }
            }
            match block.end {
                End::Branch { cond, .. } => {
                    live.insert(cond);
                }
                End::Exit(r) => live.extend(func.records[r.0 as usize].uses()),
                _ => {}
            }
            let mut positions = vec![BTreeSet::new(); block.insts.len()];
            for (i, (result, op)) in block.insts.iter().enumerate().rev() {
                positions[i] = live.clone();
                let used = result.is_some_and(|r| live.remove(&r));
                if used || op.can_fail() || op.floating() {
                    live.extend(op.uses());
                }
            }
            if let Some(head) = block.head {
                live.extend(func.records[head.record.0 as usize].uses());
            }
            if incoming[b] != live {
                incoming[b] = live;
                changed = true;
            }
            after[b] = positions;
        }
        if !changed {
            break;
        }
    }
    for (b, block) in func.blocks.iter().enumerate() {
        for (i, (_, op)) in block.insts.iter().enumerate() {
            let Op::Store { after: id, .. } = op else {
                continue;
            };
            let record = &mut func.records[id.0 as usize];
            let live = &after[b][i];
            let keep = |loc: &Option<Location<Value>>| {
                loc.is_some_and(|loc| match loc {
                    Location::At(v) | Location::Recipe(Recipe::Eflags(v)) => live.contains(&v),
                    Location::Recipe(Recipe::C1 { a, b, .. }) => {
                        live.contains(&a) || live.contains(&b)
                    }
                    Location::Constant(_) => true,
                })
            };
            for loc in record
                .regs
                .iter_mut()
                .chain([&mut record.eflags])
                .chain(record.xmm.iter_mut())
                .chain(record.x87.stack.iter_mut())
            {
                if !keep(loc) {
                    *loc = None;
                }
            }
        }
    }
}

fn hoist(func: &mut Func, proof: &Proof, loads: &BTreeMap<Value, usize>) {
    let mut available = vec![false; func.types.len()];
    for &p in &func.blocks[0].params {
        available[p.0 as usize] = true;
    }
    for (r, _) in &func.blocks[0].insts {
        if let Some(r) = r {
            available[r.0 as usize] = true;
        }
    }
    loop {
        let mut moved = Vec::new();
        for block in &mut func.blocks[1..] {
            block.insts.retain(|&(r, op)| {
                let Some(r) = r else {
                    return true;
                };
                let invariant = !op.can_fail() && !op.floating()
                    || matches!(op, Op::Load { .. })
                        && loads.get(&r).is_some_and(|&i| proof.invariant_read(i));
                if invariant && op.uses().all(|v| available[v.0 as usize]) {
                    moved.push((Some(r), op));
                    available[r.0 as usize] = true;
                    false
                } else {
                    true
                }
            });
        }
        if moved.is_empty() {
            break;
        }
        func.blocks[0].insts.extend(moved);
    }
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
                (_, Op::Div { .. } | Op::Validate) => false,
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
