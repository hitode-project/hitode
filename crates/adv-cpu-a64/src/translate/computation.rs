//! Memory effects of detached translated computation. Expressions model only
//! address-bearing integer values. Native instruction lowering still owns all
//! results, flags, exceptional exits and floating-point state.

use std::collections::{BTreeMap, VecDeque};

use adv_cpu_core::CpuState;
use adv_cpu_core::floating::fast::{Fast, Source};
use iced_x86::ConditionCode as Cond;
use rustc_hash::FxHashMap;

use super::decode::{Alu, Cc, EBP, ESP, Mem, Narrow8or16, Op, Part, ShiftKind, Src};
use super::integer::Integer;
use super::simd::{Kind, Loc};
use super::trace::{Edge, Mode, Next, Trace};

type ExprId = usize;
type VarId = usize;

// Admission performs bounded work on the owner, including parameter reads.
const PROOF_STEPS: usize = 128 * 1024;
const PARAMETER_WORDS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Expr {
    Constant(u32),
    Register(u8),
    Word(ExprId),
    Add(ExprId, ExprId),
    Mul(ExprId, ExprId),
    Neg(ExprId),
}

// An affine sum in the guest's modulo-2^32 address arithmetic. Coefficients
// are invariant within the segment; loop indices and remainders are not.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Form {
    base: ExprId,
    terms: Vec<(VarId, ExprId)>,
}

impl Form {
    fn invariant(&self) -> Option<ExprId> {
        self.terms.is_empty().then_some(self.base)
    }
}

type Value = Option<Form>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Limit {
    var: VarId,
    // Narrow an unsigned normalized remainder from n-1 to n-2.
    trim: u32,
}

#[derive(Clone, Debug)]
enum Var {
    Counter(Option<ExprId>),
    Remainder { divisor: ExprId, normalized: bool },
    Either([(Form, Vec<Limit>); 2]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    Read,
    FrameRead,
    FrameWrite,
    Write,
}

#[derive(Clone, Debug)]
struct Effect {
    addr: Form,
    limits: Vec<Limit>,
    access: Access,
    bytes: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    Equal(ExprId, ExprId),
    Positive(ExprId, u32),
    StackAlignment(u32),
}

/// An ordinary guest span, still to be checked against the memory authority
/// and the active jobs. The exclusive end may equal 2^32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Range {
    pub start: u32,
    pub end: u64,
    pub write: bool,
}

impl Range {
    fn intersects(self, other: Self) -> bool {
        u64::from(self.start) < other.end && u64::from(other.start) < self.end
    }
}

#[derive(Default)]
pub(crate) struct Evaluation {
    values: Vec<Option<u32>>,
    words: Vec<(u32, u32)>,
    ranges: Vec<Range>,
    parameters: Vec<Range>,
    outputs: Vec<Range>,
    frame: Vec<Range>,
}

pub(crate) struct Proof {
    exprs: Vec<Expr>,
    vars: Vec<Var>,
    checks: Vec<Check>,
    effects: Vec<Effect>,
    divisions: Vec<Option<Division>>,
    frame_cache: Option<FrameCache>,
}

pub(crate) struct FrameCache {
    pub head: usize,
    /// Byte displacements from ESP on entry to the loop.
    pub offsets: Vec<i32>,
    reads: Vec<Option<u8>>,
}

impl FrameCache {
    pub(crate) fn read(&self, index: usize) -> Option<u8> {
        self.reads[index]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Division {
    /// The dividend is sign extended from EAX and the divisor is positive.
    SignExtended,
    /// EAX is a signed remainder plus its positive divisor (at most 2^30).
    Normalized,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Test {
    Compare(Form, Form),
    Zero(Form),
}

#[derive(Clone)]
struct State {
    regs: [Value; 8],
    // A missing slot has not been touched; a present None is opaque.
    slots: BTreeMap<i32, Value>,
    limits: Vec<Limit>,
    test: Option<Test>,
    cdq: bool,
    entry_reads: u8,
}

impl State {
    fn put(&mut self, reg: u8, value: Value) {
        self.regs[usize::from(reg)] = value;
        if reg == 0 || reg == 2 {
            self.cdq = false;
        }
    }
}

struct Loop {
    head: usize,
    tail: usize,
    test: usize,
    condition: Cond,
    taken: bool,
    exit: Option<usize>,
    inside: Vec<bool>,
}

struct Graph {
    edges: Vec<[Option<usize>; 2]>,
    order: Vec<usize>,
    loops: Vec<Loop>,
    heads: Vec<Option<usize>>,
}

impl Graph {
    fn build(trace: &Trace) -> Option<Self> {
        let n = trace.len();
        if n == 0 {
            return None;
        }
        let internal = |edge| match edge {
            Edge::Internal(to) => Some(to),
            _ => None,
        };
        let edges: Vec<_> = trace
            .insts
            .iter()
            .enumerate()
            .map(|(at, inst)| {
                let fall = match inst.next {
                    Next::Fall => Some(at + 1),
                    Next::Edge(edge) => internal(edge),
                    Next::Dynamic => None,
                };
                [fall, inst.taken.and_then(internal)]
            })
            .collect();
        let mut incoming = vec![Vec::new(); n];
        for (from, edges) in edges.iter().enumerate() {
            for &to in edges.iter().flatten() {
                incoming.get_mut(to)?.push(from);
            }
        }
        fn visit(
            edges: &[[Option<usize>; 2]],
            at: usize,
            color: &mut [u8],
            back: &mut Vec<(usize, usize)>,
        ) {
            color[at] = 1;
            for &to in edges[at].iter().flatten() {
                match color[to] {
                    0 => visit(edges, to, color, back),
                    1 => back.push((at, to)),
                    _ => {}
                }
            }
            color[at] = 2;
        }
        let mut back = Vec::new();
        visit(&edges, 0, &mut vec![0; n], &mut back);
        if back.is_empty() {
            return None;
        }
        let mut loops = Vec::new();
        let mut heads = vec![None; n];
        for &(tail, head) in &back {
            if heads[head].is_some() {
                return None;
            }
            let mut inside = vec![false; n];
            inside[head] = true;
            let mut pending = vec![tail];
            while let Some(at) = pending.pop() {
                if !std::mem::replace(&mut inside[at], true) {
                    pending.extend(&incoming[at]);
                }
            }
            // Only the head may be entered from outside the loop.
            if (head != 0 && inside[0])
                || (0..n).any(|i| inside[i] && i != head && incoming[i].iter().any(|&p| !inside[p]))
            {
                return None;
            }
            let mut test = tail;
            let (condition, taken, exit) = loop {
                let inst = trace.insts[test];
                if let Op::Jcc(Cc::Flags(condition), _) = inst.op {
                    let [fall, taken] = edges[test];
                    match (
                        fall.is_some_and(|i| inside[i]),
                        taken.is_some_and(|i| inside[i]),
                    ) {
                        (true, false) => break (condition, false, taken),
                        (false, true) => break (condition, true, fall),
                        _ => {}
                    }
                }
                let [prev] = incoming[test].as_slice() else {
                    return None;
                };
                if *prev == head || !inside[*prev] {
                    return None;
                }
                test = *prev;
            };
            // A conditional latch may branch to a short appended tail before
            // reentering its head. Other edges may leave only for the owner.
            if (0..n)
                .any(|i| inside[i] && i != test && edges[i].iter().flatten().any(|&to| !inside[to]))
            {
                return None;
            }
            heads[head] = Some(loops.len());
            loops.push(Loop {
                head,
                tail,
                test,
                condition,
                taken,
                exit,
                inside,
            });
        }
        for (i, a) in loops.iter().enumerate() {
            for b in &loops[..i] {
                if a.inside.iter().zip(&b.inside).any(|(&a, &b)| a && b)
                    && a.inside.iter().zip(&b.inside).any(|(&a, &b)| a && !b)
                    && a.inside.iter().zip(&b.inside).any(|(&a, &b)| b && !a)
                {
                    return None;
                }
            }
        }
        let mut indegree: Vec<_> = incoming.iter().map(Vec::len).collect();
        for &(_, head) in &back {
            indegree[head] -= 1;
        }
        let mut queue: VecDeque<_> = (0..n).filter(|&i| indegree[i] == 0).collect();
        let mut order = Vec::with_capacity(n);
        while let Some(at) = queue.pop_front() {
            order.push(at);
            for &to in edges[at].iter().flatten() {
                if !back.contains(&(at, to)) {
                    indegree[to] -= 1;
                    if indegree[to] == 0 {
                        queue.push_back(to);
                    }
                }
            }
        }
        (order.len() == n).then_some(Self {
            edges,
            order,
            loops,
            heads,
        })
    }
}

/// Find the outermost returning loop in a prepared trace. A nominated site
/// still needs its own trace and proof before it can be admitted.
pub(crate) fn candidate(trace: &Trace) -> Option<u32> {
    trace.insts[..trace.primary]
        .iter()
        .enumerate()
        .flat_map(|(from, inst)| {
            [
                inst.taken,
                match inst.next {
                    Next::Edge(edge) => Some(edge),
                    _ => None,
                },
            ]
            .into_iter()
            .filter_map(move |edge| match edge {
                Some(Edge::Internal(to)) if to <= from => Some(to),
                _ => None,
            })
        })
        .min()
        .map(|head| trace.insts[head].pc)
}

struct Builder<'a> {
    trace: &'a Trace,
    graph: &'a Graph,
    exprs: Vec<Expr>,
    interned: FxHashMap<Expr, ExprId>,
    vars: Vec<Var>,
    effects: Vec<(usize, Effect)>,
    checks: Vec<Check>,
    divisions: Vec<(usize, Option<Division>)>,
    frame_heads: Vec<(usize, Option<i32>)>,
    at: usize,
    zero: ExprId,
    one: ExprId,
    remaining: usize,
    states: Vec<Vec<Option<State>>>,
}

impl Builder<'_> {
    fn expr(&mut self, expr: Expr) -> ExprId {
        *self.interned.entry(expr).or_insert_with(|| {
            let id = self.exprs.len();
            self.exprs.push(expr);
            id
        })
    }

    fn constant(&mut self, bits: u32) -> ExprId {
        self.expr(Expr::Constant(bits))
    }

    fn neg(&mut self, a: ExprId) -> ExprId {
        match self.exprs[a] {
            Expr::Constant(a) => self.constant(a.wrapping_neg()),
            Expr::Neg(a) => a,
            _ => self.expr(Expr::Neg(a)),
        }
    }

    fn add(&mut self, a: ExprId, b: ExprId) -> ExprId {
        if a == self.zero || b == self.zero {
            return if a == self.zero { b } else { a };
        }
        fn collect(
            exprs: &[Expr],
            id: ExprId,
            positive: bool,
            constant: &mut u32,
            terms: &mut BTreeMap<ExprId, i32>,
        ) {
            match exprs[id] {
                Expr::Constant(v) => {
                    *constant = if positive {
                        constant.wrapping_add(v)
                    } else {
                        constant.wrapping_sub(v)
                    };
                }
                Expr::Add(a, b) => {
                    collect(exprs, a, positive, constant, terms);
                    collect(exprs, b, positive, constant, terms);
                }
                Expr::Neg(a) => collect(exprs, a, !positive, constant, terms),
                _ => *terms.entry(id).or_default() += if positive { 1 } else { -1 },
            }
        }
        let mut constant = 0;
        let mut terms = BTreeMap::new();
        collect(&self.exprs, a, true, &mut constant, &mut terms);
        collect(&self.exprs, b, true, &mut constant, &mut terms);
        let mut result = self.constant(constant);
        for (term, count) in terms {
            if count != 0 {
                let count = self.constant(count as u32);
                let term = self.mul(term, count);
                result = if result == self.zero {
                    term
                } else {
                    self.expr(Expr::Add(result, term))
                };
            }
        }
        result
    }

    fn mul(&mut self, a: ExprId, b: ExprId) -> ExprId {
        if a == self.zero || b == self.zero {
            return self.zero;
        }
        if a == self.one || b == self.one {
            return if a == self.one { b } else { a };
        }
        match (self.exprs[a], self.exprs[b]) {
            (Expr::Constant(a), Expr::Constant(b)) => self.constant(a.wrapping_mul(b)),
            (Expr::Constant(u32::MAX), _) => self.neg(b),
            (_, Expr::Constant(u32::MAX)) => self.neg(a),
            (Expr::Neg(a), _) => {
                let p = self.mul(a, b);
                self.neg(p)
            }
            (_, Expr::Neg(b)) => {
                let p = self.mul(a, b);
                self.neg(p)
            }
            _ => self.expr(Expr::Mul(a.min(b), a.max(b))),
        }
    }

    fn form(&self, base: ExprId) -> Form {
        Form {
            base,
            terms: Vec::new(),
        }
    }

    fn imm(&mut self, bits: u32) -> Form {
        let base = self.constant(bits);
        self.form(base)
    }

    fn variable(&mut self, var: Var) -> VarId {
        let id = self.vars.len();
        self.vars.push(var);
        id
    }

    fn term(&self, var: VarId) -> Form {
        Form {
            base: self.zero,
            terms: vec![(var, self.one)],
        }
    }

    fn plus(&mut self, mut a: Form, b: Form) -> Form {
        a.base = self.add(a.base, b.base);
        for (var, coefficient) in b.terms {
            match a.terms.binary_search_by_key(&var, |&(v, _)| v) {
                Ok(i) => {
                    let coefficient = self.add(a.terms[i].1, coefficient);
                    if coefficient == self.zero {
                        a.terms.remove(i);
                    } else {
                        a.terms[i].1 = coefficient;
                    }
                }
                Err(i) => a.terms.insert(i, (var, coefficient)),
            }
        }
        a
    }

    fn scale(&mut self, mut form: Form, factor: ExprId) -> Form {
        form.base = self.mul(form.base, factor);
        for (_, c) in &mut form.terms {
            *c = self.mul(*c, factor);
        }
        form.terms.retain(|&(_, c)| c != self.zero);
        form
    }

    fn minus(&mut self, a: Form, b: Form) -> Form {
        let m = self.constant(u32::MAX);
        let b = self.scale(b, m);
        self.plus(a, b)
    }

    fn register(&self, state: &mut State, reg: u8) -> Value {
        let value = state.regs[usize::from(reg)].clone();
        if let Some(Expr::Register(entry)) = value
            .as_ref()
            .and_then(Form::invariant)
            .map(|id| self.exprs[id])
        {
            state.entry_reads |= 1 << entry;
        }
        value
    }

    fn address(&mut self, state: &mut State, mem: Mem) -> Value {
        if mem.address_bits != 32 || mem.fs {
            return None;
        }
        let mut a = self.imm(mem.disp);
        if let Some(base) = mem.base {
            a = self.plus(a, self.register(state, base)?);
        }
        if let Some(index) = mem.index {
            let s = self.constant(mem.scale);
            let index = self.scale(self.register(state, index)?, s);
            a = self.plus(a, index);
        }
        Some(a)
    }

    fn frame_offset(&self, form: &Form) -> Option<i32> {
        match self.exprs[form.invariant()?] {
            Expr::Register(ESP) => Some(0),
            Expr::Add(a, b) => match (self.exprs[a], self.exprs[b]) {
                (Expr::Register(ESP), Expr::Constant(c))
                | (Expr::Constant(c), Expr::Register(ESP)) => Some(c as i32),
                _ => None,
            },
            _ => None,
        }
    }

    fn frame_address(&mut self, offset: i32) -> Form {
        let sp = self.expr(Expr::Register(ESP));
        let offset = self.constant(offset as u32);
        let base = self.add(sp, offset);
        self.form(base)
    }

    fn check(&mut self, check: Check) {
        if !self.checks.contains(&check) {
            self.checks.push(check);
        }
    }

    fn word(&mut self, addr: ExprId) -> Form {
        let value = self.expr(Expr::Word(addr));
        self.form(value)
    }

    fn frame_word(&mut self, offset: i32) -> Form {
        let addr = self.frame_address(offset);
        self.word(addr.base)
    }

    fn read(&mut self, state: &mut State, addr: Form, bytes: u8, record: bool) -> Value {
        let frame = self.frame_offset(&addr);
        if record {
            self.effects.push((
                self.at,
                Effect {
                    addr: addr.clone(),
                    limits: state.limits.clone(),
                    access: if frame.is_some() {
                        Access::FrameRead
                    } else {
                        Access::Read
                    },
                    bytes: u32::from(bytes),
                },
            ));
        }
        if bytes != 4 {
            return None;
        }
        if let Some(offset) = frame
            && let Some(value) = state.slots.get(&offset)
        {
            return value.clone();
        }
        Some(self.word(addr.invariant()?))
    }

    fn write(&mut self, state: &mut State, addr: Form, bytes: u8, value: Value, record: bool) {
        let frame = self.frame_offset(&addr);
        if let Some(offset) = frame {
            // An opaque store poisons every overlapping dword, including a
            // read at an offset that has not appeared yet.
            for delta in -3..i32::from(bytes) {
                state.slots.insert(offset.wrapping_add(delta), None);
            }
            if bytes == 4 {
                state.slots.insert(offset, value);
            }
        }
        if record {
            self.effects.push((
                self.at,
                Effect {
                    addr,
                    limits: state.limits.clone(),
                    access: if frame.is_some() {
                        Access::FrameWrite
                    } else {
                        Access::Write
                    },
                    bytes: u32::from(bytes),
                },
            ));
        }
    }

    fn src(&mut self, state: &mut State, src: Src, record: bool) -> Option<Value> {
        Some(match src {
            Src::Reg(reg) => self.register(state, reg),
            Src::Imm(bits) => Some(self.imm(bits)),
            Src::Mem(mem) => {
                let addr = self.address(state, mem)?;
                self.read(state, addr, 4, record)
            }
        })
    }

    fn put(&mut self, state: &mut State, dst: Src, value: Value, record: bool) -> Option<()> {
        match dst {
            Src::Reg(reg) => state.put(reg, value),
            Src::Mem(mem) => {
                let addr = self.address(state, mem)?;
                self.write(state, addr, 4, value, record);
            }
            Src::Imm(_) => return None,
        }
        Some(())
    }

    fn part(&mut self, state: &mut State, part: Part, bytes: u8, record: bool) -> Option<Value> {
        Some(match part {
            Part::Mem(mem) => {
                let addr = self.address(state, mem)?;
                self.read(state, addr, bytes, record)
            }
            Part::Reg(reg) if bytes == 4 => self.register(state, reg),
            Part::Imm(bits) => Some(self.imm(bits)),
            Part::Reg(_) => None,
        })
    }

    fn put_part(&mut self, state: &mut State, part: Part, bytes: u8, record: bool) -> Option<()> {
        match part {
            Part::Mem(mem) => {
                let addr = self.address(state, mem)?;
                self.write(state, addr, bytes, None, record);
            }
            Part::Reg(reg) => state.put(if bytes == 1 { reg & 3 } else { reg }, None),
            Part::Imm(_) => return None,
        }
        Some(())
    }

    fn push(&mut self, state: &mut State, value: Value, record: bool) -> Option<()> {
        let sp = self.register(state, ESP)?;
        let delta = self.imm((-4i32) as u32);
        let sp = self.plus(sp, delta);
        self.frame_offset(&sp)?;
        self.write(state, sp.clone(), 4, value, record);
        state.put(ESP, Some(sp));
        Some(())
    }

    fn pop(&mut self, state: &mut State, record: bool) -> Option<Value> {
        let sp = self.register(state, ESP)?;
        self.frame_offset(&sp)?;
        let value = self.read(state, sp.clone(), 4, record);
        let delta = self.imm(4);
        state.put(ESP, Some(self.plus(sp, delta)));
        Some(value)
    }

    fn alu(&mut self, op: Alu, a: Value, b: Value) -> Value {
        let (a, b) = (a?, b?);
        Some(match op {
            Alu::Add => self.plus(a, b),
            Alu::Sub => self.minus(a, b),
            Alu::Xor if a == b => self.form(self.zero),
            _ => {
                let (Expr::Constant(a), Expr::Constant(b)) =
                    (self.exprs[a.invariant()?], self.exprs[b.invariant()?])
                else {
                    return None;
                };
                self.imm(match op {
                    Alu::And => a & b,
                    Alu::Or => a | b,
                    Alu::Xor => a ^ b,
                    _ => return None,
                })
            }
        })
    }

    fn operation(&mut self, at: usize, state: &mut State, record: bool) -> Option<()> {
        self.remaining = self.remaining.checked_sub(1)?;
        self.at = at;
        if record && self.graph.heads[at].is_some() {
            let offset = state.regs[usize::from(ESP)]
                .as_ref()
                .and_then(|sp| self.frame_offset(sp));
            self.frame_heads.push((at, offset));
        }
        let inst = self.trace.insts[at];
        if inst.locked.is_some() || inst.call.is_some() || inst.spin {
            return None;
        }
        if inst.effects.writes != super::flags::FlagSet::NONE {
            state.test = None;
        }
        match inst.op {
            Op::Nop | Op::Jmp(_) | Op::Jcc(Cc::Flags(_), _) => {}
            Op::Mov(dst, src) => {
                let value = self.src(state, src, record)?;
                self.put(state, dst, value, record)?;
            }
            Op::Lea(dst, mem) => {
                let value = self.address(state, mem);
                state.put(dst, value);
            }
            Op::Alu(Alu::Xor, Src::Reg(dst), Src::Reg(src)) if dst == src => {
                let zero = self.form(self.zero);
                state.test = Some(Test::Zero(zero.clone()));
                state.put(dst, Some(zero));
            }
            Op::Alu(op, dst, src) => {
                let a = self.src(state, dst, record)?;
                let b = self.src(state, src, record)?;
                if op == Alu::Cmp {
                    state.test = a.zip(b).map(|(a, b)| Test::Compare(a, b));
                } else if op != Alu::Test {
                    let stack_align = match (&a, &b, op, dst) {
                        (Some(a), Some(b), Alu::And, Src::Reg(ESP)) => {
                            let offset = self.frame_offset(a)?;
                            let Expr::Constant(mask) = self.exprs[b.invariant()?] else {
                                return None;
                            };
                            let align = mask.wrapping_neg();
                            if !align.is_power_of_two() {
                                return None;
                            }
                            self.check(Check::StackAlignment(align));
                            Some(self.frame_address(offset & mask as i32))
                        }
                        _ => None,
                    };
                    let value = stack_align.or_else(|| self.alu(op, a, b));
                    state.test = value.clone().map(Test::Zero);
                    self.put(state, dst, value, record)?;
                }
            }
            Op::Step(up, reg) => {
                let inc = self.imm(if up { 1 } else { u32::MAX });
                let value = state.regs[usize::from(reg)]
                    .clone()
                    .map(|a| self.plus(a, inc));
                state.test = value.clone().map(Test::Zero);
                state.put(reg, value);
            }
            Op::StepMemory(up, mem) => {
                let addr = self.address(state, mem)?;
                let value = self.read(state, addr.clone(), 4, record);
                let inc = self.imm(if up { 1 } else { u32::MAX });
                let value = value.map(|a| self.plus(a, inc));
                state.test = value.clone().map(Test::Zero);
                self.write(state, addr, 4, value, record);
            }
            Op::Not(reg) | Op::Neg(reg) => {
                let m = self.constant(u32::MAX);
                let value = state.regs[usize::from(reg)]
                    .clone()
                    .map(|a| self.scale(a, m));
                let value = if matches!(inst.op, Op::Not(_)) {
                    let minus_one = self.form(m);
                    value.map(|a| self.plus(a, minus_one))
                } else {
                    state.test = value.clone().map(Test::Zero);
                    value
                };
                state.put(reg, value);
            }
            Op::Imul(dst, src, imm) => {
                let left = if let Some(imm) = imm {
                    Some(self.imm(imm))
                } else {
                    self.register(state, dst)
                };
                let right = self.src(state, src, record)?;
                let value = left.zip(right).and_then(|(l, r)| {
                    if let Some(i) = l.invariant() {
                        Some(self.scale(r, i))
                    } else {
                        Some(self.scale(l, r.invariant()?))
                    }
                });
                state.put(dst, value);
            }
            Op::Shift(kind, dst, count) => {
                let value = self.src(state, dst, record)?;
                let value = match (kind, value, count) {
                    (ShiftKind::Shl, Some(a), Some(n)) => {
                        let k = self.constant(1u32 << n);
                        Some(self.scale(a, k))
                    }
                    _ => None,
                };
                self.put(state, dst, value, record)?;
            }
            Op::Cdq => {
                state.put(2, None);
                state.cdq = true;
            }
            Op::Integer(Integer::Divide(src, true, _)) => {
                let divisor = self.src(state, src, record)?.and_then(|x| x.invariant());
                let mut fact = None;
                let value = if let Some(divisor) = divisor.filter(|_| state.cdq) {
                    let normalized = state.regs[0].as_ref().is_some_and(|a| {
                        a.base == divisor
                            && matches!(a.terms.as_slice(), [(v, c)] if *c == self.one
                                && matches!(self.vars[*v], Var::Remainder { divisor: d, normalized: false } if d == divisor))
                    });
                    self.check(Check::Positive(
                        divisor,
                        if normalized { 1 << 30 } else { i32::MAX as u32 },
                    ));
                    fact = Some(if normalized {
                        Division::Normalized
                    } else {
                        Division::SignExtended
                    });
                    let var = self.variable(Var::Remainder {
                        divisor,
                        normalized,
                    });
                    Some(self.term(var))
                } else {
                    None
                };
                if record {
                    self.divisions.push((at, fact));
                }
                state.put(0, None);
                state.put(2, value);
                state.test = None;
            }
            Op::Carry(_, dst, src) => {
                self.src(state, src, record)?;
                state.put(dst, None);
            }
            Op::Extend(dst, source, _) => {
                if let Narrow8or16::Mem(mem, word) = source {
                    let addr = self.address(state, mem)?;
                    self.read(state, addr, if word { 2 } else { 1 }, record);
                }
                state.put(dst, None);
            }
            Op::Narrow(op, width, dst, src) => {
                if op.is_some() {
                    self.part(state, dst, width, record)?;
                }
                self.part(state, src, width, record)?;
                if !matches!(op, Some(Alu::Cmp | Alu::Test)) {
                    self.put_part(state, dst, width, record)?;
                }
            }
            Op::Push(src) => {
                let value = self.src(state, src, record)?;
                self.push(state, value, record)?;
            }
            Op::Pop(reg) if reg != ESP => {
                let value = self.pop(state, record)?;
                state.put(reg, value);
            }
            Op::Leave => {
                state.put(ESP, state.regs[usize::from(EBP)].clone());
                let value = self.pop(state, record)?;
                state.put(EBP, value);
            }
            Op::Call(target) if inst.mode != Mode::Real => {
                if self.graph.edges[at][0].is_none() && target != inst.next_pc {
                    return None;
                }
                let ret = self.imm(inst.next_pc);
                self.push(state, Some(ret), record)?;
            }
            Op::Ret(0) if matches!(inst.mode, Mode::Inline(_)) => {
                self.pop(state, record)?;
            }
            Op::X87(x) => {
                let source_bytes = |src| match src {
                    Source::St(_) => 0,
                    Source::I16 => 2,
                    Source::I32 | Source::F32 => 4,
                    Source::F64 => 8,
                };
                let (width, write) = match x.fast {
                    Fast::Load(src)
                    | Fast::Arith { source: src, .. }
                    | Fast::Compare { source: src, .. } => (source_bytes(src), false),
                    Fast::LoadControl => (2, false),
                    Fast::Store { width, .. } => (width, true),
                    Fast::StoreControl | Fast::StoreStatus => (2, true),
                    Fast::Load80 => (10, false),
                    Fast::Store80 => (10, true),
                    Fast::Leaf(_) => return None,
                    _ => (0, false),
                };
                if let Some(mem) = x.mem {
                    if width == 0 {
                        return None;
                    }
                    let addr = self.address(state, mem)?;
                    if write {
                        self.write(state, addr, width, None, record);
                    } else {
                        self.read(state, addr, width, record);
                    }
                } else if write && width == 2 {
                    state.put(0, None);
                }
            }
            Op::Fild64(mem, ..) => {
                let addr = self.address(state, mem)?;
                self.read(state, addr, 8, record);
            }
            Op::Simd(s) if !s.mmx() && s.kind != Kind::MaskMove => {
                if s.memory() {
                    let addr = self.address(state, s.mem)?;
                    if s.dst == Loc::Mem {
                        self.write(state, addr, s.width, None, record);
                    } else {
                        self.read(state, addr, s.width, record);
                    }
                }
                if let Loc::Gpr(reg) = s.dst {
                    state.put(reg, None);
                }
            }
            _ => return None,
        }
        Some(())
    }

    fn refine(&self, at: usize, taken: bool, state: &mut State) {
        let Op::Jcc(Cc::Flags(cond), _) = self.trace.insts[at].op else {
            return;
        };
        if !matches!((cond, taken), (Cond::b, true) | (Cond::ae, false)) {
            return;
        }
        let Some(Test::Compare(a, b)) = &state.test else {
            return;
        };
        let [(v, coefficient)] = a.terms.as_slice() else {
            return;
        };
        if a.base == self.one
            && *coefficient == self.one
            && matches!(self.vars[*v], Var::Remainder { divisor, normalized: true } if b.invariant() == Some(divisor))
        {
            state.limits.push(Limit { var: *v, trim: 1 });
        }
    }

    fn cached_word(&mut self, a: &Form, b: &Form) -> Option<Form> {
        let (ai, bi) = (a.invariant()?, b.invariant()?);
        match (self.exprs[ai], self.exprs[bi]) {
            (Expr::Register(_), Expr::Word(_)) => {
                self.check(Check::Equal(ai, bi));
                Some(b.clone())
            }
            (Expr::Word(_), Expr::Register(_)) => {
                self.check(Check::Equal(bi, ai));
                Some(a.clone())
            }
            _ => None,
        }
    }

    fn join_value(&mut self, a: Value, al: &[Limit], b: Value, bl: &[Limit], used: u8) -> Value {
        if a == b {
            return a;
        }
        let (a, b) = a.zip(b)?;
        if [&a, &b].into_iter().any(|v| {
            v.invariant()
                .is_some_and(|i| matches!(self.exprs[i], Expr::Register(r) if used & (1 << r) != 0))
        }) && let Some(word) = self.cached_word(&a, &b)
        {
            return Some(word);
        }
        let var = self.variable(Var::Either([(a, al.to_vec()), (b, bl.to_vec())]));
        Some(self.term(var))
    }

    fn join(&mut self, into: &mut Option<State>, mut incoming: State) {
        let Some(state) = into else {
            *into = Some(incoming);
            return;
        };
        let used = state.entry_reads | incoming.entry_reads;
        for reg in 0..8 {
            state.regs[reg] = self.join_value(
                state.regs[reg].take(),
                &state.limits,
                incoming.regs[reg].take(),
                &incoming.limits,
                used,
            );
        }
        let mut slots = std::mem::take(&mut state.slots);
        for (offset, b) in incoming.slots {
            let a = slots
                .remove(&offset)
                .unwrap_or_else(|| Some(self.frame_word(offset)));
            let value = self.join_value(a, &state.limits, b, &incoming.limits, 0);
            state.slots.insert(offset, value);
        }
        for (offset, a) in slots {
            let b = Some(self.frame_word(offset));
            let value = self.join_value(a, &state.limits, b, &incoming.limits, 0);
            state.slots.insert(offset, value);
        }
        state.cdq &= incoming.cdq;
        state.entry_reads |= incoming.entry_reads;
        if state.test != incoming.test {
            state.test = None;
        }
        state.limits.retain(|l| incoming.limits.contains(l));
    }

    // One acyclic pass, with child loops summarized instead of unrolled.
    fn walk(
        &mut self,
        start: usize,
        initial: State,
        lp: Option<usize>,
        record: bool,
    ) -> Option<Option<(State, State)>> {
        let mut states = self
            .states
            .pop()
            .unwrap_or_else(|| vec![None; self.trace.len()]);
        states[start] = Some(initial);
        let mut back = None;
        let mut exit = None;
        for &at in &self.graph.order {
            let Some(mut state) = states[at].take() else {
                continue;
            };
            if let Some(child) = self.graph.heads[at].filter(|&c| Some(c) != lp) {
                let after = self.loop_body(child, state, record)?;
                if let Some(to) = self.graph.loops[child].exit {
                    self.join(&mut states[to], after);
                }
                continue;
            }
            self.operation(at, &mut state, record)?;
            if let Some(lp) = lp {
                let info = &self.graph.loops[lp];
                if at == info.test {
                    exit = Some(state.clone());
                }
                if at == info.tail {
                    back = Some(state);
                    continue;
                }
            }
            let inside = |to| lp.is_none_or(|l| self.graph.loops[l].inside[to]);
            let [fall, taken] = self.graph.edges[at].map(|edge| edge.filter(|&to| inside(to)));
            if let Some(to) = fall {
                if taken.is_some() {
                    let mut fall = state.clone();
                    self.refine(at, false, &mut fall);
                    self.join(&mut states[to], fall);
                } else {
                    self.refine(at, false, &mut state);
                    self.join(&mut states[to], state);
                    continue;
                }
            }
            if let Some(to) = taken {
                self.refine(at, true, &mut state);
                self.join(&mut states[to], state);
            }
        }
        if states.iter().any(Option::is_some) {
            return None;
        }
        self.states.push(states);
        if lp.is_some() {
            Some(Some((back?, exit?)))
        } else {
            Some(None)
        }
    }

    fn trip(&mut self, lp: usize, test: Test, var: VarId) -> Option<ExprId> {
        let lp = &self.graph.loops[lp];
        let coefficient = |a: &Form, c| a.terms.as_slice() == [(var, c)];
        let count = match (test, lp.condition, lp.taken) {
            (Test::Zero(a), Cond::ne, true) | (Test::Zero(a), Cond::e, false)
                if coefficient(&a, *self.interned.get(&Expr::Constant(u32::MAX))?) =>
            {
                self.add(a.base, self.one)
            }
            (Test::Compare(a, b), Cond::l | Cond::b, true)
            | (Test::Compare(a, b), Cond::ge | Cond::ae, false)
                if coefficient(&a, self.one) && a.base == self.one =>
            {
                b.invariant()?
            }
            _ => return None,
        };
        // A do-while loop entered with zero cannot be treated as empty.
        // Only a signed compare limits the count to a signed dword.
        let max = if matches!(lp.condition, Cond::l | Cond::ge) {
            i32::MAX as u32
        } else {
            u32::MAX
        };
        self.check(Check::Positive(count, max));
        Some(count)
    }

    fn recurrence(&mut self, a: Value, b: Value, var: VarId) -> Option<(Form, ExprId)> {
        let (a, b) = a.zip(b)?;
        let delta = self.minus(b, a.clone()).invariant()?;
        let step = self.scale(self.term(var), delta);
        Some((self.plus(a, step), delta))
    }

    fn reloaded_word(&mut self, a: &Form, b: &Form) -> Value {
        self.cached_word(a, b).or_else(|| {
            let [(v, k)] = b.terms.as_slice() else {
                return None;
            };
            if b.base != self.zero || *k != self.one {
                return None;
            }
            let Var::Either([(l, _), (r, _)]) = self.vars[*v].clone() else {
                return None;
            };
            if &l == a {
                self.cached_word(a, &r)
            } else if &r == a {
                self.cached_word(a, &l)
            } else {
                None
            }
        })
    }

    fn inductive(&mut self, start: &(Form, ExprId), back: &Value) -> bool {
        let (form, delta) = start;
        let step = self.form(*delta);
        back.as_ref() == Some(&self.plus(form.clone(), step))
    }

    fn final_value(&mut self, value: Value, var: VarId, count: ExprId) -> Value {
        let mut value = value?;
        if value
            .terms
            .iter()
            .any(|&(v, _)| matches!(self.vars[v], Var::Either(_)))
        {
            return None;
        }
        if let Ok(i) = value.terms.binary_search_by_key(&var, |&(v, _)| v) {
            let (_, k) = value.terms.remove(i);
            let minus_one = self.neg(self.one);
            let last = self.add(count, minus_one);
            let x = self.mul(k, last);
            value.base = self.add(value.base, x);
        }
        Some(value)
    }

    fn loop_body(&mut self, lp: usize, mut initial: State, record: bool) -> Option<State> {
        let head = self.graph.loops[lp].head;
        let mut used = 0;
        let first = loop {
            let mut seed = initial.clone();
            seed.entry_reads = 0;
            let (first, _) = self.walk(head, seed, Some(lp), false)??;
            used |= first.entry_reads;
            let mut changed = false;
            for (r, value) in initial.regs.iter_mut().enumerate() {
                let Some(a) = value.as_ref() else {
                    continue;
                };
                let Some(Expr::Register(entry)) = a.invariant().map(|id| self.exprs[id]) else {
                    continue;
                };
                if used & (1 << entry) != 0 {
                    let known = self.checks.iter().find_map(|&c| match c {
                        Check::Equal(reg, word) if reg == a.base => Some(self.form(word)),
                        _ => None,
                    });
                    if let Some(word) =
                        known.or_else(|| self.reloaded_word(a, first.regs[r].as_ref()?))
                        && &word != a
                    {
                        *value = Some(word);
                        changed = true;
                    }
                }
            }
            if !changed {
                break first;
            }
        };
        let var = self.variable(Var::Counter(None));
        let mut recurring = [const { None }; 8];
        let mut slots = BTreeMap::new();
        let mut before = initial.clone();
        before.test = None;
        before.cdq = false;
        for (r, recurrence) in recurring.iter_mut().enumerate() {
            *recurrence = self.recurrence(initial.regs[r].clone(), first.regs[r].clone(), var);
            before.regs[r] = recurrence.as_ref().map(|(v, _)| v.clone());
        }
        for (&offset, b) in &first.slots {
            let a = initial
                .slots
                .get(&offset)
                .cloned()
                .unwrap_or_else(|| Some(self.frame_word(offset)));
            let r = self.recurrence(a, b.clone(), var);
            before
                .slots
                .insert(offset, r.as_ref().map(|(v, _)| v.clone()));
            slots.insert(offset, r);
        }
        // A single observed transfer is only a candidate recurrence. Recheck
        // it with symbolic iteration indices, discarding dead or unproved
        // candidates before using the resulting memory effects.
        let checks = self.checks.len();
        let effects = self.effects.len();
        let divisions = self.divisions.len();
        let frame_heads = self.frame_heads.len();
        let mut after = loop {
            let (back, after) = self.walk(head, before.clone(), Some(lp), record)??;
            let mut changed = false;
            for (r, recurrence) in recurring.iter_mut().enumerate() {
                if recurrence
                    .as_ref()
                    .is_some_and(|rec| !self.inductive(rec, &back.regs[r]))
                {
                    *recurrence = None;
                    before.regs[r] = None;
                    changed = true;
                }
            }
            for (&offset, recurrence) in &mut slots {
                let back = back
                    .slots
                    .get(&offset)
                    .cloned()
                    .unwrap_or_else(|| Some(self.frame_word(offset)));
                if recurrence
                    .as_ref()
                    .is_some_and(|rec| !self.inductive(rec, &back))
                {
                    *recurrence = None;
                    before.slots.insert(offset, None);
                    changed = true;
                }
            }
            if !changed {
                break after;
            }
            self.checks.truncate(checks);
            self.effects.truncate(effects);
            self.divisions.truncate(divisions);
            self.frame_heads.truncate(frame_heads);
        };
        let count = self.trip(lp, after.test.take()?, var)?;
        self.vars[var] = Var::Counter(Some(count));
        after.cdq = false;
        after.entry_reads |= used | initial.entry_reads;
        after.limits = initial.limits;
        for r in &mut after.regs {
            *r = self.final_value(r.take(), var, count);
        }
        for r in after.slots.values_mut() {
            *r = self.final_value(r.take(), var, count);
        }
        Some(after)
    }
}

impl Builder<'_> {
    fn frame_cache(&self) -> Option<FrameCache> {
        let n = self.trace.len();
        let mut reads = vec![None; n];
        let mut writes = Vec::new();
        for (at, effect) in &self.effects {
            let at = *at;
            let offset = self.frame_offset(&effect.addr);
            if effect.access == Access::FrameWrite {
                writes.push((at, offset?, effect.bytes));
            }
            if matches!(
                self.trace.insts[at].op,
                Op::Mov(Src::Reg(_), Src::Mem(_)) | Op::Imul(_, Src::Mem(_), _)
            ) {
                let offset = (effect.access == Access::FrameRead && effect.bytes == 4)
                    .then_some(offset)
                    .flatten();
                reads[at] = Some(match reads[at] {
                    None => offset,
                    Some(previous) if previous == offset => offset,
                    _ => None,
                });
            }
        }
        let mut best = None;
        let mut best_count = 1;
        for lp in &self.graph.loops {
            let head = lp.head;
            if self
                .graph
                .loops
                .iter()
                .any(|other| other.head != head && lp.inside[other.head])
            {
                continue;
            }
            // The initialization sits on fall-through before the loop's
            // body label. Every other arrival must already be in the loop.
            if head != 0 && (lp.inside[head - 1] || self.trace.insts[head - 1].next != Next::Fall)
                || self.graph.edges.iter().enumerate().any(|(from, edges)| {
                    !lp.inside[from]
                        && (edges[1] == Some(head) || edges[0] == Some(head) && from + 1 != head)
                })
            {
                continue;
            }
            let mut stack = self
                .frame_heads
                .iter()
                .filter_map(|&(at, offset)| (at == head).then_some(offset));
            let Some(Some(sp)) = stack.next() else {
                continue;
            };
            if stack.any(|offset| offset != Some(sp)) {
                continue;
            }
            let mut offsets = Vec::new();
            let mut slots = vec![None; n];
            let mut count = 0;
            for (at, offset) in reads.iter().enumerate() {
                let Some(Some(offset)) = *offset else {
                    continue;
                };
                // Entry admission excludes non-frame writes from these
                // reads. Here, exclude frame writes at exact byte widths.
                if !lp.inside[at]
                    || writes.iter().any(|&(from, write, bytes)| {
                        lp.inside[from]
                            && ((offset.wrapping_sub(write) as u32) < bytes
                                || (write.wrapping_sub(offset) as u32) < 4)
                    })
                {
                    continue;
                }
                let disp = offset.wrapping_sub(sp);
                let slot = if let Some(slot) = offsets.iter().position(|&d| d == disp) {
                    slot
                } else if offsets.len() < 8 {
                    offsets.push(disp);
                    offsets.len() - 1
                } else {
                    continue;
                };
                slots[at] = Some(slot as u8);
                count += 1;
            }
            if count > best_count {
                best_count = count;
                best = Some(FrameCache {
                    head,
                    offsets,
                    reads: slots,
                });
            }
        }
        best
    }

    fn expand(&mut self, mut effect: Effect, out: &mut Vec<Effect>) -> Option<()> {
        self.remaining = self.remaining.checked_sub(1)?;
        let choice = effect
            .addr
            .terms
            .iter()
            .position(|&(v, _)| matches!(self.vars[v], Var::Either(_)));
        let Some(i) = choice else {
            if !out.iter().any(|e| {
                e.addr == effect.addr
                    && e.access == effect.access
                    && e.bytes == effect.bytes
                    && e.limits == effect.limits
            }) {
                out.push(effect);
            }
            return Some(());
        };
        let (var, k) = effect.addr.terms.remove(i);
        let Var::Either(arms) = self.vars[var].clone() else {
            unreachable!()
        };
        for (arm, limits) in arms {
            let mut e = effect.clone();
            let scaled = self.scale(arm, k);
            e.addr = self.plus(e.addr, scaled);
            for l in limits {
                if !e.limits.contains(&l) {
                    e.limits.push(l);
                }
            }
            self.expand(e, out)?;
        }
        Some(())
    }
}

impl Proof {
    pub(crate) fn build(trace: &Trace) -> Option<Self> {
        let graph = Graph::build(trace)?;
        let mut b = Builder {
            trace,
            graph: &graph,
            exprs: Vec::new(),
            interned: FxHashMap::default(),
            vars: Vec::new(),
            effects: Vec::new(),
            checks: Vec::new(),
            divisions: Vec::new(),
            frame_heads: Vec::new(),
            at: 0,
            zero: 0,
            one: 0,
            remaining: PROOF_STEPS,
            states: Vec::new(),
        };
        b.zero = b.constant(0);
        b.one = b.constant(1);
        let regs = std::array::from_fn(|r| {
            let id = b.expr(Expr::Register(r as u8));
            Some(b.form(id))
        });
        b.walk(
            0,
            State {
                regs,
                slots: BTreeMap::new(),
                limits: Vec::new(),
                test: None,
                cdq: false,
                entry_reads: 0,
            },
            None,
            true,
        )?;
        let frame_cache = b.frame_cache();
        let mut effects = Vec::new();
        for (_, effect) in std::mem::take(&mut b.effects) {
            b.expand(effect, &mut effects)?;
        }
        if !effects.iter().any(|e| e.access == Access::Write) {
            return None;
        }
        let mut frame: [Vec<(i32, i64)>; 2] = Default::default();
        effects.retain(|effect| {
            let Some(offset) = b.frame_offset(&effect.addr) else {
                return true;
            };
            let kind = match effect.access {
                Access::FrameRead => 0,
                Access::FrameWrite => 1,
                _ => return true,
            };
            let end = i64::from(offset) + i64::from(effect.bytes);
            frame[kind].push((offset, end));
            false
        });
        for (mut spans, access) in frame
            .into_iter()
            .zip([Access::FrameRead, Access::FrameWrite])
        {
            spans.sort_unstable();
            let mut len = 0;
            for i in 0..spans.len() {
                let (start, end) = spans[i];
                if len > 0 && i64::from(start) <= spans[len - 1].1 {
                    spans[len - 1].1 = spans[len - 1].1.max(end);
                } else {
                    spans[len] = (start, end);
                    len += 1;
                }
            }
            for (start, end) in spans.into_iter().take(len) {
                effects.push(Effect {
                    addr: b.frame_address(start),
                    limits: Vec::new(),
                    access,
                    bytes: u32::try_from(end - i64::from(start)).ok()?,
                });
            }
        }
        // Branch choices have been expanded. Retain only the bounds that
        // survive in final span expressions, not the speculative transfers.
        let mut ids = FxHashMap::default();
        let mut vars = Vec::new();
        for effect in &mut effects {
            effect
                .limits
                .retain(|l| effect.addr.terms.iter().any(|&(var, _)| var == l.var));
            for (v, _) in &mut effect.addr.terms {
                *v = *ids.entry(*v).or_insert_with(|| {
                    let id = vars.len();
                    vars.push(b.vars[*v].clone());
                    id
                });
            }
            for l in &mut effect.limits {
                l.var = ids[&l.var];
            }
        }
        let mut checks = Vec::new();
        for check in b.checks {
            match check {
                Check::Positive(expr, max) => {
                    if let Some(old) = checks.iter_mut().find_map(|c| match c {
                        Check::Positive(at, max) if *at == expr => Some(max),
                        _ => None,
                    }) {
                        *old = (*old).min(max);
                        continue;
                    }
                }
                Check::StackAlignment(align) => {
                    if let Some(old) = checks.iter_mut().find_map(|c| match c {
                        Check::StackAlignment(align) => Some(align),
                        _ => None,
                    }) {
                        *old = (*old).max(align);
                        continue;
                    }
                }
                Check::Equal(..) => {}
            }
            checks.push(check);
        }
        // A visit without a fact rules out specialization. Where the two
        // forms meet, the normalized dividend also satisfies SignExtended.
        let mut divisions = vec![None; trace.len()];
        for (at, fact) in b.divisions {
            divisions[at] = Some(match (divisions[at], fact) {
                (None, fact) => fact,
                (Some(Some(a)), Some(b)) if a == b => Some(a),
                (Some(Some(_)), Some(_)) => Some(Division::SignExtended),
                _ => None,
            });
        }
        Some(Self {
            exprs: b.exprs,
            vars,
            checks,
            effects,
            divisions: divisions.into_iter().map(Option::flatten).collect(),
            frame_cache,
        })
    }

    pub(crate) fn frame_cache(&self) -> Option<&FrameCache> {
        self.frame_cache.as_ref()
    }

    pub(crate) fn division(&self, index: usize) -> Option<Division> {
        self.divisions[index]
    }

    /// Evaluate at the matching unit entry. The reader grants and reads one
    /// ordinary four-byte parameter cell after checking active write leases.
    /// Returned reads include all the parameter cells used by this admission.
    pub(crate) fn evaluate<'a>(
        &self,
        cpu: &CpuState,
        scratch: &'a mut Evaluation,
        mut read: impl FnMut(u32) -> Option<u32>,
    ) -> Option<&'a [Range]> {
        scratch.values.clear();
        scratch.values.resize(self.exprs.len(), None);
        scratch.words.clear();
        scratch.ranges.clear();
        scratch.parameters.clear();
        scratch.outputs.clear();
        scratch.frame.clear();
        for &check in &self.checks {
            match check {
                Check::Positive(expr, max) => {
                    if !(1..=max).contains(&self.value(expr, cpu, scratch, &mut read)?) {
                        return None;
                    }
                }
                Check::Equal(a, b) => {
                    if self.value(a, cpu, scratch, &mut read)?
                        != self.value(b, cpu, scratch, &mut read)?
                    {
                        return None;
                    }
                }
                Check::StackAlignment(n) => {
                    if cpu.regs[usize::from(ESP)] & (n - 1) != 0 {
                        return None;
                    }
                }
            }
        }
        for effect in &self.effects {
            let base = i64::from(self.value(effect.addr.base, cpu, scratch, &mut read)?);
            let (mut lo, mut hi) = (base, base);
            let mut empty = false;
            for &(var, coefficient) in &effect.addr.terms {
                let k = self.value(coefficient, cpu, scratch, &mut read)? as i32;
                let (a, b) = match self.vars[var] {
                    Var::Counter(Some(count)) => (
                        0,
                        i64::from(self.value(count, cpu, scratch, &mut read)?) - 1,
                    ),
                    Var::Remainder {
                        divisor,
                        normalized,
                    } => {
                        let n = i64::from(self.value(divisor, cpu, scratch, &mut read)?);
                        let trim = effect
                            .limits
                            .iter()
                            .filter(|l| l.var == var)
                            .map(|l| l.trim)
                            .max()
                            .unwrap_or(0);
                        (if normalized { 0 } else { 1 - n }, n - 1 - i64::from(trim))
                    }
                    _ => return None,
                };
                if b < a {
                    empty = true;
                    break;
                }
                let (a, b) = (a.checked_mul(i64::from(k))?, b.checked_mul(i64::from(k))?);
                lo = lo.checked_add(a.min(b))?;
                hi = hi.checked_add(a.max(b))?;
            }
            if empty {
                continue;
            }
            let start = u32::try_from(lo).ok()?;
            let end = u64::try_from(hi.checked_add(i64::from(effect.bytes))?).ok()?;
            if end > 1 << 32 {
                return None;
            }
            let range = Range {
                start,
                end,
                write: matches!(effect.access, Access::FrameWrite | Access::Write),
            };
            match effect.access {
                Access::FrameRead | Access::FrameWrite => {
                    scratch.frame.push(range);
                    scratch.ranges.push(range);
                }
                Access::Write => scratch.outputs.push(range),
                Access::Read => scratch.ranges.push(range),
            }
        }
        for &output in &scratch.outputs {
            if scratch
                .frame
                .iter()
                .chain(&scratch.parameters)
                .any(|&range| range.intersects(output))
            {
                return None;
            }
        }
        for &frame in scratch.frame.iter().filter(|r| r.write) {
            if scratch
                .parameters
                .iter()
                .any(|&parameter| parameter.intersects(frame))
            {
                return None;
            }
        }
        scratch.ranges.extend_from_slice(&scratch.outputs);
        // Sort and combine each access class separately; read-sharing must
        // not grow a write reservation beyond the bytes it can modify.
        scratch.ranges.sort_unstable_by_key(|r| (r.write, r.start));
        let mut len = 0;
        for i in 0..scratch.ranges.len() {
            let next = scratch.ranges[i];
            if len > 0
                && scratch.ranges[len - 1].write == next.write
                && u64::from(next.start) <= scratch.ranges[len - 1].end
            {
                scratch.ranges[len - 1].end = scratch.ranges[len - 1].end.max(next.end);
            } else {
                scratch.ranges[len] = next;
                len += 1;
            }
        }
        scratch.ranges.truncate(len);
        Some(&scratch.ranges)
    }

    fn value(
        &self,
        id: ExprId,
        cpu: &CpuState,
        scratch: &mut Evaluation,
        read: &mut impl FnMut(u32) -> Option<u32>,
    ) -> Option<u32> {
        if let Some(value) = scratch.values[id] {
            return Some(value);
        }
        let value = match self.exprs[id] {
            Expr::Constant(value) => value,
            Expr::Register(reg) => cpu.regs[usize::from(reg)],
            Expr::Word(addr) => {
                let start = self.value(addr, cpu, scratch, read)?;
                let end = u64::from(start) + 4;
                if end > 1 << 32 {
                    return None;
                }
                let range = Range {
                    start,
                    end,
                    write: false,
                };
                scratch.ranges.push(range);
                if self.stack_expression(addr) {
                    scratch.frame.push(range);
                } else {
                    scratch.parameters.push(range);
                }
                if let Some((_, value)) = scratch.words.iter().find(|&&(a, _)| a == start) {
                    *value
                } else {
                    if scratch.words.len() == PARAMETER_WORDS {
                        return None;
                    }
                    let value = read(start)?;
                    scratch.words.push((start, value));
                    value
                }
            }
            Expr::Add(a, b) => self
                .value(a, cpu, scratch, read)?
                .wrapping_add(self.value(b, cpu, scratch, read)?),
            Expr::Mul(a, b) => self
                .value(a, cpu, scratch, read)?
                .wrapping_mul(self.value(b, cpu, scratch, read)?),
            Expr::Neg(a) => self.value(a, cpu, scratch, read)?.wrapping_neg(),
        };
        scratch.values[id] = Some(value);
        Some(value)
    }

    fn stack_expression(&self, id: ExprId) -> bool {
        match self.exprs[id] {
            Expr::Register(ESP) => true,
            Expr::Add(a, b) => matches!(
                (self.exprs[a], self.exprs[b]),
                (Expr::Register(ESP), Expr::Constant(_)) | (Expr::Constant(_), Expr::Register(ESP))
            ),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu_core::Memory;
    use iced_x86::code_asm::*;

    const CODE: u32 = 0x10000;
    const FRAME: u32 = 0x20000;
    const PARAMS: u32 = 0x30000;
    const INPUT: u32 = 0x40000;
    const OUTPUT: u32 = 0x50000;

    fn trace(assemble: impl FnOnce(&mut CodeAssembler)) -> Trace {
        let mut a = CodeAssembler::new(32).unwrap();
        assemble(&mut a);
        let code = a.assemble(u64::from(CODE)).unwrap();
        let mut memory = Memory::new(CODE, 4096).unwrap();
        memory.protect(CODE, 4096, 3).unwrap();
        memory.write(CODE, &code).unwrap();
        memory.protect(CODE, 4096, 5).unwrap();
        let mut trace = Trace::default();
        trace.build(&memory, CODE, &[], &|_, _| None, &|_| None);
        trace
    }

    fn range(start: u32, bytes: u32, write: bool) -> Range {
        Range {
            start,
            end: u64::from(start) + u64::from(bytes),
            write,
        }
    }

    // spec: A64-TR-9
    #[test]
    fn bounds_nested_descending_rows_and_live_stack_count() {
        let t = trace(|a| {
            let mut row = a.create_label();
            let mut pixel = a.create_label();
            a.set_label(&mut row).unwrap();
            a.mov(ebx, edi).unwrap();
            a.xor(ecx, ecx).unwrap();
            a.set_label(&mut pixel).unwrap();
            a.movzx(eax, word_ptr(esi + ecx * 2)).unwrap();
            a.mov(word_ptr(ebx + ecx * 2), ax).unwrap();
            a.inc(ecx).unwrap();
            a.cmp(ecx, dword_ptr(ebp)).unwrap();
            a.jb(pixel).unwrap();
            a.add(edi, dword_ptr(ebp + 4)).unwrap();
            a.sub(dword_ptr(esp + 8), 1).unwrap();
            a.jne(row).unwrap();
            a.int3().unwrap();
        });
        assert_eq!(candidate(&t), Some(CODE));
        let proof = Proof::build(&t).expect("nested range proof");
        let mut cpu = CpuState::default();
        cpu.regs[4..8].copy_from_slice(&[FRAME, PARAMS, INPUT, OUTPUT]);
        let mut scratch = Evaluation::default();
        let read = |addr| match addr {
            PARAMS => Some(5),
            x if x == PARAMS + 4 => Some((-16i32) as u32),
            x if x == FRAME + 8 => Some(3),
            _ => None,
        };
        let ranges = proof.evaluate(&cpu, &mut scratch, read).unwrap();
        assert!(ranges.contains(&range(INPUT, 10, false)));
        assert!(ranges.contains(&range(OUTPUT - 32, 42, true)));
        assert!(ranges.contains(&range(FRAME + 8, 4, true)));
        cpu.regs[7] = 16;
        assert!(
            proof.evaluate(&cpu, &mut scratch, read).is_none(),
            "wrapped write"
        );
        cpu.regs[7] = PARAMS + 32;
        assert!(
            proof.evaluate(&cpu, &mut scratch, read).is_none(),
            "writes to live parameters"
        );
        cpu.regs[7] = FRAME + 40;
        assert!(
            proof.evaluate(&cpu, &mut scratch, read).is_none(),
            "output aliases a live frame slot"
        );
        cpu.regs[7] = u32::MAX - 4;
        assert!(
            proof.evaluate(&cpu, &mut scratch, read).is_none(),
            "access width crosses guest zero"
        );
        assert!(
            proof.evaluate(&cpu, &mut scratch, |_| None).is_none(),
            "denied parameter access"
        );
    }

    // spec: A64-TR-9
    #[test]
    fn bounds_both_arms_of_a_wrapping_gather() {
        let t = trace(|a| {
            let mut again = a.create_label();
            let mut wrap = a.create_label();
            let mut read = a.create_label();
            a.set_label(&mut again).unwrap();
            a.mov(ecx, dword_ptr(ebp)).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.cdq().unwrap();
            a.idiv(ecx).unwrap();
            a.mov(eax, edx).unwrap();
            a.add(eax, ecx).unwrap();
            a.cdq().unwrap();
            a.idiv(ecx).unwrap();
            a.mov(ebx, edx).unwrap();
            a.imul_2(ebx, dword_ptr(ebp + 4)).unwrap();
            a.lea(eax, dword_ptr(edx + 1)).unwrap();
            a.cmp(eax, ecx).unwrap();
            a.jae(wrap).unwrap();
            a.mov(edx, dword_ptr(ebp + 4)).unwrap();
            a.jmp(read).unwrap();
            a.set_label(&mut wrap).unwrap();
            a.mov(edx, ebx).unwrap();
            a.neg(edx).unwrap();
            a.set_label(&mut read).unwrap();
            a.add(ebx, edx).unwrap();
            a.movzx(eax, byte_ptr(esi + ebx)).unwrap();
            a.mov(byte_ptr(edi), al).unwrap();
            a.add(edi, 2).unwrap();
            a.sub(dword_ptr(esp), 1).unwrap();
            a.jne(again).unwrap();
            a.int3().unwrap();
        });
        let proof = Proof::build(&t).expect("bounded gather proof");
        let divisions: Vec<_> = (0..t.len()).filter_map(|i| proof.division(i)).collect();
        assert_eq!(divisions, [Division::SignExtended, Division::Normalized]);
        let mut cpu = CpuState::default();
        cpu.regs[4..8].copy_from_slice(&[FRAME, PARAMS, INPUT, OUTPUT]);
        let mut scratch = Evaluation::default();
        for (n, stride) in [(1, 23i32), (7, 3), (11, -5)] {
            let ranges = proof
                .evaluate(&cpu, &mut scratch, |addr| match addr {
                    FRAME => Some(9),
                    PARAMS => Some(n),
                    x if x == PARAMS + 4 => Some(stride as u32),
                    _ => None,
                })
                .unwrap();
            let read = |at| {
                ranges
                    .iter()
                    .any(|r| !r.write && r.start <= at && u64::from(at) < r.end)
            };
            for x in 0..n {
                assert!(read(INPUT.wrapping_add_signed(x as i32 * stride)));
            }
            assert!(!read(INPUT.wrapping_add_signed(n as i32 * stride)));
            assert!(ranges.contains(&range(OUTPUT, 17, true)));
        }
        for divisor in [0, (1 << 30) + 1] {
            assert!(
                proof
                    .evaluate(&cpu, &mut scratch, |addr| match addr {
                        FRAME => Some(9),
                        PARAMS => Some(divisor),
                        x if x == PARAMS + 4 => Some(3),
                        _ => None,
                    })
                    .is_none()
            );
        }
    }

    // spec: A64-TR-9
    #[test]
    fn division_facts_require_every_path_and_the_final_loop_recurrence() {
        for (conditional_cdq, changing_divisor, expected) in [
            (false, false, Some(Division::SignExtended)),
            (true, false, None),
            (false, true, None),
        ] {
            let t = trace(|a| {
                let mut again = a.create_label();
                let mut divide = a.create_label();
                // The first transfer preserves ECX. Once EBX advances, that
                // candidate recurrence fails and the division loses its fact.
                a.mov(ebx, ecx).unwrap();
                a.set_label(&mut again).unwrap();
                a.mov(eax, dword_ptr(esi)).unwrap();
                if conditional_cdq {
                    a.test(ebp, ebp).unwrap();
                    a.jz(divide).unwrap();
                }
                a.cdq().unwrap();
                a.set_label(&mut divide).unwrap();
                a.idiv(ecx).unwrap();
                a.mov(byte_ptr(edi), al).unwrap();
                if changing_divisor {
                    a.mov(ecx, ebx).unwrap();
                    a.inc(ebx).unwrap();
                }
                a.sub(dword_ptr(esp), 1).unwrap();
                a.jne(again).unwrap();
                a.int3().unwrap();
            });
            let proof = Proof::build(&t).expect("division independent of output bounds");
            let at = t
                .insts
                .iter()
                .position(|i| matches!(i.op, Op::Integer(Integer::Divide(..))))
                .unwrap();
            assert_eq!(proof.division(at), expected);
        }
    }

    // spec: A64-TR-9
    #[test]
    fn a_frame_pointer_is_a_recurrence_only_if_every_backedge_preserves_it() {
        for change in 0..3 {
            let t = trace(|a| {
                let mut again = a.create_label();
                let mut skip = a.create_label();
                a.set_label(&mut again).unwrap();
                a.mov(edx, dword_ptr(esp + 12)).unwrap();
                a.mov(al, byte_ptr(esp + 40)).unwrap();
                a.mov(byte_ptr(edx), al).unwrap();
                match change {
                    0 => a.add(dword_ptr(esp + 12), 3).unwrap(),
                    // This write is after the first pointer read, but poisons
                    // the next iteration's address just as an earlier write would.
                    1 => a.mov(byte_ptr(esp + 13), al).unwrap(),
                    _ => {
                        a.test(al, al).unwrap();
                        a.jz(skip).unwrap();
                        a.add(dword_ptr(esp + 12), 3).unwrap();
                        a.set_label(&mut skip).unwrap();
                    }
                }
                a.dec(esi).unwrap();
                a.jnz(again).unwrap();
                a.int3().unwrap();
            });
            let proof = Proof::build(&t);
            if change == 0 {
                let mut cpu = CpuState::default();
                cpu.regs[4] = FRAME;
                cpu.regs[6] = 4;
                let mut scratch = Evaluation::default();
                let ranges = proof
                    .as_ref()
                    .unwrap()
                    .evaluate(&cpu, &mut scratch, |at| {
                        (at == FRAME + 12).then_some(OUTPUT)
                    })
                    .unwrap();
                assert!(ranges.contains(&range(OUTPUT, 10, true)));
                let ranges = proof
                    .as_ref()
                    .unwrap()
                    .evaluate(&cpu, &mut scratch, |at| {
                        (at == FRAME + 12).then_some(FRAME + 24)
                    })
                    .unwrap();
                assert!(ranges.contains(&range(FRAME + 24, 10, true)));
            } else {
                assert!(proof.is_none());
            }
        }
    }

    // spec: A64-TR-9
    #[test]
    fn declines_unproved_addresses_and_synchronization() {
        for barrier in [false, true] {
            let t = trace(|a| {
                let mut again = a.create_label();
                a.set_label(&mut again).unwrap();
                if barrier {
                    a.mfence().unwrap();
                } else {
                    a.mov(edi, dword_ptr(edi)).unwrap();
                }
                a.mov(byte_ptr(edi), al).unwrap();
                a.inc(edi).unwrap();
                a.dec(ecx).unwrap();
                a.jne(again).unwrap();
                a.int3().unwrap();
            });
            assert!(Proof::build(&t).is_none());
        }
    }
}
