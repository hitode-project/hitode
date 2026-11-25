//! Natural loops and affine streams in the optimized SSA form. A kernel
//! proposes a batch for a loop, while the back end owns its guards, charge
//! and transition to the original scalar body.

use crate::translate::front::tags::ConditionCode as Cc;

use super::back::BatchEmitter;
use super::ir::*;
use super::verify::{dominates, dominators};

pub(super) mod vector;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reject {
    Control,
    Induction,
    Latch,
    Address,
    Access,
    Kernel,
    Cost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Affine {
    pub base: Value,
    pub offset: u32,
}

impl Affine {
    pub fn shifted(self, offset: u32) -> Self {
        Self {
            offset: self.offset.wrapping_add(offset),
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Iv {
    pub base: Value,
    pub stride: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Trip {
    pub value: Affine,
    pub bound: Option<Value>,
    pub stride: i32,
    /// The condition that takes the back edge.
    pub again: Cc,
    pub sign: bool,
}

impl Trip {
    /// For a not-equal latch, the first equality is at this iteration
    /// (modulo the period), provided the low `shift` bits of the distance
    /// are zero. The odd part of the stride is invertible modulo 2^32.
    pub fn equality(self) -> (u32, u32, u32) {
        let stride = self.stride as u32;
        let shift = stride.trailing_zeros();
        let odd = stride >> shift;
        let mut inverse = odd;
        for _ in 0..4 {
            inverse = inverse.wrapping_mul(2u32.wrapping_sub(odd.wrapping_mul(inverse)));
        }
        (shift, inverse, u32::MAX >> shift)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Access {
    Load(Value),
    Store(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Stream {
    pub addr: Affine,
    pub stride: i32,
    pub width: u8,
    pub access: Access,
}

impl Stream {
    pub fn write(self) -> bool {
        matches!(self.access, Access::Store(_))
    }
}

/// The blocks are in topological order inside the iteration.
pub(super) struct Loop {
    pub head: Block,
    pub eip: u32,
    pub latch: Block,
    pub latch_taken: bool,
    pub blocks: Vec<Block>,
    /// Header parameters and their returning edge arguments.
    pub carried: Vec<(Value, Value)>,
    /// Returning edge values needed only by lazy head flag recipes.
    pub flag_carried: Vec<(Value, Value)>,
    pub ivs: Vec<Iv>,
    pub trip: Trip,
    pub streams: Vec<Stream>,
    pub charge: u32,
}

impl Loop {
    fn covering_read(&self, narrow: Stream) -> Option<(Value, u32)> {
        if narrow.width != 4 || narrow.write() || narrow.stride == 0 {
            return None;
        }
        self.streams.iter().find_map(|wide| {
            let Access::Load(value) = wide.access else {
                return None;
            };
            let offset = narrow.addr.offset.wrapping_sub(wide.addr.offset);
            (wide.width == 8
                && wide.stride == narrow.stride
                && wide.addr.base == narrow.addr.base
                && matches!(offset, 0 | 4))
            .then(|| (value, offset * 8))
        })
    }

    fn groups(&self) -> Vec<Vec<(usize, Stream)>> {
        // Group adjacent columns of an AoS interleave regardless of IR load
        // order (e.g. loading RGB as B, G, R or XY as Y, X) while keeping
        // each stream's original index for alias ordering (`r < w`).
        let mut buckets: std::collections::BTreeMap<(Value, i32, u8, bool), Vec<(usize, Stream)>> =
            std::collections::BTreeMap::new();
        let mut order: Vec<(Value, i32, u8, bool)> = Vec::new();
        for (i, &s) in self.streams.iter().enumerate() {
            let key = (s.addr.base, s.stride, s.width, s.write());
            if !buckets.contains_key(&key) {
                order.push(key);
            }
            buckets.entry(key).or_default().push((i, s));
        }
        let mut groups: Vec<Vec<(usize, Stream)>> = Vec::new();
        for key in order {
            let mut list = buckets.remove(&key).unwrap();
            if key.1 == 0 || list.len() == 1 {
                for item in list {
                    groups.push(vec![item]);
                }
                continue;
            }
            list.sort_by_key(|&(_, s)| s.addr.offset);
            for (i, s) in list {
                if let Some(group) = groups.iter_mut().find(|g| {
                    let &(_, previous) = g.last().unwrap();
                    let bytes = u32::from(s.width) * (g.len() as u32 + 1);
                    previous.width == s.width
                        && previous.stride == s.stride
                        && previous.write() == s.write()
                        && previous.addr.offset.checked_add(u32::from(s.width))
                            == Some(s.addr.offset)
                        && previous.addr.base == s.addr.base
                        && bytes <= s.stride.unsigned_abs()
                }) {
                    group.push((i, s));
                } else {
                    groups.push(vec![(i, s)]);
                }
            }
        }
        groups
    }
}

pub(super) struct Defs {
    ops: Vec<Option<Op>>,
    blocks: Vec<Option<Block>>,
}

impl Defs {
    pub fn new(func: &Func) -> Self {
        let mut defs = Self {
            ops: vec![None; func.types.len()],
            blocks: vec![None; func.types.len()],
        };
        for (b, data) in func.blocks.iter().enumerate() {
            let b = Some(Block(b as u32));
            for &p in &data.params {
                defs.blocks[p.0 as usize] = b;
            }
            for &(v, op) in &data.insts {
                if let Some(v) = v {
                    defs.ops[v.0 as usize] = Some(op);
                    defs.blocks[v.0 as usize] = b;
                }
            }
        }
        defs
    }

    pub fn op(&self, v: Value) -> Option<Op> {
        self.ops[v.0 as usize]
    }

    pub fn constant(&self, v: Value) -> Option<u128> {
        match self.op(v) {
            Some(Op::Constant(_, bits)) => Some(bits),
            _ => None,
        }
    }

    pub fn invariant(&self, blocks: &[Block], v: Value) -> bool {
        self.constant(v).is_some()
            || self.blocks[v.0 as usize].is_some_and(|b| !blocks.contains(&b))
    }

    pub fn affine(&self, mut v: Value) -> Affine {
        let mut offset = 0u32;
        while let Some(Op::Int(op @ (IntOp::Add | IntOp::Sub), a, b)) = self.op(v) {
            let Some(n) = self.constant(b) else { break };
            let n = n as u32;
            offset = offset.wrapping_add(if op == IntOp::Add {
                n
            } else {
                n.wrapping_neg()
            });
            v = a;
        }
        Affine { base: v, offset }
    }
}

/// A checkpoint split has the scalar arrival record but no body of its own.
pub(super) fn through(func: &Func, block: Block) -> Option<&Target> {
    let data = &func.blocks[block.0 as usize];
    match &data.end {
        End::Jump(t)
            if data.head.is_some()
                && block.0 != 0
                && data.params.is_empty()
                && data.insts.is_empty()
                && t.charge == 0 =>
        {
            Some(t)
        }
        _ => None,
    }
}

/// Examine the natural loops formed by charged edges, following checkpoint
/// splits to their actual SSA header.
pub(super) fn discover(func: &Func) -> Vec<(u32, Result<Loop, Reject>)> {
    let dom = dominators(func);
    let defs = Defs::new(func);
    let mut incoming = vec![Vec::new(); func.blocks.len()];
    let mut back = vec![Vec::new(); func.blocks.len()];
    for (i, block) in func.blocks.iter().enumerate() {
        for edge in block.end.targets() {
            let to = edge.block.0 as usize;
            incoming[to].push(i);
            if edge.charge > 0 && func.blocks[to].head.is_some() {
                let target = through(func, edge.block).unwrap_or(edge);
                let head = target.block.0 as usize;
                if dominates(&dom, head, i) {
                    back[head].push((i, edge, target));
                }
            }
        }
    }

    back.into_iter()
        .enumerate()
        .filter_map(|(head, edges)| {
            let &(latch, edge, target) = edges.first()?;
            let record = func.blocks[edge.block.0 as usize].head.unwrap().record;
            let pc = func.records[record.0 as usize].eip;
            let shape = if edges.len() == 1 {
                classify(func, &defs, &incoming, head, latch, edge, target)
            } else {
                Err(Reject::Control)
            };
            Some((pc, shape))
        })
        .collect()
}

fn classify(
    func: &Func,
    defs: &Defs,
    incoming: &[Vec<usize>],
    head: usize,
    latch: usize,
    edge: &Target,
    back: &Target,
) -> Result<Loop, Reject> {
    let mut member = vec![false; func.blocks.len()];
    member[head] = true;
    let mut work = vec![latch];
    while let Some(b) = work.pop() {
        if !std::mem::replace(&mut member[b], true) {
            work.extend(&incoming[b]);
        }
    }
    let mut indegree: Vec<_> = incoming
        .iter()
        .map(|from| from.iter().filter(|&&p| member[p]).count())
        .collect();
    work.push(head);
    let mut blocks = Vec::new();
    while let Some(b) = work.pop() {
        blocks.push(Block(b as u32));
        for t in func.blocks[b]
            .end
            .targets()
            .filter(|&t| !std::ptr::eq(t, edge))
        {
            let to = t.block.0 as usize;
            if member[to] {
                indegree[to] -= 1;
                if indegree[to] == 0 {
                    work.push(to);
                }
            } else if b != latch {
                return Err(Reject::Control);
            }
        }
    }
    if blocks.len() != member.iter().filter(|&&b| b).count() {
        return Err(Reject::Control);
    }

    let (flag_carried, carried): (Vec<_>, Vec<_>) = func.blocks[head]
        .params
        .iter()
        .copied()
        .zip(back.args.iter().copied())
        .partition(|(p, _)| func.flag_params.contains(p));
    let ivs: Vec<_> = carried
        .iter()
        .filter_map(|&(p, a)| {
            if func.ty(p) != Type::I32 {
                return None;
            }
            let affine = defs.affine(a);
            (affine.base == p && affine.offset != 0).then_some(Iv {
                base: p,
                stride: affine.offset as i32,
            })
        })
        .collect();

    let stride = |base| ivs.iter().find(|iv| iv.base == base).map(|iv| iv.stride);
    let End::Branch { cond, taken, .. } = &func.blocks[latch].end else {
        return Err(Reject::Latch);
    };
    let Some(Op::Test { flags, mut cond }) = defs.op(*cond) else {
        return Err(Reject::Latch);
    };
    let latch_taken = std::ptr::eq(taken, edge);
    if !latch_taken {
        cond = invert(cond).ok_or(Reject::Latch)?;
    }
    let Some(Op::Flags {
        kind,
        width: 4,
        a,
        b,
        ..
    }) = defs.op(flags)
    else {
        return Err(Reject::Latch);
    };
    let sign = matches!(cond, Cc::s | Cc::ns);
    let (mut value, mut bound) = if sign {
        if !func.resident {
            return Err(Reject::Latch);
        }
        cond = if cond == Cc::s { Cc::l } else { Cc::ge };
        // SF describes the wrapped result. Ordered comparisons of the
        // operands would instead include the subtraction's overflow bit.
        let value = match kind {
            FlagKind::Sub | FlagKind::Add => {
                let rhs = defs.constant(b).ok_or(Reject::Latch)? as u32;
                let offset = if kind == FlagKind::Sub {
                    rhs.wrapping_neg()
                } else {
                    rhs
                };
                defs.affine(a).shifted(offset)
            }
            FlagKind::Increment => defs.affine(a).shifted(1),
            FlagKind::Decrement => defs.affine(a).shifted(u32::MAX),
            FlagKind::Logic => defs.affine(a),
            _ => return Err(Reject::Latch),
        };
        (value, None)
    } else {
        match kind {
            FlagKind::Sub => (defs.affine(a), Some(b)),
            FlagKind::Increment | FlagKind::Decrement if matches!(cond, Cc::e | Cc::ne) => {
                let offset = if kind == FlagKind::Increment {
                    1
                } else {
                    u32::MAX
                };
                (defs.affine(a).shifted(offset), None)
            }
            FlagKind::Logic if matches!(cond, Cc::e | Cc::ne | Cc::l | Cc::le | Cc::g | Cc::ge) => {
                (defs.affine(a), None)
            }
            FlagKind::Add if matches!(cond, Cc::e | Cc::ne) => {
                let n = defs.constant(b).ok_or(Reject::Latch)? as u32;
                (defs.affine(a).shifted(n), None)
            }
            _ => return Err(Reject::Latch),
        }
    };
    if stride(value.base).is_none()
        && let Some(rhs) = bound
    {
        (value, bound) = (defs.affine(rhs), Some(a));
        cond = swap(cond).ok_or(Reject::Latch)?;
    }
    if bound.is_some_and(|v| !defs.invariant(&blocks, v)) || invert(cond).is_none() {
        return Err(Reject::Latch);
    }
    let trip = Trip {
        value,
        bound,
        stride: stride(value.base).ok_or(Reject::Induction)?,
        again: cond,
        sign,
    };

    let mut streams = Vec::new();
    for &b in &blocks {
        for &(result, op) in &func.blocks[b.0 as usize].insts {
            let (addr, width, access) = match op {
                Op::Load { clock: true, .. }
                | Op::Load { scope: Some(_), .. }
                | Op::Store { scope: Some(_), .. }
                | Op::ScopeWritten(_)
                | Op::FrameStore { .. } => {
                    return Err(Reject::Access);
                }
                Op::Load { checked: true, .. } | Op::Store { checked: true, .. }
                    if !func.resident =>
                {
                    return Err(Reject::Access);
                }
                Op::Load { addr, width, .. } => (addr, width, Access::Load(result.unwrap())),
                Op::Store {
                    addr, width, value, ..
                } => (addr, width, Access::Store(value)),
                _ => continue,
            };
            let addr = defs.affine(addr);
            streams.push(Stream {
                addr,
                stride: stride(addr.base)
                    .or_else(|| {
                        (func.resident
                            && matches!(access, Access::Load(_))
                            && (defs.invariant(&blocks, addr.base)
                                || carried.contains(&(addr.base, addr.base))))
                        .then_some(0)
                    })
                    .ok_or(Reject::Address)?,
                width,
                access,
            });
        }
    }
    Ok(Loop {
        head: Block(head as u32),
        eip: func.records[func.blocks[edge.block.0 as usize].head.unwrap().record.0 as usize].eip,
        latch: Block(latch as u32),
        latch_taken,
        blocks,
        carried,
        flag_carried,
        ivs,
        trip,
        streams,
        charge: edge.charge as u32,
    })
}

pub(super) fn invert(cc: Cc) -> Option<Cc> {
    Some(match cc {
        Cc::e => Cc::ne,
        Cc::ne => Cc::e,
        Cc::b => Cc::ae,
        Cc::ae => Cc::b,
        Cc::be => Cc::a,
        Cc::a => Cc::be,
        Cc::l => Cc::ge,
        Cc::ge => Cc::l,
        Cc::le => Cc::g,
        Cc::g => Cc::le,
        Cc::s => Cc::ns,
        Cc::ns => Cc::s,
        _ => return None,
    })
}

pub(super) fn swap(cc: Cc) -> Option<Cc> {
    Some(match cc {
        Cc::e | Cc::ne => cc,
        Cc::b => Cc::a,
        Cc::ae => Cc::be,
        Cc::be => Cc::ae,
        Cc::a => Cc::b,
        Cc::l => Cc::g,
        Cc::ge => Cc::le,
        Cc::le => Cc::ge,
        Cc::g => Cc::l,
        _ => return None,
    })
}

/// Kernel recognition sees the original, verified SSA and the natural loop.
/// A kernel body runs only inside a checked batch region.
pub(super) trait LoopKernel {
    fn recognize(&self, func: &Func, loop_: &Loop) -> Option<Plan>;
}

pub(super) trait BatchBody {
    fn lower_preheader(&self, _emitter: &mut BatchEmitter<'_, '_>) {}
    fn lower(&self, emitter: &mut BatchEmitter<'_, '_>);
    fn has_unroll2(&self) -> bool {
        false
    }
    fn lower_unroll2(&self, _emitter: &mut BatchEmitter<'_, '_>) {}
    fn lower_epilogue(&self, _emitter: &mut BatchEmitter<'_, '_>) {}
    fn is_float(&self) -> bool {
        false
    }
}

pub(super) struct Plan {
    pub name: &'static str,
    pub lanes: u8,
    pub inputs: Vec<Value>,
    pub conditional: bool,
    pub body: Box<dyn BatchBody>,
}

#[derive(Clone, Copy)]
pub(super) struct Span {
    pub start: Affine,
    pub width: u32,
    pub step: u32,
    pub write: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub start: Affine,
    pub bytes: u32,
}

pub(super) struct Batch {
    pub loop_: Loop,
    pub plan: Plan,
    pub spans: Vec<Span>,
    pub backwards: bool,
    pub alias: Vec<(usize, usize, bool)>,
    pub frame: Vec<Frame>,
    pub crossings: Vec<Stream>,
    pub prefix_charge: u64,
    pub recovery: Vec<(Value, Affine)>,
}

impl Batch {
    pub fn priced(&self) -> bool {
        self.plan.conditional
            || self.loop_.trip.sign
            || self.loop_.streams.iter().any(|s| s.stride == 0)
    }

    pub fn inputs(&self) -> impl Iterator<Item = Value> + '_ {
        self.loop_
            .ivs
            .iter()
            .map(|iv| iv.base)
            .chain(self.recovery.iter().map(|(_, a)| a.base))
            .chain(self.loop_.trip.bound)
            .chain(
                self.spans
                    .iter()
                    .filter(|s| s.step == 0)
                    .map(|s| s.start.base),
            )
            .chain(self.frame.iter().map(|f| f.start.base))
            .chain(self.plan.inputs.iter().copied())
    }

    fn new(func: &Func, loop_: Loop, plan: Plan) -> Option<Self> {
        let mut groups = loop_.groups();
        if plan.conditional {
            let mut stores = Vec::new();
            groups.retain(|group| {
                let stream = group[0].1;
                if !stream.write() {
                    return !group.iter().all(|&(_, s)| loop_.covering_read(s).is_some());
                }
                let range = (stream.addr, stream.stride, stream.width);
                if stores.contains(&range) {
                    return false;
                }
                stores.push(range);
                true
            });
        }
        let backwards = groups
            .iter()
            .map(|g| g[0].1.stride)
            .find(|&stride| stride != 0)
            .is_some_and(|stride| stride < 0);
        let spans = groups
            .iter()
            .map(|group| {
                let (_, s) = group[0];
                let width = u32::from(s.width) * group.len() as u32;
                let stride = s.stride.unsigned_abs();
                if stride != 0 && ((s.stride < 0) != backwards || !func.resident && width != stride)
                {
                    return None;
                }
                Some(Span {
                    start: s.addr,
                    width,
                    step: stride.checked_mul(u32::from(plan.lanes))?,
                    write: s.write(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let mut alias = Vec::new();
        for (a, group) in groups.iter().enumerate() {
            for (b, other) in groups.iter().enumerate().skip(a + 1) {
                if !spans[a].write && !spans[b].write {
                    continue;
                }
                let (reads, writes) = if spans[a].write {
                    (other, group)
                } else {
                    (group, other)
                };
                let in_place = plan.conditional
                    && !reads[0].1.write()
                    && reads[0].1.stride != 0
                    && spans[a].step == spans[b].step
                    && spans[a].width == spans[b].width
                    || reads.len() == writes.len()
                        && reads.iter().zip(writes).all(|(&(r, read), &(w, write))| {
                            !read.write()
                                && r < w
                                && read.stride == write.stride
                                && read.width == write.width
                                && u32::from(read.width) <= read.stride.unsigned_abs()
                        });
                if !in_place || spans[a].start != spans[b].start {
                    alias.push((a, b, in_place));
                }
            }
        }

        let defs = Defs::new(func);
        let mut recovery = Vec::new();
        for &(param, arg) in &loop_.flag_carried {
            if param == arg {
                continue;
            }
            if func.ty(param) != Type::I32 {
                return None;
            }
            let mut value = defs.affine(arg);
            if let Some(iv) = loop_.ivs.iter().find(|iv| iv.base == value.base) {
                value.offset = value.offset.wrapping_sub(iv.stride as u32);
            } else if !defs.invariant(&loop_.blocks, value.base) {
                return None;
            }
            recovery.push((param, value));
        }
        let mut frame = Vec::new();
        let mut crossings = Vec::new();
        for block in &loop_.blocks {
            for &(_, op) in &func.blocks[block.0 as usize].insts {
                match op {
                    Op::FrameDisjoint { base, lo, hi, .. } => {
                        if !defs.invariant(&loop_.blocks, base) {
                            return None;
                        }
                        let guard = Frame {
                            start: Affine {
                                base,
                                offset: lo as u32,
                            },
                            bytes: (i64::from(hi) - i64::from(lo)) as u32,
                        };
                        if !frame
                            .iter()
                            .any(|f: &Frame| f.start == guard.start && f.bytes == guard.bytes)
                        {
                            frame.push(guard);
                        }
                    }
                    Op::PageCross { addr, .. } => {
                        let addr = defs.affine(addr);
                        crossings.extend(
                            loop_
                                .streams
                                .iter()
                                .copied()
                                .filter(|s| s.write() && s.addr == addr),
                        );
                    }
                    _ => {}
                }
            }
        }
        // A positive internal edge is a scalar checkpoint. Cover the
        // largest possible debit, including internal edges of the tail.
        let prefix_charge = loop_
            .blocks
            .iter()
            .filter(|&&b| b != loop_.latch)
            .flat_map(|b| func.blocks[b.0 as usize].end.targets())
            .map(|target| target.charge.max(0) as u64)
            .sum();
        Some(Self {
            loop_,
            plan,
            spans,
            backwards,
            alias,
            frame,
            crossings,
            prefix_charge,
            recovery,
        })
    }
}

pub(super) fn plan(func: &Func, kernels: &[&dyn LoopKernel]) -> Vec<Batch> {
    let mut batches = Vec::new();
    for (head, result) in discover(func) {
        let batch = result.and_then(|loop_| {
            let plan = kernels
                .iter()
                .find_map(|kernel| kernel.recognize(func, &loop_));
            plan.and_then(|plan| Batch::new(func, loop_, plan))
                .ok_or(Reject::Kernel)
        });
        match batch {
            Ok(batch) => batches.push(batch),
            Err(reason) => crate::jit_map::loop_reject(head, format_args!("{reason:?}")),
        }
    }
    batches
}

#[cfg(test)]
mod tests;
