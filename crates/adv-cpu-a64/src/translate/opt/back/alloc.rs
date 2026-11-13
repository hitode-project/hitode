//! Block-order linear scan. Record reads extend lifetimes to every possible
//! recovery, but do not by themselves earn a register.

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
    let mut starts = vec![usize::MAX; n];
    let mut ends = vec![0; n];
    let mut hot = vec![false; n];
    let mut block_start = Vec::new();
    let mut block_end = Vec::new();
    let mut uses = vec![BTreeSet::new(); func.blocks.len()];
    let mut defs = uses.clone();
    let mut position = 0;
    for (b, data) in func.blocks.iter().enumerate() {
        block_start.push(position);
        let mut region = regions[b];
        for &p in &data.params {
            starts[p.0 as usize] = position;
            ends[p.0 as usize] = position;
            defs[b].insert(p);
        }
        let mut read = |v: Value, pos: usize, warm: bool, immediate: bool| {
            let mut leaves = Vec::new();
            super::super::recovery::leaves_of(v, &operations, immediate, &mut leaves);
            for (v, immediate) in leaves {
                let i = v.0 as usize;
                ends[i] = ends[i].max(pos);
                hot[i] |= warm && !(immediate && constants[i].is_some());
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
            if !matches!(op, Op::Flags { kind, .. } if *kind != FlagKind::UnorderedCompare) {
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
        uses[b].retain(|v| !defs[b].contains(v));
        block_end.push(position);
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
    let mut active: Vec<(usize, Host)> = Vec::new();
    let mut locations = vec![Host::Constant(0); n];
    let mut slots = 0;
    for i in order {
        active.retain(|&(end, _)| end >= starts[i]);
        if matches!(operations[i], Some(Op::Flags { .. })) {
            locations[i] = Host::Flags(i as u32);
            continue;
        }
        if let Some(bits) = constants[i]
            && !hot[i]
        {
            locations[i] = Host::Constant(bits);
            continue;
        }
        let vector = matches!(func.types[i], Type::F64 | Type::V128);
        let available = if hot[i] || operations[i].is_some() {
            let preferences = hints[i]
                .iter()
                .filter_map(|v| match locations[v.0 as usize] {
                    Host::Register(r) if !vector => Some(r.number() as u8),
                    Host::Vector(r) if vector => Some(r.number() as u8),
                    _ => None,
                });
            preferences
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
                    (!active.iter().any(|&(_, held)| held == loc)).then_some(loc)
                })
        } else {
            None
        };
        let loc = available.unwrap_or_else(|| {
            let loc = Host::Spill(slots, func.types[i]);
            slots += 1;
            loc
        });
        locations[i] = loc;
        if available.is_some() {
            active.push((ends[i], loc));
        }
    }
    Allocation {
        locations,
        regions,
        spill_cells: slots as usize,
        defs: operations,
    }
}
