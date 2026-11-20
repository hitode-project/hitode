//! Scoped workers redirect only dwords completely defined in every row.
//! The frame plan retains instruction indices so ordinary accesses never
//! acquire a worker-private address by matching a run-time pointer.

use super::{Access, ESP, Effect, Expr, Graph, Loop, Proof, SCOPE_CELLS, ScopeCell, ScopeFrame};
use crate::translate::front::{
    decode::Op,
    trace::{Mode, Trace},
};

pub(super) fn frame(proof: &Proof, trace: &Trace, graph: &Graph) -> Option<ScopeFrame> {
    let lp = &graph.loops[graph.heads[0]?];
    let outer = proof.outer?;
    let mut cells = Vec::new();
    let mut definitions: Vec<Vec<usize>> = Vec::new();
    for effect in &proof.effects {
        if effect.tail || !matches!(effect.access, Access::Write | Access::FrameWrite) {
            continue;
        }
        let Some(location) = cell(proof, effect) else {
            if effect.access == Access::FrameWrite {
                return None;
            }
            continue;
        };
        let (start, step) = outer.regs[usize::from(location.base)]?;
        if proof.exprs[start] != Expr::Register(location.base)
            || proof.exprs[step] != Expr::Constant(0)
            || effect.bytes != 4
        {
            return None;
        }
        if let Some(i) = cells.iter().position(|c| *c == location) {
            definitions[i].extend(&effect.indices);
        } else {
            if cells.len() == SCOPE_CELLS
                || cells.iter().any(|c: &ScopeCell| {
                    c.base == location.base && c.offset.abs_diff(location.offset) < 4
                })
            {
                return None;
            }
            cells.push(location);
            definitions.push(effect.indices.clone());
        }
    }
    // The owner publishes the last completed row's frame on both exits.
    // A cell must therefore have a definition even in a row with no reads.
    if definitions
        .iter()
        .any(|defs| reaches(graph, lp, lp.head, lp.test, |at| defs.contains(&at)))
    {
        return None;
    }
    let mut access = vec![None; trace.len()];
    for effect in &proof.effects {
        let Some(location) = cell(proof, effect) else {
            continue;
        };
        let bytes = i64::from(effect.bytes);
        let lo = i64::from(location.offset);
        for (i, slot) in cells.iter().enumerate() {
            let at = i64::from(slot.offset);
            if slot.base != location.base || at + 4 <= lo || lo + bytes <= at {
                continue;
            }
            if location != *slot || effect.bytes != 4 {
                return None;
            }
            if effect.tail {
                continue;
            }
            let read = matches!(effect.access, Access::Read | Access::FrameRead);
            for &index in &effect.indices {
                let defs = &definitions[i];
                if read && !defs.iter().any(|&def| dominates(graph, lp, def, index)) {
                    return None;
                }
                if let Op::Ret(0) = trace.insts[index].op {
                    let Mode::Inline(expected) = trace.insts[index].mode else {
                        return None;
                    };
                    let matching_call = |at: usize| {
                        let inst = trace.insts[at];
                        matches!(inst.op, Op::Call(_)) && inst.next_pc == expected
                    };
                    // Check every reaching definition of the return address.
                    if reaches(graph, lp, lp.head, index, matching_call)
                        || defs.iter().any(|&def| {
                            !matching_call(def) && reaches(graph, lp, def, index, matching_call)
                        })
                    {
                        return None;
                    }
                }
                if access[index]
                    .replace(i as u8)
                    .is_some_and(|old| old != i as u8)
                {
                    return None;
                }
            }
        }
    }
    // Lowering redirects every memory operand of a mapped instruction.
    let frame = ScopeFrame {
        cells,
        access,
        mmx: 0,
        gpr: 0,
        gpr_writes: Vec::new(),
        latch: (lp.test, lp.taken),
    };
    if proof.effects.iter().any(|effect| {
        !effect.tail
            && effect.indices.iter().any(|&at| frame.access[at].is_some())
            && !routed(proof, &frame, effect)
    }) {
        return None;
    }
    Some(frame)
}

pub(super) fn routed(proof: &Proof, frame: &ScopeFrame, effect: &Effect) -> bool {
    let Some(cell) = cell(proof, effect) else {
        return false;
    };
    effect.bytes == 4
        && frame.cells.contains(&cell)
        && (effect.tail
            || effect
                .indices
                .iter()
                .all(|&at| frame.access[at].is_some_and(|i| frame.cells[usize::from(i)] == cell)))
}

fn cell(proof: &Proof, effect: &Effect) -> Option<ScopeCell> {
    let addr = effect.addr.invariant()?;
    let base = |id| match proof.exprs[id] {
        Expr::Register(base) if base == ESP || base == proof.based => Some(base),
        _ => None,
    };
    let (base, offset) = match proof.exprs[addr] {
        Expr::Add(a, b) => match (proof.exprs[a], proof.exprs[b]) {
            (_, Expr::Constant(offset)) => (base(a)?, offset),
            (Expr::Constant(offset), _) => (base(b)?, offset),
            _ => return None,
        },
        _ => (base(addr)?, 0),
    };
    Some(ScopeCell {
        base,
        offset: offset as i32,
    })
}

fn dominates(graph: &Graph, lp: &Loop, definition: usize, use_: usize) -> bool {
    definition != use_ && !reaches(graph, lp, lp.head, use_, |at| at == definition)
}

fn reaches(
    graph: &Graph,
    lp: &Loop,
    start: usize,
    use_: usize,
    stops: impl Fn(usize) -> bool,
) -> bool {
    let mut seen = vec![false; graph.edges.len()];
    let mut next = vec![start];
    while let Some(at) = next.pop() {
        if stops(at) || !lp.inside[at] || std::mem::replace(&mut seen[at], true) {
            continue;
        }
        if at == use_ {
            return true;
        }
        next.extend(
            graph.edges[at]
                .iter()
                .flatten()
                .copied()
                .filter(|&at| at != lp.head),
        );
    }
    false
}
