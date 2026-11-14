//! Block-order linear scan. Record reads extend lifetimes to every possible
//! recovery, but do not by themselves earn a register. Each value weighs its
//! definition and executed uses, eight times more per enclosing loop; without
//! a free register, the lightest value is spilled, the one that ends last
//! among equals, so values that inner loops use keep their registers.
//! Positions follow reverse postorder, which keeps side paths and exits next
//! to their branches. Integer constants come last: one takes a register
//! that stays free for its whole life, or is rebuilt at its uses. Spill
//! cells are reused once their values are dead.

use super::*;
use std::collections::BTreeSet;

pub(super) struct Allocation {
    pub locations: Vec<Host>,
    pub regions: Vec<RecordId>,
    pub spill_cells: usize,
    pub defs: Vec<Option<Op>>,
}

// spec: A64-OPT-6, A64-OPT-7
pub(super) fn allocate(func: &Func) -> Allocation {
    let n = func.types.len();
    let mut operations = vec![None; n];
    let mut constants = vec![None; n];
    let mut hints = vec![Vec::new(); n];
    for data in &func.blocks {
        for &(result, op) in &data.insts {
            if let Some(v) = result {
                operations[v.0 as usize] = Some(op);
                if let Op::Constant(_, bits) = op {
                    constants[v.0 as usize] = Some(bits);
                }
            }
        }
    }
    // Backedges get first choice, in both directions: the argument may be
    // allocated later than its parameter and can reuse its dying register.
    for backedge in [true, false] {
        for (b, data) in func.blocks.iter().enumerate() {
            for target in data
                .end
                .targets()
                .filter(|t| (t.block.0 as usize <= b) == backedge)
            {
                for (&p, &a) in func.blocks[target.block.0 as usize]
                    .params
                    .iter()
                    .zip(&target.args)
                {
                    hints[p.0 as usize].push(a);
                    hints[a.0 as usize].push(p);
                }
            }
        }
    }
    let mut regions = vec![None; func.blocks.len()];
    regions[0] = Some(func.blocks[0].head.unwrap().record);
    let mut work = vec![0];
    while let Some(b) = work.pop() {
        let data = &func.blocks[b];
        let mut region = data.head.map_or(regions[b].unwrap(), |h| h.record);
        regions[b] = Some(region);
        for (_, op) in &data.insts {
            if let Op::Store { after, .. } = op {
                region = *after;
            }
        }
        for target in data.end.targets() {
            let t = target.block.0 as usize;
            if regions[t].is_none() {
                regions[t] = Some(func.blocks[t].head.map_or(region, |h| h.record));
                work.push(t);
            }
        }
    }
    let regions: Vec<_> = regions
        .into_iter()
        .map(|r| r.unwrap_or(RecordId(0)))
        .collect();
    let depth = loop_depth(func);
    let mut weight = vec![0u64; n];
    let mut starts = vec![usize::MAX; n];
    let mut ends = vec![0; n];
    let mut hot = vec![false; n];
    let mut block_start = vec![0; func.blocks.len()];
    let mut block_end = vec![0; func.blocks.len()];
    let mut uses = vec![BTreeSet::new(); func.blocks.len()];
    let mut defs = uses.clone();
    let mut position = 0;
    for b in sequence(func) {
        let data = &func.blocks[b];
        block_start[b] = position;
        let mut region = regions[b];
        let scale = 8u64.pow(depth[b].min(6));
        for &p in &data.params {
            starts[p.0 as usize] = position;
            ends[p.0 as usize] = position;
            weight[p.0 as usize] += scale;
            defs[b].insert(p);
        }
        let mut read = |v: Value, pos: usize, warm: bool, immediate: bool| {
            let mut leaves = Vec::new();
            super::super::recovery::leaves_of(v, &operations, immediate, &mut leaves);
            for (v, immediate) in leaves {
                let i = v.0 as usize;
                ends[i] = ends[i].max(pos);
                hot[i] |= warm && !(immediate && constants[i].is_some());
                weight[i] += if warm { scale } else { 0 };
                if !defs[b].contains(&v) {
                    uses[b].insert(v);
                }
            }
        };
        for v in func.records[region.0 as usize].uses() {
            read(v, position, false, false);
        }
        for (result, op) in &data.insts {
            position += 2;
            if !matches!(op, Op::Flags { kind, .. } if !kind.floating()) {
                for v in op.uses() {
                    read(
                        v,
                        position,
                        true,
                        constants[v.0 as usize].is_some_and(|bits| matches!(*op, Op::Int(kind, _, b) if b == v && int_immediate(kind, if func.ty(v) == Type::I64 { X } else { W }, T, T, bits as u64).is_some())),
                    );
                }
            }
            if op.can_fail() {
                for v in func.records[region.0 as usize].uses() {
                    read(v, position + 1, false, false);
                }
            }
            if let Op::Store { after, .. } = op {
                region = *after;
            }
            // Definitions are added below, after collecting the block's uses.
            if let Some(v) = result {
                starts[v.0 as usize] = position + 1;
            }
        }
        position += 2;
        for target in data.end.targets() {
            for &v in &target.args {
                read(v, position, true, false);
            }
        }
        match data.end {
            End::Branch { cond, .. } => {
                read(cond, position, true, false);
                if let Some(Op::Test { flags, .. }) = operations[cond.0 as usize] {
                    read(flags, position, true, false);
                }
            }
            End::Exit(id) => {
                for v in func.records[id.0 as usize].uses() {
                    read(v, position, false, false);
                }
            }
            _ => {}
        }
        // SSA local definitions never belong to live-in, even if used later.
        defs[b].extend(data.insts.iter().filter_map(|(r, _)| *r));
        for v in data.insts.iter().filter_map(|(r, _)| *r) {
            weight[v.0 as usize] += scale;
        }
        uses[b].retain(|v| !defs[b].contains(v));
        block_end[b] = position;
        position += 1;
    }
    let mut live_in = uses.clone();
    let mut live_out = vec![BTreeSet::new(); func.blocks.len()];
    loop {
        let mut changed = false;
        for b in (0..func.blocks.len()).rev() {
            let out: BTreeSet<_> = func.blocks[b]
                .end
                .targets()
                .flat_map(|t| live_in[t.block.0 as usize].iter().copied())
                .collect();
            let incoming: BTreeSet<_> = uses[b]
                .union(&out.difference(&defs[b]).copied().collect())
                .copied()
                .collect();
            changed |= incoming != live_in[b] || out != live_out[b];
            live_in[b] = incoming;
            live_out[b] = out;
        }
        if !changed {
            break;
        }
    }
    for b in 0..func.blocks.len() {
        for &v in &live_in[b] {
            starts[v.0 as usize] = starts[v.0 as usize].min(block_start[b]);
            ends[v.0 as usize] = ends[v.0 as usize].max(block_start[b]);
        }
        for &v in &live_out[b] {
            ends[v.0 as usize] = ends[v.0 as usize].max(block_end[b]);
        }
    }
    let mut order: Vec<_> = (0..n).collect();
    order.sort_by_key(|&i| starts[i]);
    // Registers held until an end, and spill cells free from an end on.
    let mut active: Vec<(usize, Host, usize)> = Vec::new();
    let mut cells: Vec<(usize, u32)> = Vec::new();
    let mut locations = vec![Host::Constant(0); n];
    let mut slots = 0;
    let mut spill = |i: usize, cells: &mut Vec<(usize, u32)>| {
        let cell = match cells.iter().position(|&(end, _)| end < starts[i]) {
            Some(k) => cells.swap_remove(k).1,
            None => {
                slots += 1;
                slots - 1
            }
        };
        cells.push((ends[i], cell));
        Host::Spill(cell, func.types[i])
    };
    for i in order {
        active.retain(|&(end, ..)| end >= starts[i]);
        if matches!(operations[i], Some(Op::Flags { .. })) {
            locations[i] = Host::Flags(i as u32);
            continue;
        }
        if let Some(bits) = constants[i]
            && (!hot[i] || matches!(func.types[i], Type::I32 | Type::I64))
        {
            locations[i] = Host::Constant(bits);
            continue;
        }
        let vector = matches!(func.types[i], Type::F64 | Type::V128);
        let class = |loc: Host| matches!(loc, Host::Vector(_)) == vector;
        let preferences = hints[i]
            .iter()
            .filter_map(|v| match locations[v.0 as usize] {
                Host::Register(r) if !vector => Some(r.number() as u8),
                Host::Vector(r) if vector => Some(r.number() as u8),
                _ => None,
            });
        let free = preferences
            .chain(0..if vector { 29 } else { 31 })
            .find_map(|reg| {
                if !vector && [9, 16, 17, 18, 28, 29].contains(&reg) {
                    return None;
                }
                let loc = if vector {
                    Host::Vector(VReg::new(reg))
                } else {
                    Host::Register(Reg::new(reg))
                };
                (!active.iter().any(|&(_, held, _)| held == loc)).then_some(loc)
            });
        if let Some(loc) = free {
            locations[i] = loc;
            active.push((ends[i], loc, i));
            continue;
        }
        // Without a free register, the lighter of the lightest held value
        // and this one keeps a spill cell for the rest of its life.
        let victim = active
            .iter()
            .enumerate()
            .filter(|&(_, &(_, held, _))| class(held))
            .min_by_key(|&(_, &(end, _, held))| (weight[held], std::cmp::Reverse(end)))
            .map(|(k, _)| k);
        match victim {
            Some(k) if (weight[active[k].2], ends[i]) < (weight[i], active[k].0) => {
                let (_, loc, held) = active.swap_remove(k);
                locations[held] = spill(held, &mut cells);
                locations[i] = loc;
                active.push((ends[i], loc, i));
            }
            _ => locations[i] = spill(i, &mut cells),
        }
    }
    // An integer constant takes a register only where one stays free for its
    // whole life; elsewhere it is rebuilt at its uses and never displaces a
    // variable.
    let mut held: Vec<Vec<(usize, usize)>> = vec![Vec::new(); 32];
    for (i, loc) in locations.iter().enumerate() {
        if let Host::Register(r) = loc {
            held[r.number() as usize].push((starts[i], ends[i]));
        }
    }
    let mut wanted: Vec<usize> = (0..n)
        .filter(|&i| {
            hot[i]
                && constants[i].is_some()
                && matches!(func.types[i], Type::I32 | Type::I64)
                && starts[i] <= ends[i]
        })
        .collect();
    wanted.sort_by_key(|&i| std::cmp::Reverse(weight[i]));
    for i in wanted {
        let free = (0..31u8)
            .filter(|r| ![9, 16, 17, 18, 28, 29].contains(r))
            .find(|&r| {
                held[usize::from(r)]
                    .iter()
                    .all(|&(start, end)| end < starts[i] || ends[i] < start)
            });
        if let Some(r) = free {
            held[usize::from(r)].push((starts[i], ends[i]));
            locations[i] = Host::Register(Reg::new(r));
        }
    }
    Allocation {
        locations,
        regions,
        spill_cells: slots as usize,
        defs: operations,
    }
}

/// Blocks in reverse postorder from block 0, the order positions follow:
/// a branch's side paths and exits come right after it, ahead of the code
/// that follows, so values they read are not held across unrelated code.
fn sequence(func: &Func) -> Vec<usize> {
    let count = func.blocks.len();
    let mut seen = vec![false; count];
    let mut post = Vec::with_capacity(count);
    let mut stack = vec![(0, false)];
    while let Some((b, done)) = stack.pop() {
        if done {
            post.push(b);
            continue;
        }
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        stack.push((b, true));
        // The earliest target is explored first, so it finishes first and
        // comes after the others: those are the side paths and exits.
        let mut next: Vec<usize> = func.blocks[b]
            .end
            .targets()
            .map(|t| t.block.0 as usize)
            .filter(|&t| !seen[t])
            .collect();
        next.sort_unstable_by(|x, y| y.cmp(x));
        stack.extend(next.into_iter().map(|t| (t, false)));
    }
    post.reverse();
    post
}

/// How many loops enclose each block. A backedge goes to a block no later
/// in block order; its loop is what reaches the edge without passing the
/// target, unless that includes block 0, as when an appended path rejoins.
fn loop_depth(func: &Func) -> Vec<u32> {
    let count = func.blocks.len();
    let mut predecessors = vec![Vec::new(); count];
    for (b, data) in func.blocks.iter().enumerate() {
        for t in data.end.targets() {
            predecessors[t.block.0 as usize].push(b);
        }
    }
    let mut loops = std::collections::BTreeMap::<usize, Vec<bool>>::new();
    for (source, data) in func.blocks.iter().enumerate() {
        for head in data.end.targets().map(|t| t.block.0 as usize) {
            if head > source || head == 0 {
                continue;
            }
            let mut body = vec![false; count];
            body[head] = true;
            let mut work = vec![source];
            while let Some(b) = work.pop() {
                if !std::mem::replace(&mut body[b], true) {
                    work.extend(&predecessors[b]);
                }
            }
            if body[0] {
                continue;
            }
            let merged = loops.entry(head).or_insert_with(|| vec![false; count]);
            for (m, b) in merged.iter_mut().zip(body) {
                *m |= b;
            }
        }
    }
    (0..count)
        .map(|b| loops.values().filter(|body| body[b]).count() as u32)
        .collect()
}
