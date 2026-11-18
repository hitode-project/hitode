use super::*;
use crate::translate::opt::recovery::{FlagExpr, FlagGroup, FlagSelect};
use crate::translate::opt::verify::{dominates, dominators};
use adv_cpu_core::integer::{AF, PF, SF, ZF};
use std::collections::BTreeSet;

const STATUS: u32 = CF | PF | AF | ZF | SF | OF;

struct Phi {
    block: Block,
    arrivals: Vec<(usize, usize, Value)>,
}

struct RecipePhi {
    select: FlagSelect<Value>,
    operands: BTreeMap<Value, Vec<Value>>,
}

/// Status bits the producer always keeps, and bits it conditionally keeps or reads.
fn dependence(kind: FlagKind, width: u8, count: Option<u128>) -> (u32, u32) {
    use FlagKind::*;
    let count = count.map(|n| n as u32 & 31);
    match kind {
        Increment | Decrement => (CF, 0),
        CarryBit => (STATUS & !CF, 0),
        MulSigned | MulUnsigned => (STATUS & !(CF | OF), 0),
        AddCarry | SubBorrow => (0, CF),
        Shl | Shr | Sar | Rol | Ror if count == Some(0) => (STATUS, 0),
        Shl | Shr | Sar => {
            let cf_kept = kind != Sar && count.is_some_and(|n| n > u32::from(width) * 8);
            let preserved = AF
                | if cf_kept { CF } else { 0 }
                | if count.is_some_and(|n| n != 1) { OF } else { 0 };
            let conditional = if count.is_none() { STATUS & !AF } else { 0 };
            (preserved, conditional)
        }
        Rol | Ror => {
            let preserved = STATUS & !(CF | if count.is_none_or(|n| n == 1) { OF } else { 0 });
            (preserved, if count.is_none() { CF | OF } else { 0 })
        }
        _ => (0, 0),
    }
}

struct Flow<'a> {
    defs: &'a [Option<Op>],
    dom: Vec<Option<usize>>,
    base: Value,
    at: Vec<usize>,
    phis: BTreeMap<Value, Phi>,
    regress: Vec<Option<(Value, u32)>>,
}

impl Flow<'_> {
    fn producer(&self, mut v: Value, mask: u32) -> Value {
        while let Some(Op::Flags {
            kind,
            width,
            b,
            prior: Some(prior),
            ..
        }) = self.defs[v.0 as usize]
            && dependence(kind, width, constant(self.defs, b)).0 & mask == mask
        {
            v = prior;
        }
        v
    }

    fn origins(&self, v: Value, mask: u32) -> BTreeSet<Value> {
        let mut seen = BTreeSet::new();
        let mut origins = BTreeSet::new();
        let mut work = vec![v];
        while let Some(v) = work.pop() {
            let v = self.producer(v, mask);
            if let Some(phi) = self.phis.get(&v) {
                if seen.insert(v) {
                    work.extend(phi.arrivals.iter().map(|&(_, _, a)| a));
                }
            } else {
                origins.insert(v);
            }
        }
        origins
    }

    fn expr(&self, v: Value, mask: u32, demand: &mut [FlagSet]) -> FlagExpr<Value> {
        let v = self.producer(v, mask);
        let operand = |v| constant(self.defs, v).map_or(FlagExpr::At(v), FlagExpr::Constant);
        match self.defs[v.0 as usize] {
            Some(Op::Flags {
                kind,
                width,
                a,
                b,
                prior,
            }) => {
                let (kept, read) = dependence(kind, width, constant(self.defs, b));
                let needed = (kept & mask)
                    | if matches!(kind, FlagKind::AddCarry | FlagKind::SubBorrow) {
                        read
                    } else {
                        read & mask
                    };
                let prior = prior
                    .filter(|_| needed != 0)
                    .map_or(FlagExpr::Constant(0), |prior| {
                        self.expr(prior, needed, demand)
                    });
                let (a, adjust) = self.regress[v.0 as usize].unwrap_or((a, 0));
                FlagExpr::Produce {
                    kind,
                    width,
                    a: Box::new(operand(a)),
                    b: Box::new(if kind.reads_b() {
                        operand(b)
                    } else {
                        FlagExpr::Constant(0)
                    }),
                    prior: Box::new(prior),
                    adjust,
                }
            }
            _ => {
                // Conditional producers read their prior on the executed path.
                // Keep the ordinary admission proof for those inputs.
                if self.phis.contains_key(&v) {
                    for (bit, set) in [
                        (CF, FlagSet::CF),
                        (PF, FlagSet::PF),
                        (AF, FlagSet::AF),
                        (ZF, FlagSet::ZF),
                        (SF, FlagSet::SF),
                        (OF, FlagSet::OF),
                    ] {
                        if mask & bit != 0 {
                            demand[v.0 as usize] = demand[v.0 as usize].or(set);
                        }
                    }
                }
                operand(v)
            }
        }
    }
}

fn regression(defs: &[Option<Op>], insts: &[(Option<Value>, Op)], op: Op) -> Option<(Value, u32)> {
    let Op::Flags {
        kind,
        width: 4,
        a,
        b,
        ..
    } = op
    else {
        return None;
    };
    let (op, step) = match kind {
        FlagKind::Add => (IntOp::Add, constant(defs, b)? as u32),
        FlagKind::Sub => (IntOp::Sub, constant(defs, b)? as u32),
        FlagKind::Increment => (IntOp::Add, 1),
        FlagKind::Decrement => (IntOp::Sub, 1),
        _ => return None,
    };
    let adjust = if op == IntOp::Add {
        step.wrapping_neg()
    } else {
        step
    };
    insts.iter().find_map(|&(v, inst)| match inst {
        Op::Int(k, left, right)
            if k == op && left == a && constant(defs, right) == Some(u128::from(step)) =>
        {
            v.map(|v| (v, adjust))
        }
        _ => None,
    })
}

pub(super) fn prepare(func: &mut Func) -> Vec<FlagSet> {
    let mut demand = vec![FlagSet::NONE; func.types.len()];
    let Some(entry) = func.entry.iter().position(|s| *s == Source::Eflags) else {
        return demand;
    };
    let base = func.blocks[0].params[entry];
    let defs = definitions(func);
    let mut flow = Flow {
        defs: &defs,
        dom: dominators(func),
        base,
        at: vec![0; defs.len()],
        phis: BTreeMap::new(),
        regress: vec![None; defs.len()],
    };
    let mut masks = vec![STATUS];
    for (b, data) in func.blocks.iter().enumerate() {
        for &p in &data.params {
            flow.at[p.0 as usize] = b;
            if b != 0 && func.ty(p) == Type::Flags {
                flow.phis.insert(
                    p,
                    Phi {
                        block: Block(b as u32),
                        arrivals: Vec::new(),
                    },
                );
            }
        }
        for &(v, op) in &data.insts {
            if let Some(v) = v {
                flow.at[v.0 as usize] = b;
                flow.regress[v.0 as usize] = regression(&defs, &data.insts, op);
            }
            if let Op::Flags { kind, width, b, .. } = op {
                let kept = dependence(kind, width, constant(&defs, b)).0;
                let count = masks.len();
                for i in 0..count {
                    let part = masks[i] & kept;
                    if part != 0 && part != masks[i] {
                        masks[i] ^= part;
                        masks.push(part);
                    }
                }
            }
        }
    }
    for (b, data) in func.blocks.iter().enumerate() {
        for (k, edge) in data.end.targets().enumerate() {
            for (&p, &a) in func.blocks[edge.block.0 as usize]
                .params
                .iter()
                .zip(&edge.args)
            {
                if let Some(phi) = flow.phis.get_mut(&p) {
                    phi.arrivals.push((b, k, a));
                }
            }
        }
    }
    let mut snapshots = BTreeMap::new();
    for mask in masks {
        transport(func, &flow, mask, &mut demand, &mut snapshots);
    }
    demand
}

fn transport(
    func: &mut Func,
    flow: &Flow<'_>,
    mask: u32,
    demand: &mut [FlagSet],
    snapshots: &mut BTreeMap<Value, Value>,
) {
    let base = flow.base;
    let mut forms = BTreeMap::new();
    let mut columns = BTreeMap::new();
    let mut constants = BTreeMap::new();
    let mut add_constant = |func: &mut Func, ty, bits| {
        *constants
            .entry((ty as u8, bits))
            .or_insert_with(|| func.push(Block(0), Op::Constant(ty, bits)).unwrap())
    };
    let mut records = Vec::new();
    let active = func.blocks.iter().flat_map(|block| {
        block
            .head
            .map(|head| head.record)
            .into_iter()
            .chain(match block.end {
                End::Exit(record) => Some(record),
                _ => None,
            })
            .chain(block.insts.iter().filter_map(|&(_, op)| match op {
                Op::Store { after, .. } => Some(after),
                _ => None,
            }))
    });
    for id in active {
        let record = &func.records[id.0 as usize];
        let Some(Location::Recipe(Recipe::Eflags(flags))) = record.eflags else {
            continue;
        };
        let producer = flow.producer(flags, mask);
        if flow.phis.contains_key(&producer) {
            records.push((id.0 as usize, producer));
        } else {
            flow.expr(producer, mask, demand);
        }
    }
    let origins: BTreeSet<_> = records
        .iter()
        .flat_map(|&(_, p)| flow.origins(p, mask))
        .collect();
    let mut canonical = BTreeMap::new();
    for v in origins {
        let expr = flow.expr(v, mask, demand);
        let key = if let Some((&key, _)) = forms.iter().find(|(_, form)| **form == expr) {
            key
        } else {
            forms.insert(v, expr);
            v
        };
        canonical.insert(v, key);
    }
    let tags: BTreeMap<_, _> = forms.keys().copied().zip(0u32..).collect();
    let producer = |v| {
        let v = flow.producer(v, mask);
        canonical.get(&v).copied().unwrap_or(v)
    };
    let mut pending: Vec<_> = records.iter().map(|&(_, p)| p).collect();
    while let Some(p) = pending.pop() {
        if columns.contains_key(&p) {
            continue;
        }
        let phi = &flow.phis[&p];
        let origins: BTreeSet<_> = flow
            .origins(p, mask)
            .into_iter()
            .map(|v| canonical[&v])
            .collect();
        let forwarded = phi.arrivals.iter().filter_map(|&(_, _, arg)| {
            let v = producer(arg);
            flow.phis.contains_key(&v).then_some(v)
        });
        pending.extend(forwarded);
        let select = if origins.len() == 1 {
            FlagSelect::Same
        } else if origins.len() == 2
            && phi.arrivals.len() == 2
            && phi
                .arrivals
                .iter()
                .all(|&(_, _, a)| origins.contains(&producer(a)))
            && !flow.phis.iter().any(|(&other, q)| {
                other != p && q.arrivals.iter().any(|&(_, _, a)| producer(a) == p)
            })
            && let Some((current, initial)) = induction(func, phi)
        {
            let initial = if constant(flow.defs, initial).is_some() {
                initial
            } else {
                *snapshots
                    .entry(initial)
                    .or_insert_with(|| func.push(Block(0), Op::Snapshot(initial)).unwrap())
            };
            FlagSelect::Induction { current, initial }
        } else {
            FlagSelect::Tag(func.param(phi.block, Type::I32))
        };
        let head_params = func.blocks[phi.block.0 as usize].params.clone();
        let operands = origins
            .into_iter()
            .map(|v| {
                let mut args = Vec::new();
                forms[&v].map(&mut |leaf| {
                    let ty = func.ty(leaf);
                    args.push(if ty == Type::Flags || flow.at[leaf.0 as usize] == 0 {
                        leaf
                    } else if let Some(&p) = head_params.iter().enumerate().find_map(|(i, p)| {
                        if func.ty(*p) != ty {
                            return None;
                        }
                        let mut active = false;
                        for &(from, k, arg) in &phi.arrivals {
                            let source = producer(arg);
                            if flow.phis.contains_key(&source)
                                && flow.origins(source, mask).iter().any(|q| canonical[q] == v)
                            {
                                return None;
                            }
                            if source == v {
                                active = true;
                                let edge = func.blocks[from].end.targets().nth(k).unwrap();
                                if edge.args.get(i) != Some(&leaf) {
                                    return None;
                                }
                            }
                        }
                        active.then_some(p)
                    }) {
                        p
                    } else {
                        func.param(phi.block, ty)
                    });
                });
                (v, args)
            })
            .collect();
        columns.insert(p, RecipePhi { select, operands });
    }

    for (&p, recipe) in &columns {
        let phi = &flow.phis[&p];
        let b = phi.block.0 as usize;
        let mut params = Vec::new();
        if let FlagSelect::Tag(tag) = recipe.select {
            params.push((tag, None));
        }
        for (&v, args) in &recipe.operands {
            let mut leaves = Vec::new();
            forms[&v].leaves(&mut leaves);
            params.extend(args.iter().copied().zip(leaves).enumerate().filter_map(
                |(i, (param, leaf))| {
                    (param != leaf
                        && !phi.arrivals.iter().any(|&(from, k, _)| {
                            let edge = func.blocks[from].end.targets().nth(k).unwrap();
                            func.blocks[b]
                                .params
                                .iter()
                                .take(edge.args.len())
                                .any(|&p| p == param)
                        }))
                    .then_some((param, Some((v, i, leaf))))
                },
            ));
        }
        for &(from, k, arg) in &phi.arrivals {
            let origin = producer(arg);
            let forwarded = columns.get(&origin);
            for &(param, arm) in &params {
                let value = if let Some((v, i, leaf)) = arm {
                    if let Some(arg) = forwarded.and_then(|p| p.operands.get(&v)) {
                        arg[i]
                    } else if origin == v {
                        leaf
                    } else if dominates(&flow.dom, b, from) {
                        param
                    } else {
                        let ty = func.ty(param);
                        add_constant(func, ty, 0)
                    }
                } else if let Some(FlagSelect::Tag(tag)) = forwarded.map(|p| &p.select) {
                    *tag
                } else {
                    let tag =
                        forwarded.map_or(origin, |p| *p.operands.first_key_value().unwrap().0);
                    add_constant(func, Type::I32, u128::from(tags[&tag]))
                };
                targets_mut(&mut func.blocks[from].end)[k].args.push(value);
            }
        }
    }

    for (record, p) in records {
        let recipe = &columns[&p];
        if recipe.operands.len() == 1 && recipe.operands.contains_key(&base) {
            continue;
        }
        let mut arms: Vec<_> = recipe
            .operands
            .iter()
            .map(|(v, args)| {
                let mut args = args.iter().copied();
                (tags[v], forms[v].map(&mut |_| args.next().unwrap()))
            })
            .collect();
        if matches!(recipe.select, FlagSelect::Induction { .. }) {
            let entry = producer(flow.phis[&p].arrivals[0].2);
            arms.sort_by_key(|&(tag, _)| tag != tags[&entry]);
        }
        func.records[record].flag_groups.push(FlagGroup {
            mask,
            select: recipe.select.clone(),
            arms,
        });
    }
}

fn induction(func: &Func, phi: &Phi) -> Option<(Value, Value)> {
    let [(entry, enter_edge, _), (latch, k, _)] = phi.arrivals[..] else {
        return None;
    };
    let entry_edge = func.blocks[entry].end.targets().nth(enter_edge)?;
    let back = func.blocks[latch].end.targets().nth(k)?;
    if entry >= phi.block.0 as usize
        || back.charge <= 0
            && !func.blocks.iter().any(|block| {
                block
                    .end
                    .targets()
                    .any(|t| t.block.0 as usize == latch && t.charge > 0)
            })
    {
        return None;
    }
    let defs = crate::translate::opt::loops::Defs::new(func);
    func.blocks[phi.block.0 as usize]
        .params
        .iter()
        .copied()
        .zip(entry_edge.args.iter().zip(&back.args))
        .find_map(|(p, (&initial, &after))| {
            let crate::translate::opt::loops::Affine { base, offset } = defs.affine(after);
            // Native caps each run at u32::MAX retired instructions, before a
            // 32-bit IV with odd stride can return to its initial value.
            (base == p && offset & 1 != 0 && (entry == 0 || defs.constant(initial).is_some()))
                .then_some((p, initial))
        })
}
