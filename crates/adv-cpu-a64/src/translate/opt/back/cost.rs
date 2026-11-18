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
        pcs.extend(func.blocks[i].eip);
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
