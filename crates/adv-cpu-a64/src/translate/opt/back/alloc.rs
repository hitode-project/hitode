//! Block-order linear scan. Record reads extend lifetimes to every possible
//! recovery, but do not by themselves earn a register.

use super::*;
use std::collections::BTreeSet;

pub(super) struct Allocation {
    pub locations: Vec<Host>,
    pub regions: Vec<RecordId>,
    pub spill_cells: usize,
}

// spec: A64-OPT-6, A64-OPT-7
pub(super) fn allocate(func: &Func) -> Allocation {
    let n = func.types.len();
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
    let mut constants = vec![None; n];
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
        let mut read = |v: Value, pos: usize, warm: bool| {
            let i = v.0 as usize;
            ends[i] = ends[i].max(pos);
            hot[i] |= warm;
            if !defs[b].contains(&v) {
                uses[b].insert(v);
            }
        };
        for v in func.records[region.0 as usize].uses() {
            read(v, position, false);
        }
        for (result, op) in &data.insts {
            position += 1;
            for v in op.uses() {
                read(v, position, true);
            }
            if op.can_fail() {
                for v in func.records[region.0 as usize].uses() {
                    read(v, position, false);
                }
            }
            if let Op::Store { after, .. } = op {
                region = *after;
            }
            // Definitions are added below, after collecting the block's uses.
            if let Some(v) = result {
                starts[v.0 as usize] = position;
                if let Op::Constant(_, bits) = op {
                    constants[v.0 as usize] = Some(*bits);
                }
            }
        }
        position += 1;
        for target in data.end.targets() {
            for &v in &target.args {
                read(v, position, true);
            }
        }
        match data.end {
            End::Branch { cond, .. } => read(cond, position, true),
            End::Exit(id) => {
                for v in func.records[id.0 as usize].uses() {
                    read(v, position, false);
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
    let mut locations = vec![Host::Spill(0); n];
    let mut slots = 0;
    for i in order {
        active.retain(|&(end, _)| end >= starts[i]);
        if let Some(bits) = constants[i] {
            locations[i] = Host::Constant(bits);
            continue;
        }
        let vector = matches!(func.types[i], Type::F64 | Type::V128);
        let available = if hot[i] {
            (0..if vector { 29 } else { 31 }).find_map(|reg| {
                if !vector && [9, 16, 17, 18, 28, 29].contains(&reg) {
                    return None;
                }
                let loc = if vector {
                    Host::Vector(reg)
                } else {
                    Host::Register(reg)
                };
                (!active.iter().any(|&(_, held)| held == loc)).then_some(loc)
            })
        } else {
            None
        };
        let loc = available.unwrap_or_else(|| {
            let loc = Host::Spill(slots);
            slots += 1;
            loc
        });
        locations[i] = loc;
        if available.is_some() {
            active.push((ends[i], loc));
        }
    }
    let copies = func
        .blocks
        .iter()
        .map(|b| b.params.len())
        .max()
        .unwrap_or(0);
    Allocation {
        locations,
        regions,
        spill_cells: slots as usize + copies,
    }
}
