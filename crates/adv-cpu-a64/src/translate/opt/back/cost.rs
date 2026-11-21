//! Static resident loop costs for the owner's probation decision.

use super::{Block, Func};

#[derive(Clone, Copy, Default)]
pub(super) struct BlockCost {
    pub body: u32,
    pub edges: [u32; 2],
}

#[derive(Default)]
pub(super) struct Iteration {
    pub words: u32,
    pub retired: u32,
    pub pcs: Vec<u32>,
}

#[derive(Clone, Copy)]
struct Path {
    words: u32,
    retired: i64,
    next: Block,
}

pub(super) fn iteration(func: &Func, costs: &[BlockCost]) -> Iteration {
    let Some(head) = func.head_exit else {
        return Iteration::default();
    };
    // Estimate the longest path back to this head, omitting cycles at
    // other heads. Keep the guest instructions and charges on that path.
    let mut paths = vec![None; func.blocks.len()];
    let Some(total) = longest(func, costs, &mut paths, head, head) else {
        return Iteration::default();
    };
    let mut pcs = Vec::new();
    let mut block = head;
    loop {
        let i = block.0 as usize;
        pcs.extend_from_slice(&func.blocks[i].pcs);
        block = paths[i].flatten().unwrap().next;
        if block == head {
            break;
        }
    }
    Iteration {
        words: total.words,
        retired: total.retired as u32,
        pcs,
    }
}

fn longest(
    func: &Func,
    costs: &[BlockCost],
    paths: &mut [Option<Option<Path>>],
    block: Block,
    head: Block,
) -> Option<Path> {
    let b = block.0 as usize;
    if let Some(path) = paths[b] {
        return path;
    }
    paths[b] = Some(None);
    let cost = costs[b];
    let best = func.blocks[b]
        .end
        .targets()
        .enumerate()
        .filter_map(|(i, t)| {
            let (words, retired) = if t.block == head {
                (0, 0)
            } else {
                let rest = longest(func, costs, paths, t.block, head)?;
                (rest.words, rest.retired)
            };
            Some(Path {
                words: cost.body + cost.edges[i] + words,
                retired: i64::from(t.charge) + retired,
                next: t.block,
            })
        })
        .max_by_key(|path| path.words);
    paths[b] = Some(best);
    best
}

/// The cheapest returning path pays for the check and the batch, including
/// cases where a scalar branch skips all effects.
pub(super) fn cheapest(func: &Func, costs: &[BlockCost], lp: &super::loops::Loop) -> u32 {
    let mut work = vec![0; func.blocks.len()];
    for &block in lp.blocks.iter().rev() {
        let i = block.0 as usize;
        work[i] = costs[i].body
            + if block == lp.latch {
                costs[i].edges[usize::from(!lp.latch_taken)]
            } else {
                func.blocks[i]
                    .end
                    .targets()
                    .enumerate()
                    .map(|(edge, target)| costs[i].edges[edge] + work[target.block.0 as usize])
                    .min()
                    .unwrap()
            };
    }
    work[lp.head.0 as usize]
}

pub(super) fn batch_savings(
    func: &Func,
    costs: &[BlockCost],
    batch: &super::loops::Batch,
    words: u32,
    price: Option<&super::batch::Price>,
) -> u32 {
    if let Some(price) = price.filter(|price| !price.classes.is_empty()) {
        price
            .classes
            .iter()
            .map(|class| {
                let scalar = class
                    .first
                    .iter()
                    .chain(&class.second)
                    .map(|(block, edge)| {
                        let cost = costs[block.0 as usize];
                        cost.body + cost.edges[*edge]
                    })
                    .sum::<u32>();
                scalar.saturating_sub(price.common + class.words)
            })
            .min()
            .unwrap()
    } else {
        let scalar = cheapest(func, costs, &batch.loop_) * u32::from(batch.plan.lanes);
        scalar.saturating_sub(words)
    }
}
