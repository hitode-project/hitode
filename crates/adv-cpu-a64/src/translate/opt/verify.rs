//! Checks a `Func` before emission: SSA dominance, operand types, recovery
//! records, and the effect-point invariant of A64-OPT-5. A failure is a bug in
//! the front end or a pass, so translation asserts success.

use super::ir::{Block, End, Func, Op, RecordId, Type, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    /// Block 0 is not a head, a charged edge reaches a block that is not,
    /// or a block has no end.
    Shape(Block),
    /// A use the definition does not dominate.
    Undominated(Value),
    /// An operand of the wrong type, a jump with the wrong arguments, or a
    /// `Flags` parameter outside block 0: flags reach a head only as the
    /// entry EFLAGS (A64-FLAG-3).
    Type(Block),
    /// An effect point reached with an unvalidated floating-point result.
    Pending(Block),
    /// Arrivals at a block disagree on the region start.
    Region(Block),
    /// A frame commit followed by an operation that can fail, or in a
    /// block that does not end by jumping to heads.
    Commit(Block),
    /// A record activated twice, reading a value not available where it
    /// is activated, incomplete at an exit, or without EFLAGS at the head
    /// of a charged arrival.
    Record(RecordId),
}

/// A position in a block: parameters are at 0, instruction `i` at `i + 1`,
/// and the end after the last instruction.
type Position = (usize, usize);

pub(crate) fn verify(func: &Func) -> Result<(), Error> {
    let blocks = &func.blocks;
    if blocks.first().is_none_or(|b| b.head.is_none()) {
        return Err(Error::Shape(Block(0)));
    }
    if let Some(b) = blocks.iter().position(|b| b.end == End::Unset) {
        return Err(Error::Shape(Block(b as u32)));
    }
    let sources = func.entry.iter().map(|s| s.ty());
    if !sources.eq(blocks[0].params.iter().map(|&p| func.ty(p))) {
        return Err(Error::Type(Block(0)));
    }
    if let Some(b) = blocks[1..]
        .iter()
        .position(|b| b.params.iter().any(|&p| func.ty(p) == Type::Flags))
    {
        return Err(Error::Type(Block(b as u32 + 1)));
    }
    let dom = dominators(func);
    let mut defs = vec![None; func.types.len()];
    for (b, block) in blocks.iter().enumerate() {
        for &p in &block.params {
            defs[p.0 as usize] = Some((b, 0));
        }
        for (i, (result, _)) in block.insts.iter().enumerate() {
            if let Some(r) = result {
                defs[r.0 as usize] = Some((b, i + 1));
            }
        }
    }
    let available = |value: Value, (b, pos): Position| {
        defs[value.0 as usize].is_some_and(|(db, dpos)| {
            if db == b {
                dpos < pos
            } else {
                dominates(&dom, db, b)
            }
        })
    };
    let mut activated = vec![false; func.records.len()];
    let mut activate = |id: RecordId, at: Position, complete: bool| {
        let record = &func.records[id.0 as usize];
        let seen = std::mem::replace(&mut activated[id.0 as usize], true);
        if seen
            || complete && !record.complete(func.kept)
            || !record.uses().all(|v| available(v, at))
        {
            return Err(Error::Record(id));
        }
        Ok(())
    };
    for (b, block) in blocks.iter().enumerate() {
        if let Some(head) = block.head {
            activate(head.record, (b, 1), false)?;
        }
        for (i, (_, op)) in block.insts.iter().enumerate() {
            if let Some(v) = op.uses().find(|&v| !available(v, (b, i + 1))) {
                return Err(Error::Undominated(v));
            }
            if !typed(func, op) {
                return Err(Error::Type(Block(b as u32)));
            }
            if let Op::Store { after, .. } = *op {
                activate(after, (b, i + 2), false)?;
            }
        }
        let end = (b, block.insts.len() + 1);
        let mut uses: Vec<Value> = block.end.targets().flat_map(|t| t.args.clone()).collect();
        match block.end {
            End::Branch { cond, .. } => {
                if func.ty(cond) != Type::I32 {
                    return Err(Error::Type(Block(b as u32)));
                }
                uses.push(cond);
            }
            End::Exit(id) => activate(id, end, true)?,
            _ => {}
        }
        if let Some(&v) = uses.iter().find(|&&v| !available(v, end)) {
            return Err(Error::Undominated(v));
        }
        for target in block.end.targets() {
            let params = &blocks[target.block.0 as usize].params;
            let types = |list: &[Value]| list.iter().map(|&v| func.ty(v)).collect::<Vec<_>>();
            if types(params) != types(&target.args) {
                return Err(Error::Type(Block(b as u32)));
            }
            if target.charge > 0 {
                let Some(head) = blocks[target.block.0 as usize].head else {
                    return Err(Error::Shape(target.block));
                };
                if func.records[head.record.0 as usize].eflags.is_none() {
                    return Err(Error::Record(head.record));
                }
            }
        }
    }
    effects(func)
}

fn typed(func: &Func, op: &Op) -> bool {
    let is = |v: Value, ty: Type| func.ty(v) == ty;
    let int_ty = |ty: Type| matches!(ty, Type::I32 | Type::I64);
    let int = |v: Value| matches!(func.ty(v), Type::I32 | Type::I64);
    match *op {
        Op::Int(_, a, b) => int(a) && func.ty(a) == func.ty(b),
        Op::IntUnary(_, a) | Op::FromInt(a) => int(a),
        Op::Div {
            dividend, divisor, ..
        } => is(dividend, Type::I64) && is(divisor, Type::I32),
        Op::Flags { a, prior, .. } => {
            (int(a) || is(a, Type::F64)) && prior.is_none_or(|p| is(p, Type::Flags))
        }
        Op::Test { flags, .. } => is(flags, Type::Flags),
        Op::Select { cond, a, b } => is(cond, Type::I32) && func.ty(a) == func.ty(b),
        Op::Fp(_, a, b) => is(a, Type::F64) && is(b, Type::F64),
        Op::FpUnary(_, a) | Op::ToInt { value: a, .. } => is(a, Type::F64),
        Op::Vec(_, a, b) => is(a, Type::V128) && is(b, Type::V128),
        Op::Load { addr, .. } | Op::Store { addr, .. } | Op::Host(addr) => is(addr, Type::I32),
        Op::FrameLoad { ty, base, .. } => (int_ty(ty) || ty == Type::F64) && is(base, Type::I64),
        Op::FrameStore { base, value, .. } => {
            (int(value) || is(value, Type::F64)) && is(base, Type::I64)
        }
        Op::X87Load { bits, single, .. } => is(bits, if single { Type::I32 } else { Type::I64 }),
        Op::Constant(..) | Op::Bits { .. } | Op::Validate => true,
    }
}

/// Forward dataflow over (pending, region): whether a floating-point result
/// is unvalidated, and which region start failures return to.
fn effects(func: &Func) -> Result<(), Error> {
    let blocks = &func.blocks;
    let mut state: Vec<Option<(bool, RecordId)>> = vec![None; blocks.len()];
    let first = blocks[0].head.expect("checked").record;
    state[0] = Some((false, first));
    let mut work = vec![0usize];
    while let Some(b) = work.pop() {
        let block = &blocks[b];
        let (mut pending, mut region) = state[b].expect("queued with a state");
        if let Some(head) = block.head {
            region = head.record;
        }
        // Frame memory may change only when no failure can still return to
        // the region start: after validation, and with nothing that can
        // fail before the heads the block jumps to (A64-OPT-3).
        let mut committed = false;
        for (_, op) in &block.insts {
            if let Op::Store { after, .. } = *op {
                if pending {
                    return Err(Error::Pending(Block(b as u32)));
                }
                region = after;
            }
            if matches!(op, Op::FrameStore { .. }) {
                if pending {
                    return Err(Error::Pending(Block(b as u32)));
                }
                committed = true;
            } else if committed && op.can_fail() {
                return Err(Error::Commit(Block(b as u32)));
            }
            pending = match op {
                Op::Validate => false,
                op => pending || op.floating(),
            };
        }
        if pending && matches!(block.end, End::Exit(_)) {
            return Err(Error::Pending(Block(b as u32)));
        }
        let ends_at_heads = match block.end {
            End::Jump(_) | End::Branch { .. } => block
                .end
                .targets()
                .all(|t| blocks[t.block.0 as usize].head.is_some()),
            End::Exit(_) | End::Unset => false,
        };
        if committed && !ends_at_heads {
            return Err(Error::Commit(Block(b as u32)));
        }
        for target in block.end.targets() {
            let t = target.block.0 as usize;
            if blocks[t].head.is_some() && pending {
                return Err(Error::Pending(Block(b as u32)));
            }
            let arrival = (pending, region);
            let next = match state[t] {
                None => arrival,
                Some((p, r)) if blocks[t].head.is_some() || r == region => (p || pending, r),
                Some(_) => return Err(Error::Region(target.block)),
            };
            if state[t] != Some(next) {
                state[t] = Some(next);
                work.push(t);
            }
        }
    }
    Ok(())
}

/// Immediate dominators by the iterative algorithm of Cooper, Harvey and
/// Kennedy over reverse postorder. Unreachable blocks have none.
fn dominators(func: &Func) -> Vec<Option<usize>> {
    let n = func.blocks.len();
    let succ = |b: usize| func.blocks[b].end.targets().map(|t| t.block.0 as usize);
    let mut order = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut stack = vec![(0usize, false)];
    while let Some((b, done)) = stack.pop() {
        if done {
            order.push(b);
            continue;
        }
        if std::mem::replace(&mut seen[b], true) {
            continue;
        }
        stack.push((b, true));
        stack.extend(succ(b).filter(|&s| !seen[s]).map(|s| (s, false)));
    }
    order.reverse();
    let mut rank = vec![usize::MAX; n];
    for (i, &b) in order.iter().enumerate() {
        rank[b] = i;
    }
    let mut preds = vec![Vec::new(); n];
    for &b in &order {
        for s in succ(b) {
            preds[s].push(b);
        }
    }
    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[0] = Some(0);
    let mut changed = true;
    while changed {
        changed = false;
        for &b in &order[1..] {
            let mut new: Option<usize> = None;
            for &p in &preds[b] {
                if idom[p].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => p,
                    Some(mut a) => {
                        let mut p = p;
                        while a != p {
                            while rank[a] > rank[p] {
                                a = idom[a].expect("processed");
                            }
                            while rank[p] > rank[a] {
                                p = idom[p].expect("processed");
                            }
                        }
                        a
                    }
                });
            }
            if new.is_some() && idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
    }
    idom
}

fn dominates(idom: &[Option<usize>], a: usize, mut b: usize) -> bool {
    loop {
        if a == b {
            return true;
        }
        match idom[b] {
            Some(up) if up != b => b = up,
            _ => return false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::ir::{FpOp, Head, Op, Source, Target};
    use super::super::recovery::{IrRecord, Location, X87};
    use super::*;

    fn record(regs: [Option<Value>; 8]) -> IrRecord {
        IrRecord {
            eip: 0x1000,
            refund: 0,
            regs: regs.map(|r| r.map(Location::At)),
            eflags: Some(Location::Constant(0)),
            x87: Some(X87 {
                top: 0,
                valid: 0,
                control: 0x27f,
                stack: [None; 8],
                status: 0x20,
                c1: Some(Location::Constant(0)),
                ip: Some(Location::Constant(0)),
                opcode: Some(Location::Constant(0)),
            }),
            xmm: [Some(Location::Constant(0)); 8],
            frame: Vec::new(),
        }
    }

    /// A head with one I32 parameter in EAX and an F64 parameter, falling
    /// into `body`, which the test fills and ends.
    fn skeleton() -> (Func, Block, Value, Value) {
        let mut f = Func::default();
        let head = f.block(None);
        let eax = f.param(head, Type::I32);
        let x = f.param(head, Type::F64);
        f.entry = vec![Source::Reg(0), Source::X87(0)];
        let entry = f.record(record([Some(eax); 8]));
        f.blocks[0].head = Some(Head { record: entry });
        let body = f.block(None);
        f.end(
            head,
            End::Jump(Target {
                block: body,
                args: Vec::new(),
                charge: 0,
            }),
        );
        (f, body, eax, x)
    }

    fn store_product(validate: bool) -> Func {
        let (mut f, body, eax, x) = skeleton();
        let product = f.push(body, Op::Fp(FpOp::Mul, x, x)).unwrap();
        let bits = f
            .push(
                body,
                Op::Bits {
                    to: Type::I64,
                    value: product,
                },
            )
            .unwrap();
        if validate {
            f.push(body, Op::Validate);
        }
        let after = f.record(record([Some(eax); 8]));
        f.push(
            body,
            Op::Store {
                width: 8,
                addr: eax,
                value: bits,
                after,
            },
        );
        let exit = f.record(record([Some(eax); 8]));
        f.end(body, End::Exit(exit));
        f
    }

    // spec: A64-OPT-5
    #[test]
    fn a_store_needs_validation_after_floating_point() {
        assert_eq!(verify(&store_product(false)), Err(Error::Pending(Block(1))));
        assert_eq!(verify(&store_product(true)), Ok(()));
    }

    // spec: A64-OPT-5
    #[test]
    fn a_pending_result_on_one_arrival_reaches_the_merge() {
        let (mut f, body, eax, x) = skeleton();
        let left = f.block(None);
        let right = f.block(None);
        let join = f.block(None);
        let to = |block| Target {
            block,
            args: Vec::new(),
            charge: 0,
        };
        f.end(
            body,
            End::Branch {
                cond: eax,
                taken: to(left),
                fall: to(right),
            },
        );
        f.push(left, Op::Fp(FpOp::Add, x, x));
        f.end(left, End::Jump(to(join)));
        f.end(right, End::Jump(to(join)));
        let after = f.record(record([Some(eax); 8]));
        f.push(
            join,
            Op::Store {
                width: 4,
                addr: eax,
                value: eax,
                after,
            },
        );
        let exit = f.record(record([Some(eax); 8]));
        f.end(join, End::Exit(exit));
        assert_eq!(verify(&f), Err(Error::Pending(join)));
    }

    // spec: A64-OPT-5
    #[test]
    fn a_backedge_is_an_effect_point() {
        let (mut f, body, _, x) = skeleton();
        let sum = f.push(body, Op::Fp(FpOp::Add, x, x)).unwrap();
        let eax = f.blocks[0].params[0];
        let back = Target {
            block: Block(0),
            args: vec![eax, sum],
            charge: 2,
        };
        f.end(body, End::Jump(back.clone()));
        assert_eq!(verify(&f), Err(Error::Pending(body)));
        f.blocks[body.0 as usize].insts.push((None, Op::Validate));
        assert_eq!(verify(&f), Ok(()));
    }

    // spec: A64-OPT-6
    #[test]
    fn a_charged_edge_reaches_a_head_with_exact_flags() {
        let (mut f, body, eax, _) = skeleton();
        let tail = f.block(None);
        let exit = f.record(record([Some(eax); 8]));
        f.end(tail, End::Exit(exit));
        let to_tail = Target {
            block: tail,
            args: Vec::new(),
            charge: 1,
        };
        f.end(body, End::Jump(to_tail));
        assert_eq!(verify(&f), Err(Error::Shape(tail)));
        let mut partial = record([Some(eax); 8]);
        partial.eflags = None;
        let start = f.record(partial);
        f.blocks[tail.0 as usize].head = Some(Head { record: start });
        assert_eq!(verify(&f), Err(Error::Record(start)));
        // A register no execution from there reads may be absent.
        let mut dead = record([Some(eax); 8]);
        dead.regs[5] = None;
        f.records[start.0 as usize] = dead;
        assert_eq!(verify(&f), Ok(()));
    }

    // spec: A64-OPT-6
    #[test]
    fn records_read_only_available_values_and_exits_are_complete() {
        let (mut f, body, eax, _) = skeleton();
        let sum = f
            .push(body, Op::Int(super::super::ir::IntOp::Add, eax, eax))
            .unwrap();
        let early = f.record(record([Some(sum); 8]));
        f.blocks[body.0 as usize].insts.insert(
            0,
            (
                None,
                Op::Store {
                    width: 4,
                    addr: eax,
                    value: eax,
                    after: early,
                },
            ),
        );
        let mut partial = record([Some(eax); 8]);
        partial.regs[3] = None;
        let exit = f.record(partial);
        f.end(body, End::Exit(exit));
        assert_eq!(verify(&f), Err(Error::Record(early)));
        f.records[early.0 as usize] = record([Some(eax); 8]);
        assert_eq!(verify(&f), Err(Error::Record(exit)));
        // A register the form never writes keeps its entry value.
        f.kept = 1 << 3;
        assert_eq!(verify(&f), Ok(()));
    }

    #[test]
    fn uses_must_be_dominated() {
        let (mut f, body, eax, x) = skeleton();
        let side = f.block(None);
        let join = f.block(None);
        let to = |block| Target {
            block,
            args: Vec::new(),
            charge: 0,
        };
        f.end(
            body,
            End::Branch {
                cond: eax,
                taken: to(side),
                fall: to(join),
            },
        );
        let product = f.push(side, Op::Fp(FpOp::Mul, x, x)).unwrap();
        f.push(side, Op::Validate);
        f.end(side, End::Jump(to(join)));
        f.push(join, Op::Fp(FpOp::Add, product, x));
        f.push(join, Op::Validate);
        let exit = f.record(record([Some(eax); 8]));
        f.end(join, End::Exit(exit));
        assert_eq!(verify(&f), Err(Error::Undominated(product)));
    }
}
