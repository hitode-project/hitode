//! Bounded profile-guided grouping of immutable native decode snapshots.
//! Heat is an admission hint only; every selected page is validated by its owner.
use iced_x86::{FlowControl, Instruction};
use std::collections::{BTreeMap, BTreeSet};

pub const HOT: u64 = 32;
pub const MAX_REGIONS: usize = 16;
pub const MAX_INSTRUCTIONS: usize = 1024;
pub const MAX_CODE_BYTES: usize = 2 * 1024 * 1024;

pub struct Candidate {
    pub entry: u32,
    pub heat: u64,
    pub instructions: BTreeMap<u32, Instruction>,
    pub pages: Vec<(usize, u64)>,
}

pub struct Unit {
    pub entry: u32,
    pub instructions: BTreeMap<u32, Instruction>,
}

pub struct Selection {
    pub units: Vec<Unit>,
    pub pages: Vec<(usize, u64)>,
    pub merged: usize,
}

/// A deterministic Kosaraju pass. The graph is bounded before recursion.
fn components(edges: &[BTreeSet<usize>]) -> Vec<Vec<usize>> {
    fn visit(
        n: usize,
        graph: &[BTreeSet<usize>],
        seen: &mut BTreeSet<usize>,
        out: &mut Vec<usize>,
    ) {
        if seen.insert(n) {
            for &next in &graph[n] {
                visit(next, graph, seen, out);
            }
            out.push(n);
        }
    }
    let mut order = Vec::new();
    let mut seen = BTreeSet::new();
    let mut reverse = vec![BTreeSet::new(); edges.len()];
    for (n, next) in edges.iter().enumerate() {
        visit(n, edges, &mut seen, &mut order);
        for &to in next {
            reverse[to].insert(n);
        }
    }
    seen.clear();
    let mut groups = Vec::new();
    for n in order.into_iter().rev() {
        if seen.contains(&n) {
            continue;
        }
        let mut group = Vec::new();
        visit(n, &reverse, &mut seen, &mut group);
        groups.push(group);
    }
    groups
}

pub fn select(mut candidates: Vec<Candidate>, root: u32) -> Result<Selection, String> {
    candidates.retain(|c| c.entry == root || c.heat >= HOT);
    candidates.sort_by_key(|c| (c.entry != root, std::cmp::Reverse(c.heat), c.entry));
    candidates.truncate(MAX_REGIONS);
    if candidates.first().is_none_or(|c| c.entry != root) {
        return Err("missing GIR profile root".into());
    }
    let mut count = 0;
    candidates.retain(|c| {
        if count + c.instructions.len() > MAX_INSTRUCTIONS {
            return false;
        }
        count += c.instructions.len();
        true
    });
    if candidates.first().is_none_or(|c| c.entry != root) {
        return Err("GIR root exceeds instruction budget".into());
    }
    let entries: BTreeMap<_, _> = candidates
        .iter()
        .enumerate()
        .map(|(n, c)| (c.entry, n))
        .collect();
    let mut edges = vec![BTreeSet::new(); candidates.len()];
    for (n, c) in candidates.iter().enumerate() {
        for i in c.instructions.values() {
            if matches!(
                i.flow_control(),
                FlowControl::Call
                    | FlowControl::UnconditionalBranch
                    | FlowControl::ConditionalBranch
            ) && let Some(&to) = entries.get(&i.near_branch32())
            {
                edges[n].insert(to);
                // A return hint admits code, never predicts execution: lift
                // retains the guest stack read and actual-target comparison.
                if i.flow_control() == FlowControl::Call {
                    edges[to].insert(n);
                }
            }
            if matches!(
                i.flow_control(),
                FlowControl::Next | FlowControl::ConditionalBranch
            ) && let Some(&to) = entries.get(&i.next_ip32())
            {
                edges[n].insert(to);
            }
        }
    }
    // Unrelated hot regions must not increase this family's code or lease set.
    let mut reachable = BTreeSet::new();
    let mut pending = vec![0];
    while let Some(n) = pending.pop() {
        if reachable.insert(n) {
            pending.extend(edges[n].iter().copied());
        }
    }
    let mut units = Vec::new();
    let mut pages = BTreeMap::new();
    let mut merged = 0;
    for mut group in components(&edges) {
        group.retain(|n| reachable.contains(n));
        if group.is_empty() {
            continue;
        }
        group.sort_unstable();
        merged += group.len() - 1;
        let entry = candidates[group[0]].entry;
        let mut instructions = BTreeMap::new();
        for n in group {
            let c = &candidates[n];
            for (&pc, i) in &c.instructions {
                if let Some(previous) = instructions.insert(pc, *i)
                    && previous != *i
                {
                    return Err("inconsistent GIR decode snapshots".into());
                }
            }
            for &(page, version) in &c.pages {
                if let Some(previous) = pages.insert(page, version)
                    && previous != version
                {
                    return Err("inconsistent GIR code generations".into());
                }
            }
        }
        units.push(Unit {
            entry,
            instructions,
        });
    }
    units.sort_by_key(|u| (u.entry != root, u.entry));
    Ok(Selection {
        units,
        pages: pages.into_iter().collect(),
        merged,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::{Decoder, DecoderOptions, code_asm::*};

    fn candidate(entry: u32, heat: u64, f: impl FnOnce(&mut CodeAssembler)) -> Candidate {
        let mut a = CodeAssembler::new(32).unwrap();
        f(&mut a);
        let bytes = a.assemble(u64::from(entry)).unwrap();
        Candidate {
            entry,
            heat,
            instructions: Decoder::with_ip(32, &bytes, u64::from(entry), DecoderOptions::NONE)
                .into_iter()
                .map(|i| (i.ip32(), i))
                .collect(),
            pages: vec![(entry as usize / 4096, 1)],
        }
    }

    #[test]
    fn hot_calls_merge_with_checked_returns_and_cold_unrelated_code_is_excluded() {
        let a = 0x10000;
        let b = a + 0x1000;
        let c = b + 0x1000;
        let selection = select(
            vec![
                candidate(a, HOT, |asm| {
                    asm.call(u64::from(b)).unwrap();
                    asm.jmp(u64::from(a)).unwrap();
                }),
                candidate(b, HOT, |asm| {
                    asm.inc(eax).unwrap();
                    asm.ret().unwrap();
                }),
                candidate(c, HOT * 2, |asm| {
                    asm.ret().unwrap();
                }),
            ],
            a,
        )
        .unwrap();
        assert_eq!(selection.units.len(), 1);
        assert_eq!(selection.merged, 1);
        assert_eq!(selection.pages.len(), 2);
        let lifted = crate::gir::lift::lift(&selection.units[0].instructions, a).unwrap();
        assert!(lifted.inlined_calls > 0);
        assert!(
            lifted
                .region
                .blocks
                .iter()
                .any(|b| matches!(b.terminator, crate::gir::ir::Terminator::Return { .. }))
        );
        let selection = select(
            vec![
                candidate(a, HOT, |asm| {
                    asm.jmp(u64::from(b)).unwrap();
                }),
                candidate(b, HOT - 1, |asm| {
                    asm.jmp(u64::from(a)).unwrap();
                }),
            ],
            a,
        )
        .unwrap();
        assert_eq!(selection.merged, 0);
        assert_eq!(selection.pages.len(), 1);
    }

    #[test]
    fn hot_acyclic_edges_use_distinct_tail_units_and_conflicting_pages_are_rejected() {
        let a = 0x10000;
        let b = a + 0x100;
        let make = || {
            vec![
                candidate(a, HOT, |asm| {
                    asm.jmp(u64::from(b)).unwrap();
                }),
                candidate(b, HOT, |asm| {
                    asm.inc(eax).unwrap();
                }),
            ]
        };
        let selection = select(make(), a).unwrap();
        assert_eq!(selection.units.len(), 2);
        assert_eq!(selection.units[0].entry, a);
        let mut conflicting = make();
        conflicting[1].pages[0].1 += 1;
        assert!(select(conflicting, a).is_err());
    }

    #[test]
    fn nested_cycles_and_disconnected_nodes_are_separate() {
        let edges: Vec<BTreeSet<usize>> = [vec![1], vec![0, 2], vec![3], vec![2], vec![]]
            .into_iter()
            .map(|e| e.into_iter().collect())
            .collect();
        let mut result = components(&edges);
        for group in &mut result {
            group.sort_unstable();
        }
        result.sort();
        assert_eq!(result, [vec![0, 1], vec![2, 3], vec![4]]);
    }
}
