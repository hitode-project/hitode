//! Memory effects of detached translated computation. Expressions model only
//! address-bearing integer values. Native instruction lowering still owns all
//! results, flags, exceptional exits and floating-point state.

use std::cell::Cell;
use std::collections::{BTreeMap, VecDeque};

use adv_cpu_core::CpuState;
use adv_cpu_core::floating::fast::{Fast, Source};
use iced_x86::ConditionCode as Cond;
use rustc_hash::FxHashMap;

use super::decode::{Alu, Cc, EBP, ESP, Mem, Narrow8or16, Op, Part, ShiftKind, Src};
use super::integer::Integer;
use super::simd::{Kind, Loc};
use super::trace::{Edge, Mode, Next, Trace};

mod carried;

type ExprId = usize;
type VarId = usize;

// Admission performs bounded work on the owner, including parameter reads.
const PROOF_STEPS: usize = 128 * 1024;
const PARAMETER_WORDS: usize = 32;
const GATHERED_WORDS: usize = 4096;

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
    Remainder {
        divisor: ExprId,
        normalized: bool,
    },
    /// A value in 0..=max that no address depends on beyond its range: a
    /// zero extension, a right shift, a mask, or their sum or union.
    Bounded(u32),
    /// The dword at an address that varies within the segment: an entry of
    /// a table the computation reads, such as row or column pointers.
    /// Admission reads the entries it can reach and bounds it by them.
    Gathered(Form),
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
    indices: Vec<usize>,
    addr: Form,
    limits: Vec<Limit>,
    access: Access,
    bytes: u32,
    /// After the head loop exits: only its last iterations' block runs it.
    tail: bool,
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
    /// Dwords at constant offsets from the entry EBP, whose values the
    /// proof follows like frame dwords.
    based: Vec<Range>,
    gathered: Vec<Option<(i64, i64)>>,
    tables: Vec<Range>,
    entries: usize,
    spans: Vec<(i64, i64, Option<i64>, bool, bool)>,
}

impl Evaluation {
    /// The ranges the last evaluation admitted and the words it read since.
    pub(crate) fn admitted(&self) -> &[Range] {
        &self.ranges
    }
}

#[derive(Clone)]
pub(crate) struct Proof {
    exprs: Vec<Expr>,
    vars: Vec<Var>,
    checks: Vec<Check>,
    effects: Vec<Effect>,
    divisions: Vec<Option<Division>>,
    frame_cache: Option<FrameCache>,
    outer: Option<Outer>,
    /// Each trace instruction as decoded once from guest bytes, for the
    /// split proof and the optimizer's component effects.
    instructions: Vec<Option<iced_x86::Instruction>>,
}

/// The loop at the unit entry, whose iterations may run as blocks
/// (A64-TR-9).
#[derive(Clone, Copy)]
struct Outer {
    /// Its index, if a retained address depends on it.
    counter: Option<VarId>,
    count: ExprId,
    /// Start and step of each register that is affine in the index.
    regs: [Option<(ExprId, ExprId)>; 8],
    /// Offset from ESP, start and step of affine frame dwords.
    slots: [Option<(i32, ExprId, ExprId)>; 8],
    /// Whether the iterations exchange only those registers and dwords.
    split: bool,
    /// Whether they write frame bytes, so each block needs its own frame.
    frame: bool,
}

/// Iterations of an admitted head loop and its affine state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Plan {
    pub iterations: u32,
    pub regs: [Option<(u32, u32)>; 8],
    /// Offset from ESP, start and step of affine frame dwords.
    pub slots: [Option<(i32, u32, u32)>; 8],
    /// The frame bytes `[lo, hi)` from ESP that the loop and the tail touch,
    /// if the loop writes any: each block runs on a private copy.
    pub frame: Option<(i32, i32)>,
}

#[derive(Clone)]
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

/// A dword at a constant offset from the entry ESP or EBP, whose value
/// the proof follows through stores like a register's.
type Slot = (u8, i32);

#[derive(Clone)]
struct State {
    regs: [Value; 8],
    // A missing slot has not been touched; a present None is opaque.
    slots: BTreeMap<Slot, Value>,
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
    /// The loop one level in that shares this head.
    nested: Option<usize>,
}

struct Graph {
    edges: Vec<[Option<usize>; 2]>,
    order: Vec<usize>,
    loops: Vec<Loop>,
    heads: Vec<Option<usize>>,
}

impl Graph {
    /// Fails with the shape that stops it and the instruction it stops at.
    fn build(trace: &Trace) -> Result<Self, Refusal> {
        let refuse = |why, at| Err(Refusal { why, at });
        let n = trace.len();
        if n == 0 {
            return refuse("graph: empty", 0);
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
                let Some(arrivals) = incoming.get_mut(to) else {
                    return refuse("graph: edge", from);
                };
                arrivals.push(from);
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
            return refuse("graph: no loop", 0);
        }
        let mut loops = Vec::new();
        let mut heads = vec![None; n];
        for &(tail, head) in &back {
            let mut inside = vec![false; n];
            inside[head] = true;
            let mut pending = vec![tail];
            while let Some(at) = pending.pop() {
                if !std::mem::replace(&mut inside[at], true) {
                    pending.extend(&incoming[at]);
                }
            }
            // Only the head may be entered from outside the loop.
            if head != 0 && inside[0] {
                return refuse("graph: side entry", 0);
            }
            if let Some(i) =
                (0..n).find(|&i| inside[i] && i != head && incoming[i].iter().any(|&p| !inside[p]))
            {
                return refuse("graph: side entry", i);
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
                    return refuse("graph: no test", tail);
                };
                if *prev == head || !inside[*prev] {
                    return refuse("graph: no test", tail);
                }
                test = *prev;
            };
            // A conditional latch may branch to a short appended tail before
            // reentering its head. Other edges may leave only for the owner.
            if let Some(i) = (0..n).find(|&i| {
                inside[i] && i != test && edges[i].iter().flatten().any(|&to| !inside[to])
            }) {
                return refuse("graph: early exit", i);
            }
            loops.push(Loop {
                head,
                tail,
                test,
                condition,
                taken,
                exit,
                inside,
                nested: None,
            });
        }
        // Latches of one head nest when one body strictly holds the other,
        // as an outer loop that branches back to its inner loop's head; the
        // head names the outermost. Equal bodies are one loop with two
        // latches, as a `continue` makes.
        let size = |lp: &Loop| lp.inside.iter().filter(|&&i| i).count();
        let mut by_size: Vec<_> = (0..loops.len()).collect();
        by_size.sort_by_key(|&i| std::cmp::Reverse(size(&loops[i])));
        for &i in &by_size {
            let head = loops[i].head;
            match heads[head] {
                None => heads[head] = Some(i),
                Some(mut outer) => {
                    while let Some(next) = loops[outer].nested {
                        outer = next;
                    }
                    let (a, b) = (&loops[outer], &loops[i]);
                    if size(a) == size(b) || b.inside.iter().zip(&a.inside).any(|(&b, &a)| b && !a)
                    {
                        return refuse("graph: shared head", b.tail);
                    }
                    loops[outer].nested = Some(i);
                }
            }
        }
        for (i, a) in loops.iter().enumerate() {
            for b in &loops[..i] {
                if a.inside.iter().zip(&b.inside).any(|(&a, &b)| a && b)
                    && a.inside.iter().zip(&b.inside).any(|(&a, &b)| a && !b)
                    && a.inside.iter().zip(&b.inside).any(|(&a, &b)| b && !a)
                {
                    return refuse("graph: overlap", a.head);
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
        if order.len() != n {
            return refuse("graph: order", 0);
        }
        Ok(Self {
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

/// Why a trace has no proof: the stage that refused it and the trace
/// position it stopped at, for the JIT map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub why: &'static str,
    pub at: usize,
}

struct Builder<'a> {
    trace: &'a Trace,
    graph: &'a Graph,
    /// The first refusal: the walk gives up on its first failure.
    refused: &'a Cell<Option<Refusal>>,
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
    outer: Option<Outer>,
}

impl Builder<'_> {
    fn refuse<T>(&self, why: &'static str, at: usize) -> Option<T> {
        if self.refused.get().is_none() {
            self.refused.set(Some(Refusal { why, at }));
        }
        None
    }

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

    fn bounded(&mut self, max: u32) -> Form {
        let var = self.variable(Var::Bounded(max));
        self.term(var)
    }

    /// The largest value of a constant plus nonnegative multiples of
    /// bounded variables, if it fits a dword.
    fn bound(&self, form: &Form) -> Option<u32> {
        let Expr::Constant(mut max) = self.exprs[form.base] else {
            return None;
        };
        for &(var, k) in &form.terms {
            let (Var::Bounded(m), Expr::Constant(k)) = (&self.vars[var], self.exprs[k]) else {
                return None;
            };
            max = max.checked_add(m.checked_mul(i32::try_from(k).ok()? as u32)?)?;
        }
        Some(max)
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

    fn slot_of(&self, form: &Form) -> Option<Slot> {
        let id = form.invariant()?;
        let base = |id: ExprId| match self.exprs[id] {
            Expr::Register(r @ (ESP | EBP)) => Some(r),
            _ => None,
        };
        match self.exprs[id] {
            Expr::Add(a, b) => match (self.exprs[a], self.exprs[b]) {
                (_, Expr::Constant(c)) => Some((base(a)?, c as i32)),
                (Expr::Constant(c), _) => Some((base(b)?, c as i32)),
                _ => None,
            },
            _ => Some((base(id)?, 0)),
        }
    }

    fn slot_address(&mut self, (base, offset): Slot) -> Form {
        let sp = self.expr(Expr::Register(base));
        let offset = self.constant(offset as u32);
        let base = self.add(sp, offset);
        self.form(base)
    }

    fn frame_address(&mut self, offset: i32) -> Form {
        self.slot_address((ESP, offset))
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

    fn slot_word(&mut self, slot: Slot) -> Form {
        let addr = self.slot_address(slot);
        self.word(addr.base)
    }

    fn tail(&self) -> bool {
        self.graph.heads[0].is_none_or(|lp| !self.graph.loops[lp].inside[self.at])
    }

    fn read(&mut self, state: &mut State, addr: Form, bytes: u8, record: bool) -> Value {
        let frame = self.frame_offset(&addr);
        if record {
            self.effects.push((
                self.at,
                Effect {
                    indices: vec![self.at],
                    addr: addr.clone(),
                    limits: state.limits.clone(),
                    access: if frame.is_some() {
                        Access::FrameRead
                    } else {
                        Access::Read
                    },
                    bytes: u32::from(bytes),
                    tail: self.tail(),
                },
            ));
        }
        if bytes != 4 {
            return None;
        }
        if let Some(slot) = self.slot_of(&addr)
            && let Some(value) = state.slots.get(&slot)
        {
            return value.clone();
        }
        Some(match addr.invariant() {
            Some(addr) => self.word(addr),
            None => {
                let var = self.variable(Var::Gathered(addr));
                self.term(var)
            }
        })
    }

    fn write(&mut self, state: &mut State, addr: Form, bytes: u8, value: Value, record: bool) {
        let frame = self.frame_offset(&addr);
        if let Some((base, offset)) = self.slot_of(&addr) {
            // An opaque store poisons every overlapping dword, including a
            // read at an offset that has not appeared yet.
            for delta in -3..i32::from(bytes) {
                state.slots.insert((base, offset.wrapping_add(delta)), None);
            }
            if bytes == 4 {
                state.slots.insert((base, offset), value);
            }
        }
        if record {
            self.effects.push((
                self.at,
                Effect {
                    indices: vec![self.at],
                    addr,
                    limits: state.limits.clone(),
                    access: if frame.is_some() {
                        Access::FrameWrite
                    } else {
                        Access::Write
                    },
                    bytes: u32::from(bytes),
                    tail: self.tail(),
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
            // a & b <= min(a, b) and a | b <= a + b for unsigned values.
            Alu::And
                if self.bound(&a).or(self.bound(&b)).is_some()
                    && a.invariant().zip(b.invariant()).is_none() =>
            {
                let max = match (self.bound(&a), self.bound(&b)) {
                    (Some(x), Some(y)) => x.min(y),
                    (x, y) => x.or(y)?,
                };
                self.bounded(max)
            }
            Alu::Or if a.invariant().zip(b.invariant()).is_none() => {
                let max = self.bound(&a)?.checked_add(self.bound(&b)?)?;
                self.bounded(max)
            }
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
                } else if op == Alu::Test {
                    // A value tested against itself sets the flags of a
                    // compare with zero.
                    if dst == src {
                        let zero = self.form(self.zero);
                        state.test = a.map(|a| Test::Compare(a, zero));
                    }
                } else {
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
                let value = self.register(state, reg).map(|a| self.plus(a, inc));
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
                let value = self.register(state, reg).map(|a| self.scale(a, m));
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
                    (ShiftKind::Shr, a, Some(n)) if n != 0 => {
                        let max = a.and_then(|a| self.bound(&a)).unwrap_or(u32::MAX);
                        Some(self.bounded(max >> n))
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
            Op::Extend(dst, source, signed) => {
                let word = match source {
                    Narrow8or16::Mem(mem, word) => {
                        let addr = self.address(state, mem)?;
                        self.read(state, addr, if word { 2 } else { 1 }, record);
                        word
                    }
                    Narrow8or16::Byte(_) => false,
                    Narrow8or16::Word(_) => true,
                };
                let value = (!signed).then(|| self.bounded(if word { 0xffff } else { 0xff }));
                state.put(dst, value);
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
            (Expr::Word(_) | Expr::Constant(_), Expr::Register(_)) => {
                self.check(Check::Equal(bi, ai));
                Some(a.clone())
            }
            (Expr::Register(_), Expr::Constant(_)) => {
                self.check(Check::Equal(ai, bi));
                Some(b.clone())
            }
            _ => None,
        }
    }

    /// The frame word that ends each iteration equal to an entry register,
    /// as when a loop keeps a pointer in both: admission checks that the
    /// register starts equal to it too.
    fn mirrored_word(
        &mut self,
        a: &Form,
        back: &Form,
        first: &State,
        slots: &BTreeMap<Slot, Value>,
    ) -> Value {
        let slot = first
            .slots
            .iter()
            .find_map(|(&slot, value)| (value.as_ref() == Some(back)).then_some(slot))?;
        let word = match slots.get(&slot) {
            Some(value) => value.clone()?,
            None => self.slot_word(slot),
        };
        self.cached_word(a, &word)
    }

    /// The entry register that one arm holds where the other holds a
    /// constant: admission checks they are equal, as for a cached word.
    fn entry_or_constant(&self, a: &Value, b: &Value) -> Option<u8> {
        let (a, b) = (a.as_ref()?.invariant()?, b.as_ref()?.invariant()?);
        match (self.exprs[a], self.exprs[b]) {
            (Expr::Register(r), Expr::Constant(_)) | (Expr::Constant(_), Expr::Register(r)) => {
                Some(r)
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
        let mut used = state.entry_reads | incoming.entry_reads;
        for reg in 0..8 {
            if let Some(r) = self.entry_or_constant(&state.regs[reg], &incoming.regs[reg]) {
                used |= 1 << r;
                state.entry_reads |= 1 << r;
            }
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
                .unwrap_or_else(|| Some(self.slot_word(offset)));
            let value = self.join_value(a, &state.limits, b, &incoming.limits, 0);
            state.slots.insert(offset, value);
        }
        for (offset, a) in slots {
            let b = Some(self.slot_word(offset));
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
            // At a loop's own head, the walk enters the loop nested there.
            let child = match lp {
                Some(lp) if self.graph.loops[lp].head == at => self.graph.loops[lp].nested,
                _ => self.graph.heads[at],
            };
            if let Some(child) = child {
                let after = self.loop_body(child, state, record)?;
                if let Some(to) = self.graph.loops[child].exit {
                    self.join(&mut states[to], after);
                }
                continue;
            }
            if self.operation(at, &mut state, record).is_none() {
                let why = if self.remaining == 0 {
                    "steps"
                } else {
                    "instruction"
                };
                return self.refuse(why, at);
            }
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
            let [fall, mut taken] = self.graph.edges[at].map(|edge| edge.filter(|&to| inside(to)));
            if fall.is_some() && taken.is_some() && self.enters_loop(at, &state) {
                taken = None;
            }
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
        if let Some(at) = states.iter().position(Option::is_some) {
            return self.refuse("order", at);
        }
        self.states.push(states);
        match lp {
            Some(lp) => match (back, exit) {
                (Some(back), Some(exit)) => Some(Some((back, exit))),
                _ => self.refuse("loop", self.graph.loops[lp].head),
            },
            None => Some(None),
        }
    }

    /// A forward branch over a child loop, on a value that is the same in
    /// every iteration, as compilers guard a loop that may run zero times:
    /// the walk enters the loop, and admission checks the value does.
    fn enters_loop(&mut self, at: usize, state: &State) -> bool {
        let inst = &self.trace.insts[at];
        let Op::Jcc(Cc::Flags(cond), target) = inst.op else {
            return false;
        };
        let pc = |i: usize| self.trace.insts[i].pc;
        if !self
            .graph
            .loops
            .iter()
            .any(|l| inst.pc < pc(l.head) && pc(l.head) < target)
        {
            return false;
        }
        // A result's own zero and sign flags, or a compare with zero, which
        // also clears overflow: the loop runs when the branch is not taken,
        // so the value is nonzero, positive or not negative.
        let (value, compared) = match &state.test {
            Some(Test::Zero(a)) => (a, false),
            Some(Test::Compare(a, b)) if b.invariant() == Some(self.zero) => (a, true),
            _ => return false,
        };
        let Some(value) = value.invariant() else {
            return false;
        };
        match cond {
            Cond::e => self.check(Check::Positive(value, u32::MAX)),
            Cond::le if compared => self.check(Check::Positive(value, i32::MAX as u32)),
            Cond::s | Cond::l if compared || cond == Cond::s => {
                let next = self.add(value, self.one);
                self.check(Check::Positive(next, 1 << 31));
            }
            _ => return false,
        }
        true
    }

    fn trip(&mut self, lp: usize, test: Test, var: VarId) -> Option<ExprId> {
        let lp = &self.graph.loops[lp];
        let coefficient = |a: &Form, c| a.terms.as_slice() == [(var, c)];
        let signed = matches!(lp.condition, Cond::l | Cond::ge | Cond::le | Cond::g);
        let count = match (test, lp.condition, lp.taken) {
            (Test::Zero(a), Cond::ne, true) | (Test::Zero(a), Cond::e, false)
                if coefficient(&a, *self.interned.get(&Expr::Constant(u32::MAX))?) =>
            {
                self.add(a.base, self.one)
            }
            (Test::Compare(a, b), Cond::l | Cond::b | Cond::le, true)
            | (Test::Compare(a, b), Cond::ge | Cond::ae | Cond::g, false)
                if coefficient(&a, self.one) && (signed || a.base == self.one) =>
            {
                // The counter runs from its start to the bound, or one past
                // an inclusive bound that admission keeps below i32::MAX.
                let mut end = b.invariant()?;
                if matches!(lp.condition, Cond::le | Cond::g) {
                    end = self.add(end, self.one);
                    let bias = self.constant(1 << 31);
                    let biased = self.add(end, bias);
                    self.check(Check::Positive(biased, u32::MAX));
                }
                let start = self.neg(a.base);
                let past = self.add(end, start);
                self.add(past, self.one)
            }
            _ => return None,
        };
        // A do-while loop entered with zero cannot be treated as empty.
        // Only a signed compare limits the count to a signed dword, which
        // also keeps a count from a nonzero start exact.
        let max = if signed { i32::MAX as u32 } else { u32::MAX };
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
                    if let Some(word) = known
                        .or_else(|| self.reloaded_word(a, first.regs[r].as_ref()?))
                        .or_else(|| {
                            self.mirrored_word(a, first.regs[r].as_ref()?, &first, &initial.slots)
                        })
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
                .unwrap_or_else(|| Some(self.slot_word(offset)));
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
                    .unwrap_or_else(|| Some(self.slot_word(offset)));
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
        let test = self.graph.loops[lp].test;
        let Some(count) = after.test.take().and_then(|t| self.trip(lp, t, var)) else {
            return self.refuse("trip", test);
        };
        self.vars[var] = Var::Counter(Some(count));
        after.cdq = false;
        after.entry_reads |= used | initial.entry_reads;
        after.limits = initial.limits;
        if head == 0 {
            let mut affine = [None; 8];
            let starts = slots.iter().filter_map(|(&slot, recurrence)| {
                let (_, delta) = recurrence.as_ref()?;
                let (ESP, offset) = slot else {
                    return None;
                };
                let start = match initial.slots.get(&slot) {
                    Some(value) => value.as_ref()?.invariant()?,
                    None => self.slot_word(slot).invariant()?,
                };
                Some((offset, start, *delta))
            });
            for (slot, affine) in starts.zip(&mut affine) {
                *affine = Some(slot);
            }
            self.outer = Some(Outer {
                counter: Some(var),
                count,
                regs: std::array::from_fn(|r| {
                    let (_, delta) = recurring[r].as_ref()?;
                    Some((initial.regs[r].as_ref()?.invariant()?, *delta))
                }),
                slots: affine,
                split: false,
                frame: false,
            });
        }
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
            if lp.nested.is_some()
                || self
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
            if let Some(old) = out.iter_mut().find(|e| {
                e.addr == effect.addr
                    && e.access == effect.access
                    && e.bytes == effect.bytes
                    && e.limits == effect.limits
                    && e.tail == effect.tail
            }) {
                old.indices.extend(effect.indices);
                old.indices.sort_unstable();
                old.indices.dedup();
            } else {
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

/// The retained index of `v`, retaining first the variables a gathered
/// address depends on.
fn renumber(
    old: &[Var],
    v: VarId,
    ids: &mut FxHashMap<VarId, VarId>,
    vars: &mut Vec<Var>,
) -> VarId {
    if let Some(&id) = ids.get(&v) {
        return id;
    }
    let mut var = old[v].clone();
    if let Var::Gathered(addr) = &mut var {
        for (inner, _) in &mut addr.terms {
            *inner = renumber(old, *inner, ids, vars);
        }
    }
    let id = vars.len();
    ids.insert(v, id);
    vars.push(var);
    id
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Proof {
    pub(crate) fn build(trace: &Trace) -> Result<Self, Refusal> {
        let refused = Cell::new(None);
        Self::prove(trace, &refused).ok_or_else(|| {
            refused.get().unwrap_or(Refusal {
                why: "proof",
                at: 0,
            })
        })
    }

    fn prove(trace: &Trace, refused: &Cell<Option<Refusal>>) -> Option<Self> {
        let graph = match Graph::build(trace) {
            Ok(graph) => graph,
            Err(refusal) => {
                refused.set(Some(refusal));
                return None;
            }
        };
        let mut b = Builder {
            trace,
            graph: &graph,
            refused,
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
            outer: None,
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
        for (at, effect) in std::mem::take(&mut b.effects) {
            if b.expand(effect, &mut effects).is_none() {
                return b.refuse("expand", at);
            }
        }
        if !effects.iter().any(|e| e.access == Access::Write) {
            return b.refuse("unwritten", 0);
        }
        let mut frame: [Vec<(i32, i64)>; 4] = Default::default();
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
            frame[kind + 2 * usize::from(effect.tail)].push((offset, end));
            false
        });
        for (kind, mut spans) in frame.into_iter().enumerate() {
            let access = [Access::FrameRead, Access::FrameWrite][kind % 2];
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
                    indices: Vec::new(),
                    addr: b.frame_address(start),
                    limits: Vec::new(),
                    access,
                    bytes: u32::try_from(end - i64::from(start)).ok()?,
                    tail: kind >= 2,
                });
            }
        }
        // Branch choices have been expanded. Retain only the bounds that
        // survive in final span expressions, not the speculative transfers.
        let mut ids = FxHashMap::default();
        let mut vars = Vec::new();
        let outer = b.outer;
        for effect in &mut effects {
            effect
                .limits
                .retain(|l| effect.addr.terms.iter().any(|&(var, _)| var == l.var));
            for (v, _) in &mut effect.addr.terms {
                *v = renumber(&b.vars, *v, &mut ids, &mut vars);
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
            outer: outer.map(|mut outer| {
                outer.counter = outer.counter.and_then(|var| ids.get(&var).copied());
                outer
            }),
            instructions: Vec::new(),
        })
    }

    /// Decodes the trace's instructions, then proves that the head loop's
    /// iterations exchange only affine registers and frame dwords. `decode`
    /// returns the instruction at an address and length.
    pub(crate) fn prove_split(
        &mut self,
        trace: &Trace,
        decode: impl Fn(u32, u32) -> Option<iced_x86::Instruction>,
    ) {
        self.instructions = trace
            .insts
            .iter()
            .map(|inst| decode(inst.pc, inst.next_pc.wrapping_sub(inst.pc)))
            .collect();
        let Some(outer) = self.outer else {
            return;
        };
        let zero = |step: ExprId| self.exprs[step] == Expr::Constant(0);
        let mask = |f: &dyn Fn(ExprId) -> bool| {
            (0..8).fold(0, |set, r| {
                set | u8::from(outer.regs[r].is_some_and(|(_, step)| f(step))) << r
            })
        };
        let affine = (mask(&|_| true), mask(&zero));
        let slots: Vec<(i32, bool)> = outer
            .slots
            .iter()
            .flatten()
            .map(|&(offset, _, step)| (offset, zero(step)))
            .collect();
        let carried =
            carried::independent(trace, affine, &slots, &self.divisions, &self.instructions);
        self.outer = Some(Outer {
            split: carried.is_some(),
            frame: carried.flatten().is_some(),
            ..outer
        });
    }

    pub(crate) fn splits(&self) -> bool {
        self.outer.is_some_and(|outer| outer.split)
    }

    /// After a successful `evaluate`, the head loop's iterations if every
    /// one writes only bytes no other iteration reads or writes. A write
    /// needs a nonzero stride, and an effect it meets the same stride and a
    /// common span within one stride. Tail effects meet only the last block.
    pub(crate) fn plan(
        &self,
        cpu: &CpuState,
        scratch: &mut Evaluation,
        mut read: impl FnMut(u32) -> Option<u32>,
    ) -> Option<Plan> {
        let outer = self.outer.filter(|outer| outer.split)?;
        let n = self.value(outer.count, cpu, scratch, &mut read)?;
        if n < 2 {
            return None;
        }
        let mut spans = std::mem::take(&mut scratch.spans);
        spans.clear();
        // Each block's frame bytes are its own: only the span matters.
        let mut frame = (i64::MAX, i64::MIN);
        for effect in &self.effects {
            let Some((lo, hi)) =
                self.interval(&effect.addr, &effect.limits, cpu, scratch, &mut read)?
            else {
                continue;
            };
            if outer.frame && matches!(effect.access, Access::FrameRead | Access::FrameWrite) {
                frame = (frame.0.min(lo), frame.1.max(hi + i64::from(effect.bytes)));
                continue;
            }
            let stride = if effect.tail {
                Some(0)
            } else if effect
                .addr
                .terms
                .iter()
                .any(|&(v, _)| Some(v) != outer.counter && self.varies(v, outer.counter))
            {
                None
            } else {
                match effect
                    .addr
                    .terms
                    .iter()
                    .find(|&&(v, _)| Some(v) == outer.counter)
                {
                    Some(&(_, k)) => {
                        Some(i64::from(self.value(k, cpu, scratch, &mut read)? as i32))
                    }
                    None => Some(0),
                }
            };
            let write = matches!(effect.access, Access::Write | Access::FrameWrite);
            spans.push((lo, hi + i64::from(effect.bytes), stride, write, effect.tail));
        }
        let reach = i64::from(n - 1);
        // An iteration's span, at index 0.
        let first = |(lo, hi, stride, ..): (i64, i64, Option<i64>, bool, bool)| {
            let stride = stride.unwrap_or(0);
            let shift = reach * stride.abs();
            if stride >= 0 {
                (lo, hi - shift)
            } else {
                (lo + shift, hi)
            }
        };
        let independent = spans.iter().enumerate().all(|(i, &a)| {
            spans[i..].iter().enumerate().all(|(j, &b)| {
                let (_, _, sa, wa, ta) = a;
                let (_, _, sb, wb, tb) = b;
                if !(wa || wb) || (ta && tb) {
                    return true;
                }
                let disjoint = a.1 <= b.0 || b.1 <= a.0;
                if ta || tb {
                    return disjoint;
                }
                let Some(stride) = sa.filter(|&s| s != 0 && sb == Some(s)) else {
                    return j != 0 && disjoint;
                };
                let ((a0, a1), (b0, b1)) = (first(a), first(b));
                (disjoint && j != 0) || a1.max(b1) - a0.min(b0) <= stride.abs()
            })
        });
        let private = spans
            .iter()
            .all(|&(lo, hi, ..)| hi <= frame.0 || frame.1 <= lo);
        scratch.spans = spans;
        if !independent || !private {
            return None;
        }
        let mut regs = [None; 8];
        for (reg, affine) in regs.iter_mut().zip(outer.regs) {
            if let Some((start, step)) = affine {
                *reg = Some((
                    self.value(start, cpu, scratch, &mut read)?,
                    self.value(step, cpu, scratch, &mut read)?,
                ));
            }
        }
        let mut slots = [None; 8];
        for (slot, affine) in slots.iter_mut().zip(outer.slots) {
            if let Some((offset, start, step)) = affine {
                *slot = Some((
                    offset,
                    self.value(start, cpu, scratch, &mut read)?,
                    self.value(step, cpu, scratch, &mut read)?,
                ));
            }
        }
        let esp = i64::from(cpu.regs[usize::from(ESP)]);
        Some(Plan {
            iterations: n,
            regs,
            slots,
            frame: outer
                .frame
                .then(|| ((frame.0 - esp) as i32, (frame.1 - esp) as i32)),
        })
    }

    /// Whether a gathered dword's address depends on the head loop index.
    fn varies(&self, var: VarId, counter: Option<VarId>) -> bool {
        match &self.vars[var] {
            Var::Gathered(addr) => addr
                .terms
                .iter()
                .any(|&(v, _)| Some(v) == counter || self.varies(v, counter)),
            _ => false,
        }
    }

    /// The trace instruction at `index` as decoded for the split proof.
    pub(crate) fn instruction(&self, index: usize) -> Option<&iced_x86::Instruction> {
        self.instructions.get(index)?.as_ref()
    }

    /// Whether this instruction's non-frame read is invariant in the unit.
    /// Admission separates such a span from every write span.
    pub(crate) fn invariant_read(&self, index: usize) -> bool {
        let effects: Vec<_> = self
            .effects
            .iter()
            .filter(|e| e.access == Access::Read && e.indices.contains(&index))
            .collect();
        let written = |read: &Effect| {
            let Some(at) = self.followed(read) else {
                return false;
            };
            self.effects.iter().any(|e| {
                e.access == Access::Write
                    && self.followed(e).is_some_and(|w| {
                        w < at + i64::from(read.bytes) && at < w + i64::from(e.bytes)
                    })
            })
        };
        !effects.is_empty()
            && effects
                .iter()
                .all(|e| e.addr.terms.is_empty() && !written(e))
    }

    /// The offset from the entry EBP of an effect on a dword whose value
    /// the proof follows through the loop's own stores.
    fn followed(&self, effect: &Effect) -> Option<i64> {
        self.based_offset(effect.addr.invariant()?)
    }

    /// Disjoint bytes of two effects in the same iteration. Different
    /// nonzero strides are separated by admission; equal strides require
    /// a constant displacement from the same symbolic base.
    pub(crate) fn disjoint_effects(&self, store: usize, load: usize) -> bool {
        let writes: Vec<_> = self
            .effects
            .iter()
            .filter(|e| e.access == Access::Write && e.indices.contains(&store))
            .collect();
        let reads: Vec<_> = self
            .effects
            .iter()
            .filter(|e| e.access == Access::Read && e.indices.contains(&load))
            .collect();
        if writes.is_empty() || reads.is_empty() {
            return false;
        }
        fn base(
            exprs: &[Expr],
            id: ExprId,
            scale: u32,
            terms: &mut BTreeMap<ExprId, u32>,
            constant: &mut u32,
        ) {
            match exprs[id] {
                Expr::Constant(n) => *constant = constant.wrapping_add(n.wrapping_mul(scale)),
                Expr::Add(a, b) => {
                    base(exprs, a, scale, terms, constant);
                    base(exprs, b, scale, terms, constant);
                }
                Expr::Neg(a) => base(exprs, a, scale.wrapping_neg(), terms, constant),
                _ => {
                    let entry = terms.entry(id).or_default();
                    *entry = entry.wrapping_add(scale);
                }
            }
        }
        writes.iter().all(|a| {
            reads.iter().all(|b| {
                // Admission keeps other writes off followed dwords, but a
                // followed dword's own store may reach any other read.
                match (self.followed(a).is_some(), self.followed(b).is_some()) {
                    (false, true) => return true,
                    (true, false) => return false,
                    (false, false) if b.addr.terms.is_empty() => return true,
                    _ => {}
                }
                if let Some(counter) = self.outer.and_then(|o| o.counter) {
                    let stride = |e: &Effect| {
                        e.addr.terms.iter().find(|(v, _)| *v == counter).map_or(
                            Some(0),
                            |&(_, k)| match self.exprs[k] {
                                Expr::Constant(n) => Some(n),
                                _ => None,
                            },
                        )
                    };
                    if stride(a).zip(stride(b)).is_some_and(|(a, b)| a != b) {
                        return true;
                    }
                }
                if a.addr.terms != b.addr.terms {
                    return false;
                }
                let (mut at, mut bt) = (BTreeMap::new(), BTreeMap::new());
                let (mut ac, mut bc) = (0u32, 0u32);
                base(&self.exprs, a.addr.base, 1, &mut at, &mut ac);
                base(&self.exprs, b.addr.base, 1, &mut bt, &mut bc);
                at.retain(|_, k| *k != 0);
                bt.retain(|_, k| *k != 0);
                let delta = bc.wrapping_sub(ac);
                at == bt
                    && u64::from(delta) >= u64::from(a.bytes)
                    && u64::from(delta) + u64::from(b.bytes) <= 1 << 32
            })
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
        scratch.based.clear();
        scratch.gathered.clear();
        scratch.gathered.resize(self.vars.len(), None);
        scratch.tables.clear();
        scratch.entries = 0;
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
            let Some((lo, hi)) =
                self.interval(&effect.addr, &effect.limits, cpu, scratch, &mut read)?
            else {
                continue;
            };
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
                _ if effect
                    .addr
                    .invariant()
                    .is_some_and(|a| self.based_expression(a)) =>
                {
                    scratch.based.push(range);
                    scratch.ranges.push(range);
                }
                Access::Write => scratch.outputs.push(range),
                Access::Read => scratch.ranges.push(range),
            }
        }
        // Values followed through stores hold only if no other write
        // reaches them, and ESP and EBP dwords do not alias each other.
        let followed = || scratch.frame.iter().chain(&scratch.based);
        for &output in &scratch.outputs {
            if followed()
                .chain(&scratch.parameters)
                .chain(&scratch.tables)
                .any(|&range| range.intersects(output))
            {
                return None;
            }
        }
        for &table in &scratch.tables {
            if followed().any(|&f| f.write && f.intersects(table)) {
                return None;
            }
        }
        for &written in followed().filter(|r| r.write) {
            if scratch
                .parameters
                .iter()
                .any(|&parameter| parameter.intersects(written))
            {
                return None;
            }
        }
        for &based in &scratch.based {
            if scratch
                .frame
                .iter()
                .any(|&frame| (frame.write || based.write) && frame.intersects(based))
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

    /// The least and greatest guest address `form` takes over the segment,
    /// or `Some(None)` when a limit leaves it no value.
    fn interval(
        &self,
        form: &Form,
        limits: &[Limit],
        cpu: &CpuState,
        scratch: &mut Evaluation,
        read: &mut impl FnMut(u32) -> Option<u32>,
    ) -> Option<Option<(i64, i64)>> {
        let base = i64::from(self.value(form.base, cpu, scratch, read)?);
        let (mut lo, mut hi) = (base, base);
        for &(var, coefficient) in &form.terms {
            let k = self.value(coefficient, cpu, scratch, read)? as i32;
            let (a, b) = match self.vars[var] {
                Var::Counter(Some(count)) => {
                    (0, i64::from(self.value(count, cpu, scratch, read)?) - 1)
                }
                Var::Remainder {
                    divisor,
                    normalized,
                } => {
                    let n = i64::from(self.value(divisor, cpu, scratch, read)?);
                    let trim = limits
                        .iter()
                        .filter(|l| l.var == var)
                        .map(|l| l.trim)
                        .max()
                        .unwrap_or(0);
                    (if normalized { 0 } else { 1 - n }, n - 1 - i64::from(trim))
                }
                Var::Bounded(max) => (0, i64::from(max)),
                Var::Gathered(_) => self.gathered(var, cpu, scratch, read)?,
                _ => return None,
            };
            if b < a {
                return Some(None);
            }
            let (a, b) = (a.checked_mul(i64::from(k))?, b.checked_mul(i64::from(k))?);
            lo = lo.checked_add(a.min(b))?;
            hi = hi.checked_add(a.max(b))?;
        }
        // Guest addresses wrap: a span within one 2^32 period is the same
        // span shifted. Callers reject one that crosses a period boundary.
        let shift = lo.div_euclid(1 << 32) << 32;
        Some(Some((lo - shift, hi - shift)))
    }

    /// Reads every table entry a gathered dword can come from, at the
    /// common stride of its address terms, and returns their least and
    /// greatest value. The table joins the ranges no write may touch.
    fn gathered(
        &self,
        var: VarId,
        cpu: &CpuState,
        scratch: &mut Evaluation,
        read: &mut impl FnMut(u32) -> Option<u32>,
    ) -> Option<(i64, i64)> {
        if let Some(bounds) = scratch.gathered[var] {
            return Some(bounds);
        }
        let Var::Gathered(addr) = &self.vars[var] else {
            return None;
        };
        let (lo, hi) = self.interval(addr, &[], cpu, scratch, read)??;
        let mut stride = 0u64;
        for &(_, coefficient) in &addr.terms {
            let k = u64::from((self.value(coefficient, cpu, scratch, read)? as i32).unsigned_abs());
            stride = gcd(stride, k);
        }
        let stride = stride.max(1);
        let entries = u64::try_from(hi - lo).ok()? / stride + 1;
        scratch.entries += usize::try_from(entries).ok()?;
        if scratch.entries > GATHERED_WORDS {
            return None;
        }
        let end = u64::try_from(hi).ok()? + 4;
        if end > 1 << 32 {
            return None;
        }
        let (mut min, mut max) = (u32::MAX, 0);
        for n in 0..entries {
            let word = read(u32::try_from(lo).ok()? + (n * stride) as u32)?;
            (min, max) = (min.min(word), max.max(word));
        }
        scratch.tables.push(Range {
            start: lo as u32,
            end,
            write: false,
        });
        let bounds = (i64::from(min), i64::from(max));
        scratch.gathered[var] = Some(bounds);
        Some(bounds)
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
                } else if self.based_expression(addr) {
                    scratch.based.push(range);
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

    fn based_expression(&self, id: ExprId) -> bool {
        self.based_offset(id).is_some()
    }

    fn based_offset(&self, id: ExprId) -> Option<i64> {
        match self.exprs[id] {
            Expr::Register(EBP) => Some(0),
            Expr::Add(a, b) => match (self.exprs[a], self.exprs[b]) {
                (Expr::Register(EBP), Expr::Constant(c))
                | (Expr::Constant(c), Expr::Register(EBP)) => Some(i64::from(c as i32)),
                _ => None,
            },
            _ => None,
        }
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
        trace.build(&memory, CODE, &[], &|_, _| None, &|_| None, true);
        trace
    }

    /// A proof with its split decided from the same assembled bytes.
    fn proved(assemble: impl Fn(&mut CodeAssembler)) -> Option<Proof> {
        let mut a = CodeAssembler::new(32).unwrap();
        assemble(&mut a);
        let code = a.assemble(u64::from(CODE)).unwrap();
        let t = trace(&assemble);
        let mut proof = Proof::build(&t).ok()?;
        proof.prove_split(&t, |pc, len| {
            let at = usize::try_from(pc - CODE).ok()?;
            let bytes = code.get(at..at + len as usize)?;
            Some(iced_x86::Decoder::with_ip(32, bytes, u64::from(pc), 0).decode())
        });
        Some(proof)
    }

    /// `head: body; add esi,4; add edi,4; dec ecx; jne head; int3`
    fn row(body: impl Fn(&mut CodeAssembler)) -> impl Fn(&mut CodeAssembler) {
        move |a| {
            let mut head = a.create_label();
            a.set_label(&mut head).unwrap();
            body(a);
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(head).unwrap();
            a.int3().unwrap();
        }
    }

    fn plan(proof: &Proof, count: u32) -> Option<Plan> {
        let cpu = CpuState {
            regs: [0, count, 0, 0, FRAME, 0, INPUT, OUTPUT],
            ..CpuState::default()
        };
        let mut scratch = Evaluation::default();
        proof.evaluate(&cpu, &mut scratch, |_| None)?;
        proof.plan(&cpu, &mut scratch, |_| None)
    }

    // spec: A64-TR-9
    #[test]
    fn splits_iterations_that_touch_only_their_own_bytes() {
        let blend = |back: i32| {
            row(move |a| {
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.add(eax, dword_ptr(edi + back)).unwrap();
                a.shr(eax, 1).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
            })
        };
        let proof = proved(blend(0)).expect("in-place blend proof");
        assert!(proof.splits());
        let blocks = plan(&proof, 1000).expect("in-place blocks");
        assert_eq!(blocks.iterations, 1000);
        assert_eq!(blocks.regs[1], Some((1000, u32::MAX)));
        assert_eq!(blocks.regs[6], Some((INPUT, 4)));
        assert_eq!(blocks.regs[7], Some((OUTPUT, 4)));
        assert_eq!(blocks.regs[0], None, "EAX is written before it is read");
        // The previous element is another iteration's output.
        let proof = proved(blend(-4)).expect("previous-element proof");
        assert!(proof.splits());
        assert!(plan(&proof, 1000).is_none());
        // A two-dword output overlaps the next iteration's.
        let proof = proved(row(|a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.mov(dword_ptr(edi + 4), eax).unwrap();
        }))
        .unwrap();
        assert!(plan(&proof, 1000).is_none());
        // A store to one invariant address in every iteration.
        let proof = proved(row(|a| {
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.mov(dword_ptr(ebp), eax).unwrap();
        }))
        .unwrap();
        assert!(plan(&proof, 1000).is_none());
    }

    // spec: A64-TR-9
    #[test]
    fn a_guard_every_iteration_takes_alike_keeps_rows_splittable() {
        // Rows of EBX dwords; the inner count lives in a frame slot that
        // rows with no width skip. `guard` sets the flags the skip tests.
        let rows = |guard: fn(&mut CodeAssembler)| {
            move |a: &mut CodeAssembler| {
                let (mut head, mut inner, mut skip) =
                    (a.create_label(), a.create_label(), a.create_label());
                a.set_label(&mut head).unwrap();
                guard(a);
                a.jle(skip).unwrap();
                a.mov(dword_ptr(esp + 8), ebx).unwrap();
                a.xor(edx, edx).unwrap();
                a.set_label(&mut inner).unwrap();
                a.mov(eax, dword_ptr(esi + edx * 4)).unwrap();
                a.mov(dword_ptr(edi + edx * 4), eax).unwrap();
                a.inc(edx).unwrap();
                a.dec(dword_ptr(esp + 8)).unwrap();
                a.jne(inner).unwrap();
                a.set_label(&mut skip).unwrap();
                a.add(esi, 0x100).unwrap();
                a.add(edi, 0x100).unwrap();
                a.dec(ecx).unwrap();
                a.jne(head).unwrap();
                a.int3().unwrap();
            }
        };
        let proof = proved(rows(|a| a.test(ebx, ebx).unwrap())).expect("uniform guard proof");
        assert!(proof.splits());
        // A guard on each row's data lets the last rows skip the slot.
        let proof = proved(rows(|a| a.cmp(dword_ptr(esi), 0).unwrap())).expect("data guard proof");
        assert!(!proof.splits());
    }

    // spec: A64-TR-9
    #[test]
    fn iterations_that_exchange_state_do_not_split() {
        let exchanges: [&dyn Fn(&mut CodeAssembler); 4] = [
            // An integer accumulator.
            &|a| {
                a.add(ebx, dword_ptr(esi)).unwrap();
                a.mov(dword_ptr(edi), ebx).unwrap();
            },
            // A carry from the previous iteration's add.
            &|a| {
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.adc(eax, 0).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
            },
            // A running sum in a frame slot.
            &|a| {
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.add(dword_ptr(esp + 4), eax).unwrap();
                a.mov(dword_ptr(edi), eax).unwrap();
            },
            // x87 state.
            &|a| {
                a.fld(dword_ptr(esi)).unwrap();
                a.fstp(dword_ptr(edi)).unwrap();
            },
        ];
        for (i, body) in exchanges.into_iter().enumerate() {
            let proof = proved(row(body)).expect("an exchanging proof");
            assert!(!proof.splits(), "case {i}");
        }
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
    fn bounds_table_lookups_indexed_by_loaded_bytes() {
        const TABLE: u32 = 0x60000;
        let blend = |signed: bool| {
            trace(|a| {
                let mut pixel = a.create_label();
                a.set_label(&mut pixel).unwrap();
                if signed {
                    a.movsx(eax, byte_ptr(esi + 3)).unwrap();
                } else {
                    a.movzx(eax, byte_ptr(esi + 3)).unwrap();
                }
                a.movzx(ebx, byte_ptr(ebp)).unwrap();
                a.shl(eax, 8).unwrap();
                a.or(eax, ebx).unwrap();
                a.mov(al, byte_ptr(eax + edi)).unwrap();
                a.movzx(edx, al).unwrap();
                a.shl(edx, 8).unwrap();
                a.add(edx, edi).unwrap();
                a.mov(eax, dword_ptr(esi)).unwrap();
                a.shr(eax, 24).unwrap();
                a.movzx(eax, byte_ptr(eax + edx)).unwrap();
                a.and(ebx, 0x3f).unwrap();
                a.mov(byte_ptr(ecx), al).unwrap();
                a.mov(byte_ptr(ebx + ecx + 1), al).unwrap();
                a.add(esi, 4).unwrap();
                a.add(ecx, 2).unwrap();
                a.sub(dword_ptr(esp + 8), 1).unwrap();
                a.jne(pixel).unwrap();
                a.int3().unwrap();
            })
        };
        let proof = Proof::build(&blend(false)).expect("byte-indexed table proof");
        let mut cpu = CpuState::default();
        cpu.regs[1] = OUTPUT;
        cpu.regs[4..8].copy_from_slice(&[FRAME, PARAMS, INPUT, TABLE]);
        let mut scratch = Evaluation::default();
        let read = |addr| (addr == FRAME + 8).then_some(3);
        let ranges = proof.evaluate(&cpu, &mut scratch, read).unwrap();
        // Index (alpha << 8 | level) and (entry << 8) + top byte reach the
        // whole 64 KiB table and nothing past it; the masked offset adds
        // 0x3f to the last output byte.
        assert!(ranges.contains(&range(TABLE, 0x1_0000, false)));
        assert!(ranges.contains(&range(INPUT, 12, false)));
        assert!(ranges.contains(&range(OUTPUT, 4 + 1 + 0x3f + 1, true)));
        cpu.regs[7] = u32::MAX - 0x8000;
        assert!(
            proof.evaluate(&cpu, &mut scratch, read).is_none(),
            "table crosses guest zero"
        );
        assert!(
            Proof::build(&blend(true))
                .ok()
                .is_none_or(|proof| proof.evaluate(&cpu, &mut scratch, read).is_none()),
            "a sign-extended index is unbounded"
        );
    }

    // spec: A64-TR-9
    #[test]
    fn bounds_rows_of_a_table_blend_with_an_opaque_skip() {
        const GLOBAL: u32 = 0x31000;
        let t = trace(|a| {
            let (mut row, mut pixel, mut blend, mut next, mut kept, mut empty) = (
                a.create_label(),
                a.create_label(),
                a.create_label(),
                a.create_label(),
                a.create_label(),
                a.create_label(),
            );
            a.set_label(&mut row).unwrap();
            a.mov(ecx, ebp).unwrap();
            a.mov(esi, edx).unwrap();
            a.test(eax, eax).unwrap();
            a.jle(empty).unwrap();
            a.mov(dword_ptr(esp + 0x30), eax).unwrap();
            a.set_label(&mut pixel).unwrap();
            a.movzx(eax, byte_ptr(esi + 3)).unwrap();
            a.mov(ebx, dword_ptr(esp + 0x34)).unwrap();
            a.movzx(ebx, byte_ptr(ebx)).unwrap();
            a.shl(eax, 8).unwrap();
            a.or(eax, ebx).unwrap();
            a.mov(al, byte_ptr(eax + edi)).unwrap();
            a.cmp(al, 0xff).unwrap();
            a.jne(blend).unwrap();
            a.mov(ax, word_ptr(esi)).unwrap();
            a.mov(word_ptr(ecx), ax).unwrap();
            a.jmp(next).unwrap();
            a.set_label(&mut blend).unwrap();
            a.test(al, al).unwrap();
            a.jbe(kept).unwrap();
            a.movzx(edx, al).unwrap();
            a.shl(edx, 8).unwrap();
            a.add(edx, edi).unwrap();
            a.mov(edi, edx).unwrap();
            a.movzx(edx, byte_ptr(esi)).unwrap();
            a.movzx(edx, byte_ptr(edx + edi)).unwrap();
            a.mov(byte_ptr(ecx), dl).unwrap();
            a.mov(edx, dword_ptr(esp + 0x10)).unwrap();
            a.set_label(&mut next).unwrap();
            a.mov(edi, dword_ptr(GLOBAL)).unwrap();
            a.set_label(&mut kept).unwrap();
            a.mov(ebx, 1).unwrap();
            a.add(ecx, 3).unwrap();
            a.add(esi, 4).unwrap();
            a.sub(dword_ptr(esp + 0x30), ebx).unwrap();
            a.jne(pixel).unwrap();
            a.mov(eax, dword_ptr(esp + 0x2c)).unwrap();
            a.set_label(&mut empty).unwrap();
            a.add(edx, dword_ptr(esp + 0x1c)).unwrap();
            a.add(ebp, dword_ptr(esp + 0x20)).unwrap();
            a.sub(dword_ptr(esp + 0x14), ebx).unwrap();
            a.mov(dword_ptr(esp + 0x10), edx).unwrap();
            a.jne(row).unwrap();
            a.int3().unwrap();
        });
        assert_eq!(candidate(&t), Some(CODE));
        let proof = Proof::build(&t).expect("row blend proof");
        const TABLE: u32 = 0x60000;
        // eax pixels, edx source row, ebx the latch step, ebp target row.
        let mut cpu = CpuState {
            regs: [4, 0, INPUT, 1, FRAME, OUTPUT, 0, TABLE],
            ..CpuState::default()
        };
        let mut scratch = Evaluation::default();
        let read = |addr| match addr {
            GLOBAL => Some(TABLE),
            x if x == FRAME + 0x34 => Some(PARAMS),
            x if x == FRAME + 0x2c => Some(4),
            x if x == FRAME + 0x1c || x == FRAME + 0x20 => Some(0x100),
            x if x == FRAME + 0x14 => Some(3),
            // The source row, saved for the blend arm.
            x if x == FRAME + 0x10 => Some(INPUT),
            _ => None,
        };
        let ranges = proof.evaluate(&cpu, &mut scratch, read).unwrap();
        assert!(ranges.contains(&range(TABLE, 0x1_0000, false)));
        assert!(ranges.contains(&range(INPUT, 0x200 + 12 + 4, false)));
        assert!(ranges.contains(&range(OUTPUT, 0x200 + 9 + 2, true)));
        // The proof enters every row's pixel loop, which sets the latch step,
        // so its entry value does not matter; admission keeps rows nonempty.
        cpu.regs[3] = 2;
        assert!(proof.evaluate(&cpu, &mut scratch, read).is_some());
        cpu.regs[0] = 0;
        assert!(proof.evaluate(&cpu, &mut scratch, read).is_none());
    }

    // spec: A64-TR-9
    #[test]
    fn follows_a_counter_kept_beside_its_register_at_a_frame_pointer() {
        const TABLE: u32 = 0x60000;
        // Rows of pixels blended in place through a table. The pixel count
        // lives in ESI and at [ebp+0xc]; the blend arm borrows ESI and
        // reloads it, and the row count is decremented in memory.
        let t = trace(|a| {
            let (mut row, mut pixel, mut skip) =
                (a.create_label(), a.create_label(), a.create_label());
            a.set_label(&mut row).unwrap();
            a.mov(ecx, dword_ptr(ebp + 0x10)).unwrap();
            a.mov(esi, ecx).unwrap();
            a.mov(dword_ptr(ebp + 0xc), ecx).unwrap();
            a.set_label(&mut pixel).unwrap();
            a.mov(dl, byte_ptr(eax + 3)).unwrap();
            a.test(dl, dl).unwrap();
            a.je(skip).unwrap();
            a.movzx(esi, dl).unwrap();
            a.shl(esi, 8).unwrap();
            a.movzx(ecx, byte_ptr(ebx)).unwrap();
            a.movzx(ecx, byte_ptr(ecx + esi + TABLE as i32)).unwrap();
            a.mov(byte_ptr(ebx), cl).unwrap();
            a.mov(esi, dword_ptr(ebp + 0xc)).unwrap();
            a.set_label(&mut skip).unwrap();
            a.add(ebx, 4).unwrap();
            a.add(eax, 4).unwrap();
            a.dec(esi).unwrap();
            a.mov(dword_ptr(ebp + 0xc), esi).unwrap();
            a.jne(pixel).unwrap();
            a.add(ebx, dword_ptr(ebp + 0x18)).unwrap();
            a.add(eax, dword_ptr(ebp + 0x18)).unwrap();
            a.dec(dword_ptr(ebp + 0x14)).unwrap();
            a.jne(row).unwrap();
            a.int3().unwrap();
        });
        let proof = Proof::build(&t).expect("a counter followed through its frame dword");
        // The reload sees the loop's own store, so it is not invariant; the
        // width it starts from is.
        let load = |disp: u32| {
            t.insts
                .iter()
                .position(|i| matches!(i.op, Op::Mov(Src::Reg(_), Src::Mem(m)) if m.disp == disp))
                .unwrap()
        };
        assert!(!proof.invariant_read(load(0xc)));
        assert!(proof.invariant_read(load(0x10)));
        let mut cpu = CpuState {
            regs: [INPUT, 0, 0, OUTPUT, FRAME - 0x100, FRAME, 0, 0],
            ..CpuState::default()
        };
        let read = |addr| match addr - FRAME {
            0x10 => Some(4),
            0x14 => Some(3),
            0x18 => Some(0x100 - 16),
            _ => None,
        };
        let mut scratch = Evaluation::default();
        let ranges = proof.evaluate(&cpu, &mut scratch, read).unwrap();
        assert!(ranges.contains(&range(TABLE, 0x1_0000, false)));
        assert!(ranges.contains(&range(OUTPUT, 0x200 + 13, true)));
        assert!(ranges.contains(&range(FRAME + 0xc, 4, true)));
        assert!(ranges.contains(&range(FRAME + 0x14, 4, true)));
        // Pixels written over the followed dwords would change them.
        cpu.regs[3] = FRAME;
        assert!(proof.evaluate(&cpu, &mut scratch, read).is_none());
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
            let proof = Proof::build(&t).ok();
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
            let refusal = Proof::build(&t).err().expect("no proof");
            if barrier {
                // The refusal names the stage and the instruction it stopped at.
                let at = Refusal {
                    why: "instruction",
                    at: 0,
                };
                assert_eq!(refusal, at);
            }
        }
    }
    // spec: A64-TR-9
    #[test]
    fn enters_a_loop_its_guard_skips_only_when_it_runs_zero_times() {
        const TABLE: i32 = 0x60000;
        // Rows of pixels: a guarded inner loop that skips transparent
        // pixels, copies opaque ones and blends the rest through a table.
        let t = trace(|a| {
            let (mut row, mut pixel, mut skip, mut store, mut tail, mut blend) = (
                a.create_label(),
                a.create_label(),
                a.create_label(),
                a.create_label(),
                a.create_label(),
                a.create_label(),
            );
            a.set_label(&mut row).unwrap();
            a.test(ecx, ecx).unwrap();
            a.jle(tail).unwrap();
            a.mov(esi, ecx).unwrap();
            a.mov(dword_ptr(ebp + 0xc), ecx).unwrap();
            a.set_label(&mut pixel).unwrap();
            a.mov(dl, byte_ptr(eax + 3)).unwrap();
            a.test(dl, dl).unwrap();
            a.je(skip).unwrap();
            a.cmp(dl, 0xff).unwrap();
            a.jne(blend).unwrap();
            a.movzx(ecx, byte_ptr(eax)).unwrap();
            a.mov(byte_ptr(ebx), cl).unwrap();
            a.movzx(ecx, byte_ptr(eax + 2)).unwrap();
            a.jmp(store).unwrap();
            a.set_label(&mut blend).unwrap();
            a.movzx(esi, dl).unwrap();
            a.shl(esi, 8).unwrap();
            a.movzx(ecx, byte_ptr(ebx)).unwrap();
            a.movzx(ecx, byte_ptr(ecx + esi + TABLE)).unwrap();
            a.mov(byte_ptr(ebx), cl).unwrap();
            a.mov(esi, dword_ptr(ebp + 0xc)).unwrap();
            a.set_label(&mut store).unwrap();
            a.mov(byte_ptr(ebx + 2), cl).unwrap();
            a.set_label(&mut skip).unwrap();
            a.add(ebx, 4).unwrap();
            a.add(eax, 4).unwrap();
            a.dec(esi).unwrap();
            a.mov(dword_ptr(ebp + 0xc), esi).unwrap();
            a.jne(pixel).unwrap();
            a.mov(edx, dword_ptr(ebp - 4)).unwrap();
            a.set_label(&mut tail).unwrap();
            a.mov(ecx, dword_ptr(ebp + 0x18)).unwrap();
            a.sub(ecx, edx).unwrap();
            a.add(ebx, ecx).unwrap();
            a.mov(ecx, dword_ptr(ebp + 0x1c)).unwrap();
            a.sub(ecx, edx).unwrap();
            a.add(eax, ecx).unwrap();
            a.dec(dword_ptr(ebp + 0x14)).unwrap();
            a.mov(ecx, dword_ptr(ebp + 0x10)).unwrap();
            a.jne(row).unwrap();
            a.int3().unwrap();
        });
        let proof = Proof::build(&t).expect("a guarded row loop");
        let mut cpu = CpuState {
            regs: [INPUT, 4, 0, OUTPUT, FRAME - 0x100, FRAME, 0, 0],
            ..CpuState::default()
        };
        let read = |width: u32| {
            move |addr: u32| match addr.wrapping_sub(FRAME) {
                0x10 => Some(width),
                0x14 => Some(3),
                0x18 | 0x1c => Some(0x100),
                0xffff_fffc => Some(width * 4),
                _ => None,
            }
        };
        let mut scratch = Evaluation::default();
        let ranges = proof.evaluate(&cpu, &mut scratch, read(4)).unwrap();
        assert!(ranges.contains(&range(INPUT, 0x200 + 16, false)));
        assert!(ranges.contains(&range(OUTPUT, 0x200 + 15, true)));
        assert!(ranges.contains(&range(FRAME + 0x14, 4, true)));
        // A row the guard would skip is not one the proof walked.
        cpu.regs[1] = 0;
        assert!(proof.evaluate(&cpu, &mut scratch, read(0)).is_none());
    }

    // spec: A64-TR-9
    #[test]
    fn nests_an_outer_loop_that_branches_back_to_its_inner_head() {
        // Rows of pixels: the row loop resets the pixel count from a frame
        // slot and branches straight back to the pixel loop's head.
        let t = trace(|a| {
            let (mut pixel, mut skip) = (a.create_label(), a.create_label());
            a.set_label(&mut pixel).unwrap();
            a.mov(eax, dword_ptr(esi)).unwrap();
            a.test(eax, 0xff00_0000u32 as i32).unwrap();
            a.je(skip).unwrap();
            a.mov(dword_ptr(edi), eax).unwrap();
            a.set_label(&mut skip).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.dec(ecx).unwrap();
            a.jne(pixel).unwrap();
            a.add(edi, dword_ptr(ebp - 0x5c)).unwrap();
            a.add(esi, dword_ptr(ebp - 0x54)).unwrap();
            a.mov(ecx, dword_ptr(ebp - 0x4c)).unwrap();
            a.dec(edx).unwrap();
            a.jne(pixel).unwrap();
            a.int3().unwrap();
        });
        let proof = Proof::build(&t).expect("nested loops sharing a head");
        let cpu = CpuState {
            regs: [0, 4, 3, 0, FRAME - 0x100, FRAME, INPUT, OUTPUT],
            ..CpuState::default()
        };
        let read = |addr: u32| match addr.wrapping_sub(FRAME) as i32 {
            -0x4c => Some(4),
            -0x54 | -0x5c => Some(0xf0),
            _ => None,
        };
        let mut scratch = Evaluation::default();
        let ranges = proof.evaluate(&cpu, &mut scratch, read).unwrap();
        assert!(ranges.contains(&range(INPUT, 0x200 + 16, false)));
        assert!(ranges.contains(&range(OUTPUT, 0x200 + 16, true)));
    }
}
