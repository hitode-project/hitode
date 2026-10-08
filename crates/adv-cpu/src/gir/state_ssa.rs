//! Observation-aware SSA parameters and per-bit flag transport.
use super::ir::*;
use std::collections::BTreeSet;

pub fn edges_mut(t: &mut Terminator) -> Vec<&mut Edge> {
    match t {
        Terminator::Jump(Target::Block(e))
        | Terminator::Call {
            callee: Target::Block(e),
            ..
        } => vec![e],
        Terminator::Branch {
            taken, fallthrough, ..
        } => [taken, fallthrough]
            .into_iter()
            .filter_map(|t| {
                if let Target::Block(e) = t {
                    Some(e)
                } else {
                    None
                }
            })
            .collect(),
        Terminator::Return { matched, .. } => vec![matched],
        Terminator::Guard { passed, .. } => vec![passed],
        _ => vec![],
    }
}
fn state(s: &mut StateMap, f: &mut impl FnMut(&mut ValueId)) {
    for v in &mut s.gpr {
        f(v);
    }
    f(&mut s.flags);
}
fn exit(e: &mut Exit, f: &mut impl FnMut(&mut ValueId)) {
    state(&mut e.state, f);
    f(&mut e.pc);
    f(&mut e.token);
}
pub fn kind(k: &mut ValueKind, f: &mut impl FnMut(&mut ValueId)) {
    match k {
        ValueKind::Flags(v) => {
            f(&mut v.preserved);
            f(&mut v.lhs);
            f(&mut v.rhs);
            f(&mut v.carry);
            f(&mut v.result);
        }
        ValueKind::Pure(p) => match p {
            Pure::Binary(_, a, b) => {
                f(a);
                f(b);
            }
            Pure::Extract { value, .. } | Pure::Extend { value, .. } | Pure::Nonzero(value) => {
                f(value)
            }
            Pure::Insert { base, value, .. } => {
                f(base);
                f(value);
            }
            Pure::Select { condition, yes, no } => {
                f(condition);
                f(yes);
                f(no);
            }
            Pure::Condition { flags, .. } | Pure::FlagBit { flags, .. } => f(flags),
            Pure::FlagPack { bits, rest } => {
                for v in bits {
                    f(v);
                }
                f(rest);
            }
        },
        _ => {}
    }
}
pub fn values_mut(r: &mut Region, mut f: impl FnMut(&mut ValueId)) {
    for v in &mut r.values {
        kind(&mut v.kind, &mut f);
    }
    for b in &mut r.blocks {
        state(&mut b.entry_state, &mut f);
        f(&mut b.entry_token);
        for v in &mut b.parameters {
            f(v);
        }
        for op in &mut b.operations {
            match op {
                Operation::Define(v) => f(v),
                Operation::Memory(_) => {}
                Operation::Commit {
                    state: s, next_pc, ..
                } => {
                    state(s, &mut f);
                    f(next_pc);
                }
            }
        }
        for e in edges_mut(&mut b.terminator) {
            for v in &mut e.arguments {
                f(v);
            }
        }
        let mut target = |t: &mut Target| {
            if let Target::Exit(e) = t {
                exit(e, &mut f);
            }
        };
        match &mut b.terminator {
            Terminator::Jump(t) | Terminator::Call { callee: t, .. } => target(t),
            Terminator::Branch {
                condition,
                taken,
                fallthrough,
            } => {
                target(taken);
                target(fallthrough);
                f(condition);
            }
            Terminator::Return {
                actual, mismatch, ..
            } => {
                f(actual);
                exit(mismatch, &mut f);
            }
            Terminator::Guard {
                condition, token, ..
            } => {
                f(condition);
                f(token);
            }
            Terminator::Exit(e) => exit(e, &mut f),
        }
    }
    for m in &mut r.memory {
        f(&mut m.address);
        f(&mut m.input);
        f(&mut m.output);
        if let Some(v) = &mut m.result {
            f(v);
        }
        if let Access::Write { value, enabled } = &mut m.access {
            f(value);
            if let Some(v) = enabled {
                f(v);
            }
        }
    }
    for s in &mut r.snapshots {
        state(&mut s.state, &mut f);
        for v in &mut s.recipes {
            f(v);
        }
        for slot in &mut s.private_slots {
            f(&mut slot.value);
        }
    }
}
fn define(r: &mut Region, b: usize, width: Width, kind: ValueKind) -> ValueId {
    let id = ValueId(r.values.len());
    r.values.push(Value { width, kind });
    r.blocks[b].operations.push(Operation::Define(id));
    id
}
fn flag_part(r: &mut Region, b: usize, flags: ValueId, bit: Option<u32>) -> ValueId {
    match r.values[flags.0].kind.clone() {
        ValueKind::Flags(f) if bit.is_none_or(|bit| f.defined & bit == 0) => {
            flag_part(r, b, f.preserved, bit)
        }
        ValueKind::Pure(Pure::FlagPack { bits, rest }) => bit.map_or(rest, |bit| {
            bits[FLAG_BITS.iter().position(|&v| v == bit).unwrap()]
        }),
        _ => {
            if let Some(bit) = bit {
                define(
                    r,
                    b,
                    Width::B1,
                    ValueKind::Pure(Pure::FlagBit { flags, bit }),
                )
            } else {
                let mask = define(
                    r,
                    b,
                    Width::B32,
                    ValueKind::Constant(u64::from(!ARITHMETIC)),
                );
                define(
                    r,
                    b,
                    Width::B32,
                    ValueKind::Pure(Pure::Binary(Binary::And, flags, mask)),
                )
            }
        }
    }
}
/// Every commit is a diagnostic budget observation. Consequently all modified
/// architectural fields are live; unchanged fields may use dominating SSA.
/// This is deliberately stricter than overwrite-before-read resume liveness.
pub fn compact(r: &mut Region) {
    let original_entry = r.entry;
    let prologue = BlockId(r.blocks.len());
    let parameters: Vec<_> = (0..10)
        .map(|index| {
            let id = ValueId(r.values.len());
            r.values.push(Value {
                width: if index == 9 { Width::Token } else { Width::B32 },
                kind: ValueKind::Parameter {
                    block: prologue,
                    index,
                },
            });
            id
        })
        .collect();
    let s = StateMap {
        gpr: std::array::from_fn(|n| parameters[n]),
        flags: parameters[8],
    };
    r.blocks.push(Block {
        entry_state: s,
        entry_token: parameters[9],
        inline_depth: 0,
        parameters: parameters.clone(),
        operations: vec![],
        terminator: Terminator::Jump(Target::Block(Edge {
            block: original_entry,
            arguments: parameters,
        })),
    });
    r.entry = prologue;
    for b in 0..prologue.0 {
        let old = r.blocks[b].parameters.clone();
        let mut new = old[..8].to_vec();
        for n in 0..7 {
            let index = new.len();
            let id = ValueId(r.values.len());
            r.values.push(Value {
                width: if n == 6 { Width::B32 } else { Width::B1 },
                kind: ValueKind::Parameter {
                    block: BlockId(b),
                    index,
                },
            });
            new.push(id);
        }
        r.values[old[8].0].kind = ValueKind::Pure(Pure::FlagPack {
            bits: new[8..14].try_into().unwrap(),
            rest: new[14],
        });
        r.blocks[b].operations.insert(0, Operation::Define(old[8]));
        new.push(old[9]);
        r.blocks[b].parameters = new;
    }
    for b in 0..r.blocks.len() {
        let mut term = r.blocks[b].terminator.clone();
        for edge in edges_mut(&mut term) {
            let flags = edge.arguments[8];
            let token = edge.arguments[9];
            let mut args = edge.arguments[..8].to_vec();
            args.extend(FLAG_BITS.map(|bit| flag_part(r, b, flags, Some(bit))));
            args.push(flag_part(r, b, flags, None));
            args.push(token);
            edge.arguments = args;
        }
        r.blocks[b].terminator = term;
    }
    let liveness = observation_liveness(r);
    // Solve parameter provenance to a fixed point. Cyclic unchanged phis
    // collapse to their unique external definition, including loop backedges.
    let mut incoming = vec![vec![]; r.values.len()];
    for b in &r.blocks {
        for edge in b.terminator.edges() {
            for (&param, &arg) in r.blocks[edge.block.0]
                .parameters
                .iter()
                .zip(&edge.arguments)
            {
                incoming[param.0].push(arg);
            }
        }
    }
    let mut sources: Vec<BTreeSet<ValueId>> = r
        .values
        .iter()
        .enumerate()
        .map(|(n, v)| {
            if matches!(v.kind, ValueKind::Parameter { block, .. } if block != r.entry)
                && !incoming[n].is_empty()
            {
                BTreeSet::new()
            } else {
                BTreeSet::from([ValueId(n)])
            }
        })
        .collect();
    loop {
        let mut changed = false;
        for n in 0..r.values.len() {
            if incoming[n].is_empty() || sources[n].len() > 1 {
                continue;
            }
            for arg in &incoming[n] {
                let add = sources[arg.0].clone();
                for v in add {
                    changed |= sources[n].insert(v);
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut reachable = BTreeSet::new();
    let mut pending = vec![r.entry];
    while let Some(b) = pending.pop() {
        if reachable.insert(b) {
            pending.extend(r.blocks[b.0].terminator.edges().iter().map(|e| e.block));
        }
    }
    let mut replace: Vec<_> = (0..r.values.len()).map(ValueId).collect();
    for (n, v) in r.values.iter().enumerate() {
        if matches!(v.kind, ValueKind::Parameter { block, .. } if block != r.entry && reachable.contains(&block) && liveness[block.0].contains(&ValueId(n)))
            && sources[n].len() == 1
        {
            replace[n] = *sources[n].first().unwrap();
        }
    }
    debug_assert!(
        r.blocks
            .iter()
            .enumerate()
            .all(|(b, block)| block.parameters.iter().all(|v| liveness[b].contains(v)))
    );
    let keep: Vec<Vec<bool>> = r
        .blocks
        .iter()
        .map(|b| b.parameters.iter().map(|v| replace[v.0] == *v).collect())
        .collect();
    for b in &mut r.blocks {
        for edge in edges_mut(&mut b.terminator) {
            let mut n = 0;
            edge.arguments.retain(|_| {
                let yes = keep[edge.block.0][n];
                n += 1;
                yes
            });
        }
    }
    for (n, b) in r.blocks.iter_mut().enumerate() {
        let mut p = 0;
        b.parameters.retain(|_| {
            let yes = keep[n][p];
            p += 1;
            yes
        });
    }
    values_mut(r, |v| *v = replace[v.0]);
    let mut dense = vec![ValueId(0); r.values.len()];
    let mut next = 0;
    for n in 0..dense.len() {
        if replace[n] == ValueId(n) {
            dense[n] = ValueId(next);
            next += 1;
        }
    }
    let mut n = 0;
    r.values.retain(|_| {
        let yes = replace[n] == ValueId(n);
        n += 1;
        yes
    });
    values_mut(r, |v| *v = dense[v.0]);
    for (b, block) in r.blocks.iter().enumerate() {
        for (index, &id) in block.parameters.iter().enumerate() {
            r.values[id.0].kind = ValueKind::Parameter {
                block: BlockId(b),
                index,
            };
        }
    }
}

/// Compare lane provenance without materializing an aggregate flags word.
pub fn same_flags(
    r: &Region,
    a: ValueId,
    b: ValueId,
    substitutions: &std::collections::BTreeMap<ValueId, ValueId>,
) -> bool {
    fn part(
        r: &Region,
        mut id: ValueId,
        bit: Option<u32>,
        subs: &std::collections::BTreeMap<ValueId, ValueId>,
        fuel: usize,
    ) -> Option<(ValueId, Option<u32>)> {
        if fuel == 0 {
            return None;
        }
        // Substitution is simultaneous: a loop phi argument can reference the
        // same target parameter from the preceding iteration.
        if let Some(&v) = subs.get(&id) {
            id = v;
            return part(r, id, bit, &Default::default(), fuel - 1);
        }
        match &r.values.get(id.0)?.kind {
            ValueKind::Flags(f) if bit.is_none_or(|b| f.defined & b == 0) => {
                part(r, f.preserved, bit, subs, fuel - 1)
            }
            ValueKind::Pure(Pure::FlagPack { bits, rest }) => {
                let selected = bit.map_or(*rest, |b| {
                    bits[FLAG_BITS.iter().position(|&x| x == b).unwrap()]
                });
                lane(r, selected, bit, subs, fuel - 1)
            }
            _ => Some((id, bit)),
        }
    }
    fn lane(
        r: &Region,
        id: ValueId,
        bit: Option<u32>,
        subs: &std::collections::BTreeMap<ValueId, ValueId>,
        fuel: usize,
    ) -> Option<(ValueId, Option<u32>)> {
        if fuel == 0 {
            return None;
        }
        if let Some(&v) = subs.get(&id) {
            return lane(r, v, bit, &Default::default(), fuel - 1);
        }
        match &r.values.get(id.0)?.kind {
            ValueKind::Pure(Pure::FlagBit { flags, bit }) => {
                part(r, *flags, Some(*bit), subs, fuel - 1)
            }
            ValueKind::Pure(Pure::Binary(Binary::And, flags, mask))
                if bit.is_none()
                    && r.values.get(mask.0)?.kind
                        == ValueKind::Constant(u64::from(!ARITHMETIC)) =>
            {
                part(r, *flags, None, subs, fuel - 1)
            }
            _ => Some((id, None)),
        }
    }
    FLAG_BITS.into_iter().map(Some).chain([None]).all(|bit| {
        let left = part(r, a, bit, substitutions, r.values.len() + 1);
        left.is_some() && left == part(r, b, bit, &Default::default(), r.values.len() + 1)
    })
}

/// Backward SSA liveness with complete commit and pre-operation observations.
/// Effects and edge parameters are roots; pure recipes contribute their inputs
/// only when their result is live. The fixed point includes loop backedges.
pub fn observation_liveness(r: &Region) -> Vec<BTreeSet<ValueId>> {
    let mut live_in = vec![BTreeSet::new(); r.blocks.len()];
    loop {
        let mut changed = false;
        for (n, b) in r.blocks.iter().enumerate().rev() {
            let mut live = BTreeSet::new();
            let mut add_exit = |e: &Exit| {
                live.extend(e.state.values());
                live.extend([e.pc, e.token]);
            };
            match &b.terminator {
                Terminator::Exit(e) => add_exit(e),
                Terminator::Jump(Target::Exit(e))
                | Terminator::Call {
                    callee: Target::Exit(e),
                    ..
                } => add_exit(e),
                Terminator::Branch {
                    condition,
                    taken,
                    fallthrough,
                } => {
                    for t in [taken, fallthrough] {
                        if let Target::Exit(e) = t {
                            add_exit(e);
                        }
                    }
                    live.insert(*condition);
                }
                Terminator::Return {
                    actual, mismatch, ..
                } => {
                    add_exit(mismatch);
                    live.insert(*actual);
                }
                Terminator::Guard {
                    condition,
                    failed,
                    token,
                    ..
                } => {
                    live.extend(r.snapshots[failed.0].state.values());
                    live.extend([*condition, *token]);
                }
                _ => {}
            }
            for e in b.terminator.edges() {
                let target = &r.blocks[e.block.0];
                for &v in &live_in[e.block.0] {
                    if let Some(index) = target.parameters.iter().position(|&p| p == v) {
                        live.insert(e.arguments[index]);
                    } else {
                        live.insert(v);
                    }
                }
                live.insert(b.entry_token);
            }
            for op in b.operations.iter().rev() {
                match op {
                    Operation::Commit {
                        point,
                        state,
                        next_pc,
                    } => {
                        live.extend(state.values());
                        live.insert(*next_pc);
                        for s in r.snapshots.iter().filter(|s| s.commit == *point) {
                            live.extend(s.state.values());
                        }
                    }
                    Operation::Define(v) => {
                        if live.remove(v) {
                            live.extend(r.values[v.0].kind.inputs());
                        }
                    }
                    Operation::Memory(id) => {
                        let m = &r.memory[id.0];
                        live.remove(&m.output);
                        if let Some(v) = m.result {
                            live.remove(&v);
                        }
                        live.extend([m.input, m.address]);
                        live.extend(r.snapshots[m.snapshot.0].state.values());
                        if let Access::Write { value, enabled } = m.access {
                            live.insert(value);
                            live.extend(enabled);
                        }
                    }
                }
            }
            if live != live_in[n] {
                live_in[n] = live;
                changed = true;
            }
        }
        if !changed {
            return live_in;
        }
    }
}
