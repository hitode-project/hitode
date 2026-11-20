//! Scoped workers redirect only dwords completely defined in every row.
//! The frame plan retains instruction indices so ordinary accesses never
//! acquire a worker-private address by matching a run-time pointer.

use super::{Access, ESP, Effect, Expr, Graph, Loop, Proof, SCOPE_CELLS, ScopeCell, ScopeFrame};
use crate::translate::front::{
    decode::{Op, Src},
    trace::{Mode, Trace},
};

pub(super) fn frame(proof: &Proof, trace: &Trace) -> Option<ScopeFrame> {
    let graph = Graph::build_selected(trace, true).ok()?;
    let lp = &graph.loops[graph.heads[0]?];
    let outer = proof.outer?;
    let mut cells = Vec::new();
    // Each cell has one instruction that writes it on every row.
    let mut definitions = Vec::new();
    for effect in &proof.effects {
        if effect.tail || !matches!(effect.access, Access::Write | Access::FrameWrite) {
            continue;
        }
        let location = cell(proof, effect);
        let Some(location) = location else {
            if effect.access == Access::FrameWrite {
                return None;
            }
            continue;
        };
        let (start, step) = outer.regs[usize::from(location.base)]?;
        if proof.exprs[start] != Expr::Register(location.base)
            || proof.exprs[step] != Expr::Constant(0)
            || effect.bytes != 4
            || cells.len() == SCOPE_CELLS
            || cells.iter().any(|c: &ScopeCell| {
                c.base == location.base && c.offset.abs_diff(location.offset) < 4
            })
        {
            return None;
        }
        let [write] = *effect.indices.as_slice() else {
            return None;
        };
        if !matches!(trace.insts[write].op, Op::Mov(Src::Mem(_), _) | Op::Call(_))
            || !dominates(&graph, lp, write, lp.tail)
        {
            return None;
        }
        cells.push(location);
        definitions.push(write);
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
            if effect.tail || location != *slot || effect.bytes != 4 {
                return None;
            }
            for &index in &effect.indices {
                if index != definitions[i]
                    && (!matches!(trace.insts[index].op, Op::Mov(_, Src::Mem(_)) | Op::Ret(0))
                        || !dominates(&graph, lp, definitions[i], index))
                {
                    return None;
                }
                if let Op::Ret(0) = trace.insts[index].op {
                    let Mode::Inline(expected) = trace.insts[index].mode else {
                        return None;
                    };
                    let write = trace.insts[definitions[i]];
                    if !matches!(write.op, Op::Call(_)) || write.next_pc != expected {
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
    Some(ScopeFrame {
        cells,
        access,
        mmx: 0,
        latch: None,
    })
}

pub(super) fn routed(proof: &Proof, frame: &ScopeFrame, effect: &Effect) -> bool {
    let Some(cell) = cell(proof, effect) else {
        return false;
    };
    effect.bytes == 4
        && !effect.tail
        && frame.cells.contains(&cell)
        && !effect.indices.is_empty()
        && effect
            .indices
            .iter()
            .all(|&at| frame.access[at].is_some_and(|i| frame.cells[usize::from(i)] == cell))
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
    let mut seen = vec![false; graph.edges.len()];
    let mut next = vec![lp.head];
    while let Some(at) = next.pop() {
        if at == definition || !lp.inside[at] || std::mem::replace(&mut seen[at], true) {
            continue;
        }
        if at == use_ {
            return false;
        }
        next.extend(graph.edges[at].iter().flatten().copied());
    }
    true
}
