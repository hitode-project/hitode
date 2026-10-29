//! x87 runs whose stack lives in host registers (A64F-FP-7).
//!
//! A run is a stretch of x87 instructions without a head after the first,
//! and the integer instructions between them that do not observe x87
//! state or v24-v31: no instruction helper, segment access, MMX, SSE
//! writing a vector register, or transfer other than a forward jcc, an
//! inlined call or an inlined return, whose miss publishes the run. A
//! merge inside it is reached only from the run, by arrivals that agree on
//! which slots hold values; there every value moves to the host register
//! of its slot, values only some arrivals hold are fetched, and stale ones
//! go to `CpuState`. Its prologue checks,
//! once, what each instruction would check: no pending exception, no MMX
//! state, and the tags of the slots the run reads before writing (not
//! empty) or pushes into (empty). A value it loads must also be one a run
//! can hold, which the tags need not tell. TOP at entry stays in w15 and
//! the control word in w8; the run tracks TOP, tags and the place of every
//! value at translation time, keeping values in v24-v31 as pairs of
//! doubles (see `pair`). Copies, exchanges and stores between slots only
//! rename. w13 holds the status word without TOP; IP and opcode are those
//! of the last instruction that records them. Before integer instructions
//! the status word with TOP goes to `CpuState` and the guest flags back to
//! NZCV; the next x87 instruction reloads both.
//! Across an access, x0-x3 hold no run state; guest flags in x4 pass through
//! NZCV for checked accesses, as outside runs.
//!
//! The run publishes at its end, at a jcc leaving it, and before
//! anything that observes `CpuState`: an instruction whose fast path does
//! not apply publishes the state before it, has its interpreter helper run
//! it and enters the rest of the run again through the same checks,
//! reloading every value and requiring the registers a rename shares to
//! still match, or runs every later x87 instruction through its helper
//! when they fail, leaving the unit at an integer one; a fault at a site
//! resumes in a stub that publishes the state before the instruction and
//! then leaves as the fault stub does. Integer instructions with a site
//! make the flags canonical first, so that stub keeps only NZCV. Instructions
//! that may change w13 before such an exit keep the status word before them
//! in w14.

use adv_cpu_core::floating::fast::{Fast, Source};

use crate::asm::Label;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::jit_map::Part;
use crate::runtime;

use super::super::decode::{self, Cc, Narrow8or16, Op, Src};
use super::super::simd::Kind;
use super::super::trace::{Edge, Mode, Next};
use super::super::{
    Cold as UnitCold, RUNTIME, State, Translator, X0, X1, X2, X3, X13, X14, X15, X16, X17,
};
use super::pair::Generic;
use super::{V0, X4, X5, X6, X8, X87};
use crate::runtime::Exit;

/// Host register `n` of the cache.
const fn v(n: u8) -> VReg {
    VReg::new(24 + n)
}

/// What a physical register holds, by its distance from TOP at entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum Slot {
    /// Its value at entry, still only in `CpuState`.
    Entry,
    /// Its value at entry, also in a host register.
    Clean(u8),
    /// A value written since, only in a host register.
    Dirty(u8),
    /// Popped or freed since, still holding the value last written to it
    /// in the run, if any: an empty register keeps its contents.
    Freed(Option<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) struct Model {
    slots: [Slot; 8],
    /// TOP minus TOP at entry, modulo 8.
    delta: u8,
    ip: Option<u32>,
    opcode: Option<u16>,
    /// The host registers whose values are narrow.
    pub(super) narrow: u8,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            slots: [Slot::Entry; 8],
            delta: 0,
            ip: None,
            opcode: None,
            narrow: 0,
        }
    }
}

impl Model {
    fn at(&self, i: u8) -> usize {
        usize::from((self.delta + i) & 7)
    }

    pub(super) fn get(&self, i: u8) -> Slot {
        self.slots[self.at(i)]
    }

    pub(super) fn set(&mut self, i: u8, slot: Slot) {
        let at = self.at(i);
        self.slots[at] = slot;
    }

    pub(super) fn reg(&self, i: u8) -> u8 {
        match self.get(i) {
            Slot::Clean(n) | Slot::Dirty(n) => n,
            slot => unreachable!("ST({i}) is {slot:?}"),
        }
    }

    /// The host registers some slot refers to.
    fn used(&self) -> u8 {
        self.slots.iter().fold(0, |used, slot| match slot {
            Slot::Clean(n) | Slot::Dirty(n) | Slot::Freed(Some(n)) => used | 1 << n,
            _ => used,
        })
    }

    pub(super) fn push(&mut self) {
        self.delta = (self.delta + 7) & 7;
    }

    fn free(&mut self, i: u8) {
        let freed = match self.get(i) {
            Slot::Dirty(n) => Slot::Freed(Some(n)),
            Slot::Freed(last) => Slot::Freed(last),
            Slot::Entry | Slot::Clean(_) => Slot::Freed(None),
        };
        self.set(i, freed);
    }

    fn pop(&mut self) {
        self.free(0);
        self.delta = (self.delta + 1) & 7;
    }
}

/// Tags a run needs at entry, by distance from TOP: a zero or a normal, or
/// empty.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in super::super) struct Need {
    value: u8,
    empty: u8,
}

/// What an instruction does to the stack, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    Read(u8),
    Write(u8),
    Push,
    Pop,
    Free(u8),
}

/// The stack effects of `fast` and whether it writes a new value, which
/// takes a free host register.
fn effects(fast: Fast) -> (Vec<Effect>, bool) {
    use Effect::*;
    let mut list = Vec::new();
    let mut fresh = false;
    match fast {
        Fast::Wait | Fast::LoadControl | Fast::StoreControl | Fast::StoreStatus => {}
        Fast::Load(Source::St(i)) => list.extend([Read(i), Push]),
        Fast::Load(_) | Fast::Constant(_) => {
            list.push(Push);
            fresh = true;
        }
        Fast::Store { pop, .. } => {
            list.push(Read(0));
            if pop {
                list.push(Pop);
            }
        }
        Fast::StoreSt { index, pop } => {
            list.extend([Read(0), Write(index)]);
            if pop {
                list.push(Pop);
            }
        }
        Fast::Arith {
            dest, source, pop, ..
        } => {
            list.push(Read(dest));
            if let Source::St(i) = source {
                list.push(Read(i));
            }
            list.push(Write(dest));
            if pop {
                list.push(Pop);
            }
            fresh = true;
        }
        Fast::Compare { source, pops } => {
            list.push(Read(0));
            if let Source::St(i) = source {
                list.push(Read(i));
            }
            if pops == 2 {
                list.push(Free(1));
            }
            list.extend(std::iter::repeat_n(Pop, usize::from(pops)));
        }
        Fast::Exchange(i) => list.extend([Read(0), Read(i), Write(0), Write(i)]),
        Fast::Negate | Fast::Absolute => {
            list.extend([Read(0), Write(0)]);
            fresh = true;
        }
        Fast::Round | Fast::Scale | Fast::Examine => unreachable!("not cached"),
    }
    (list, fresh)
}

/// What the planner knows of a physical register.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Known {
    Unknown,
    Value,
    Empty,
    /// Empty, but holding a value the run must publish.
    Stale,
}

/// Registers the run holds values of: a new value needs one more.
fn held(known: &[Known; 8]) -> usize {
    known
        .iter()
        .filter(|&&k| matches!(k, Known::Value | Known::Stale))
        .count()
}

/// What freeing a register the planner knows leaves: a value read at entry
/// is still in `CpuState`, one written may not be.
fn freed(known: Known) -> Known {
    match known {
        Known::Value | Known::Stale => Known::Stale,
        Known::Unknown | Known::Empty => Known::Empty,
    }
}

/// Simulates the stack from `known` (TOP `delta` from entry): `Some` with
/// the needs it adds when `fast` keeps to known slots and leaves a host
/// register for a new value, else `None`.
fn step(known: &mut [Known; 8], delta: &mut u8, need: &mut Need, fast: Fast) -> Option<()> {
    let (list, fresh) = effects(fast);
    let mut next = *known;
    let mut top = *delta;
    let mut added = *need;
    for effect in list {
        let at = |i: u8, top: u8| usize::from((top + i) & 7);
        match effect {
            Effect::Read(i) => {
                let at = at(i, top);
                match next[at] {
                    Known::Unknown => {
                        added.value |= 1 << at;
                        next[at] = Known::Value;
                    }
                    Known::Value => {}
                    Known::Empty | Known::Stale => return None,
                }
            }
            Effect::Write(i) => {
                if fresh && held(&next) > 7 {
                    return None;
                }
                next[at(i, top)] = Known::Value;
            }
            Effect::Push => {
                top = (top + 7) & 7;
                let at = at(0, top);
                match next[at] {
                    Known::Unknown => added.empty |= 1 << at,
                    Known::Empty | Known::Stale => {}
                    Known::Value => return None,
                }
                if fresh && held(&next) > 7 {
                    return None;
                }
                next[at] = Known::Value;
            }
            Effect::Pop => {
                let at = at(0, top);
                next[at] = freed(next[at]);
                top = (top + 1) & 7;
            }
            Effect::Free(i) => {
                let at = at(i, top);
                next[at] = freed(next[at]);
            }
        }
    }
    *known = next;
    *delta = top;
    *need = added;
    Some(())
}

/// What the planner knows of the slots in `model`.
fn known(model: Model) -> [Known; 8] {
    model.slots.map(|slot| match slot {
        Slot::Entry => Known::Unknown,
        Slot::Clean(_) | Slot::Dirty(_) => Known::Value,
        Slot::Freed(Some(_)) => Known::Stale,
        Slot::Freed(None) => Known::Empty,
    })
}

/// What a leave from inside a run does first.
#[derive(Clone, Copy)]
pub(in super::super) enum Leave {
    /// From integer instructions: publish the run in this state.
    Publish(Model),
    /// With the guest flags in x4: from a helper path, or from a fetch
    /// that failed at a merge, which publishes the run in this state.
    Restore(Option<Model>),
}

/// What an internal edge from integer instructions of a run does first.
#[derive(Clone, Copy)]
pub(in super::super) enum Arrive {
    /// To a merge of the run: from this state to the merge's.
    Reconcile(Model, Model),
    /// Out of the run: publish it in this state.
    Publish(Model),
}

/// The slot arrivals at a merge leave, if they agree on its kind: a value
/// some of them may still have to fetch, or empty.
fn meet(a: Known, b: Known) -> Option<Known> {
    use Known::*;
    match (a, b) {
        (Unknown, Unknown) => Some(Unknown),
        (Unknown | Value, Unknown | Value) => Some(Value),
        (Empty | Stale, Empty | Stale) => Some(Empty),
        _ => None,
    }
}

/// The state at a merge where the slots hold `known`: each value in the
/// host register of its slot.
fn canonical(known: [Known; 8], delta: u8) -> Model {
    let mut model = Model {
        delta,
        ..Model::default()
    };
    for (r, k) in known.iter().enumerate() {
        model.slots[r] = match k {
            Known::Value => Slot::Dirty(r as u8),
            Known::Unknown => Slot::Entry,
            Known::Empty | Known::Stale => Slot::Freed(None),
        };
    }
    model
}

/// The cold paths of one cached instruction.
pub(in super::super) struct Cold {
    index: usize,
    helper: u8,
    /// The state before the instruction, and whether w14 holds its status.
    before: Model,
    saved: bool,
    after: Model,
    /// Publishes `before`, then runs the helper.
    bail: Label,
    /// Runs the helper.
    help: Label,
    /// Where the run continues after the instruction, its next helper and
    /// what entering there needs; `None` after the last one.
    next: Option<(Label, Label, Need)>,
    done: Label,
    generic: Option<Generic>,
}

/// The run being emitted.
#[derive(Default)]
pub(in super::super) struct Cache {
    pub(in super::super) active: bool,
    /// w8 bit 16 caches the specialization decision for its control word.
    pub(super) guarded: bool,
    end: usize,
    pub(super) model: Model,
    /// The state before the instruction being emitted, its host registers
    /// and whether w14 holds its status word.
    before: Model,
    reserved: u8,
    saved: bool,
    /// Per instruction from the start: where it begins and its helper path.
    hot: Vec<Label>,
    help: Vec<Label>,
    start: usize,
    done: Option<Label>,
    /// Whether integer instructions of the run are being emitted.
    suspended: bool,
    /// The merges inside the run and their states.
    merges: Vec<(usize, Model)>,
    /// Whether the current instruction's entry is bound already.
    bound: bool,
}

impl Translator<'_> {
    /// The cacheable x87 instruction at `index`, if it is one.
    fn cacheable(&self, index: usize) -> Option<X87> {
        let inst = self.trace.insts[index];
        match inst.op {
            Op::X87(x) if !matches!(x.fast, Fast::Examine | Fast::Round | Fast::Scale) => Some(x),
            _ => None,
        }
    }

    /// Whether the integer instruction at `index` may sit inside a run:
    /// it does not observe x87 state or v24-v31, and leaves only through
    /// a jcc out of the trace or the miss of an inlined return.
    fn joins(&self, index: usize) -> bool {
        let inst = self.trace.insts[index];
        let register = |src: Src| !matches!(src, Src::Mem(mem) if mem.fs);
        match inst.op {
            Op::Push(src) => register(src),
            Op::Pop(_) | Op::Leave => true,
            Op::Call(_) => inst.mode == Mode::Plain,
            Op::Ret(_) => matches!(inst.mode, Mode::Inline(_)),
            // SSE that writes no vector register keeps the SIMD cache clean,
            // as the run's sites and exits assume.
            Op::Simd(simd) => {
                !simd.mmx()
                    && simd.footprint().1 == 0
                    && !((self.checked)(inst.pc)
                        && simd.memory()
                        && matches!(simd.kind, Kind::InsertWord | Kind::StoreControl))
            }
            Op::Integer(super::super::integer::Integer::Sahf)
            | Op::Nop
            | Op::Step(..)
            | Op::Neg(_)
            | Op::Not(_)
            | Op::Cdq
            | Op::Jmp(_)
            | Op::Setcc(..)
            | Op::Shift(_, Src::Reg(_), Some(_))
            | Op::Direction(_) => true,
            Op::Narrow(_, _, a, b) => [a, b]
                .iter()
                .all(|part| !matches!(part, decode::Part::Mem(mem) if mem.fs)),
            Op::Lea(_, mem) => !mem.fs,
            Op::Mov(a, b) | Op::Alu(_, a, b) => register(a) && register(b),
            Op::Cmov(_, _, src) | Op::Imul(_, src, _) | Op::Carry(_, _, src) => register(src),
            Op::Extend(_, source, _) => !matches!(source, Narrow8or16::Mem(mem, _) if mem.fs),
            Op::Jcc(Cc::Flags(_), _) => match inst.taken {
                Some(Edge::Exit(_)) => true,
                Some(Edge::Internal(to)) => to > index,
                _ => false,
            },
            _ => false,
        }
    }

    /// The internal edges that reach `index`.
    fn edges_into(&self, index: usize) -> usize {
        let internal = |edge| edge == Edge::Internal(index);
        self.trace
            .insts
            .iter()
            .filter(|inst| inst.taken.is_some_and(internal))
            .count()
            + self
                .trace
                .insts
                .iter()
                .filter(|inst| matches!(inst.next, Next::Edge(edge) if internal(edge)))
                .count()
    }

    /// The state of the run at its merge `index`, if it is one.
    fn merge_model(&self, index: usize) -> Option<Model> {
        self.x87.merges.iter().find(|m| m.0 == index).map(|m| m.1)
    }

    /// The last instruction of a run from `start` in state `from`, what
    /// it needs, and the states of its merges, which are `stored` once the
    /// run is open; `None` when not even the first fits.
    #[allow(clippy::type_complexity)]
    fn plan(
        &self,
        start: usize,
        from: Model,
        stored: Option<&[(usize, Model)]>,
    ) -> Option<(usize, Need, Vec<(usize, Model)>)> {
        let mut known = known(from);
        let mut need = Need::default();
        for (at, slot) in from.slots.iter().enumerate() {
            match slot {
                Slot::Clean(_) | Slot::Dirty(_) => need.value |= 1 << at,
                Slot::Freed(_) => need.empty |= 1 << at,
                Slot::Entry => {}
            }
        }
        let mut delta = from.delta;
        let mut end = None;
        let mut merges = Vec::new();
        // Forward edges from the run: target, slots and TOP.
        let mut pending: Vec<(usize, [Known; 8], u8)> = Vec::new();
        // Whether the instruction before jumps instead of falling through.
        let mut gap = false;
        'run: for index in start..self.trace.len() {
            if index != start && self.trace.head(index) {
                break;
            }
            if gap && !self.trace.merge[index] {
                break;
            }
            if index != start && self.trace.merge[index] {
                let met = match stored {
                    Some(stored) => match stored.iter().find(|m| m.0 == index) {
                        Some(&(_, model)) => model,
                        None => break,
                    },
                    None => {
                        let arrivals: Vec<_> = pending.iter().filter(|p| p.0 == index).collect();
                        if arrivals.is_empty() || arrivals.len() != self.edges_into(index) {
                            break;
                        }
                        if gap {
                            (_, known, delta) = *arrivals[0];
                        }
                        let mut met = known;
                        for &&(_, slots, arrival) in &arrivals {
                            if arrival != delta {
                                break 'run;
                            }
                            for r in 0..8 {
                                let Some(k) = meet(met[r], slots[r]) else {
                                    break 'run;
                                };
                                met[r] = k;
                            }
                        }
                        let model = canonical(met, delta);
                        merges.push((index, model));
                        model
                    }
                };
                // An arrival that has not read a value the merge holds
                // fetches it.
                for (r, slot) in met.slots.iter().enumerate() {
                    let unread = (!gap && known[r] == Known::Unknown)
                        || pending
                            .iter()
                            .any(|p| p.0 == index && p.1[r] == Known::Unknown);
                    if matches!(slot, Slot::Dirty(_)) && unread {
                        need.value |= 1 << r;
                    }
                }
                known = self::known(met);
                delta = met.delta;
            }
            gap = false;
            match self.cacheable(index) {
                Some(x) => {
                    if step(&mut known, &mut delta, &mut need, x.fast).is_none() {
                        break;
                    }
                    end = Some(index);
                }
                None if self.joins(index) => {
                    if let Some(Edge::Internal(to)) = self.trace.insts[index].taken {
                        pending.push((to, known, delta));
                    }
                    if let Next::Edge(Edge::Internal(to)) = self.trace.insts[index].next
                        && to > index
                    {
                        pending.push((to, known, delta));
                        gap = true;
                        continue;
                    }
                }
                None => break,
            }
            if self.trace.insts[index].next != Next::Fall {
                break;
            }
        }
        let end = end?;
        merges.retain(|m| m.0 <= end);
        Some((end, need, merges))
    }

    /// Whether the C1 that the instruction at `index` computes is dead:
    /// the next one sets C1 and can neither bail nor fault, its operands
    /// already in host registers once `model` has run `x`.
    fn c1_dead(&self, index: usize, x: X87, model: Model) -> bool {
        if index >= self.x87.end
            || self.trace.insts[index].next != Next::Fall
            || self.trace.merge[index + 1]
        {
            return false;
        }
        let Some(next) = self.cacheable(index + 1) else {
            return false;
        };
        let mut known = known(model);
        let (mut delta, mut need) = (model.delta, Need::default());
        if step(&mut known, &mut delta, &mut need, x.fast).is_none() {
            return false;
        }
        let cached = |i: u8| known[usize::from((delta + i) & 7)] == Known::Value;
        match next.fast {
            Fast::Exchange(i)
            | Fast::Compare {
                source: Source::St(i),
                ..
            } => cached(0) && cached(i),
            Fast::Load(Source::St(i)) => cached(i),
            Fast::StoreSt { .. } | Fast::Negate | Fast::Absolute => cached(0),
            Fast::Constant(_) => true,
            _ => false,
        }
    }

    /// Emits instruction `x` in a run, opening one if none is; false when
    /// it is not cacheable here.
    pub(in super::super) fn x87_cached(&mut self, x: X87) -> bool {
        if !self.x87.active && !self.x87_open() {
            return false;
        }
        if self.x87.suspended {
            self.x87_resume();
        }
        let index = self.index;
        let offset = index - self.x87.start;
        if !std::mem::take(&mut self.x87.bound) {
            self.asm.bind(self.x87.hot[offset]);
        }
        self.x87.before = self.x87.model;
        self.x87.reserved = self.x87.model.used();
        self.x87.saved = matches!(x.fast, Fast::Arith { .. } | Fast::Store { .. });
        if self.x87.saved {
            self.emit(mov(W, X14, X13));
        }
        if let Some(mem) = x.mem {
            let address = self.effective(mem);
            self.emit(mov(W, X6, address));
        }
        // Values read come into host registers before any other branch.
        let bail = self.asm.label();
        let (list, _) = effects(x.fast);
        for effect in list {
            match effect {
                Effect::Read(i) => self.x87_materialize(i, bail),
                Effect::Push => break,
                _ => {}
            }
        }
        let c1_dead = self.c1_dead(index, x, self.x87.model);
        let (dp, metadata, generic) = self.pair_body(x, bail, c1_dead);
        if dp {
            self.emit(str(W, X6, X5, runtime::X87_DP));
        }
        let mut model = self.x87.model;
        if metadata {
            model.ip = Some(self.trace.insts[index].pc);
            if x.opcode.is_some() {
                model.opcode = x.opcode;
            }
        }
        self.x87.model = model;
        let done = self.x87.done.expect("an open run");
        let next = (index < self.x87.end).then(|| {
            // A merge next reconciles from this state first.
            let (_, need, _) = self
                .plan(index + 1, model, Some(&self.x87.merges))
                .expect("a planned run");
            (self.x87.hot[offset + 1], self.x87.help[offset + 1], need)
        });
        self.push_cold(UnitCold::X87(Cold {
            index,
            helper: x.helper,
            before: self.x87.before,
            saved: self.x87.saved,
            after: model,
            bail,
            help: self.x87.help[offset],
            next,
            done,
            generic,
        }));
        if index == self.x87.end {
            self.x87_close();
        }
        true
    }

    /// Starts an integer instruction inside the run: the status word with
    /// TOP goes to `CpuState` and the guest flags to NZCV before the first
    /// of a stretch, whose helper path leaves the unit.
    pub(in super::super) fn x87_integer(&mut self) {
        let index = self.index;
        let offset = index - self.x87.start;
        if !std::mem::take(&mut self.x87.bound) {
            self.asm.bind(self.x87.hot[offset]);
        }
        if self.trace.insts[index].effects.site {
            // Its fault stub publishes the state as it is now.
            self.x87_suspend();
            self.canonicalize();
            self.x87.before = self.x87.model;
            self.x87.saved = false;
        }
        if index > self.x87.start && self.cacheable(index - 1).is_some() {
            self.push_cold(UnitCold::Leave {
                label: self.x87.help[offset],
                state: State::CANONICAL,
                simd_dirty: 0,
                refund: self.trace.owed(index),
                exit: Exit::Dispatch,
                eip: self.trace.insts[index].pc,
                link: false,
                x87: Some(Leave::Restore(None)),
            });
        }
        self.x87_suspend();
    }

    fn x87_suspend(&mut self) {
        if self.x87.suspended {
            return;
        }
        self.x87_status();
        self.emit_all(&[strh(X13, X5, runtime::X87_STATUS), msr_nzcv(X4)]);
        self.x87.suspended = true;
    }

    /// The fall-through arrival at merge `index` of the run, before the
    /// merge's own code, which runs suspended in the merge's state.
    pub(in super::super) fn x87_merge(&mut self, index: usize) {
        let to = self.merge_model(index).expect("a planned merge");
        if self.trace.insts[index - 1].next != Next::Fall {
            // Only jumps arrive, each reconciled already.
            self.x87.model = to;
            return;
        }
        if !self.x87.suspended {
            // A helper path of the instruction before enters here.
            self.asm.bind(self.x87.hot[index - self.x87.start]);
            self.x87.bound = true;
            self.x87_suspend();
        }
        let eip = self.trace.insts[index].pc;
        self.x87_reconcile(self.x87.model, to, self.trace.owed(index), eip);
        self.remark(Part::Body);
        self.x87.model = to;
    }

    /// What an internal edge to `to` from integer instructions of the
    /// run does first, in cold code for a jcc or before the jump.
    pub(in super::super) fn x87_arriving(&self, to: usize) -> Option<Arrive> {
        if !self.x87.active {
            return None;
        }
        let from = self.x87.model;
        Some(match self.merge_model(to) {
            Some(model) => Arrive::Reconcile(from, model),
            None => Arrive::Publish(from),
        })
    }

    /// The x87 part of an internal edge from `from` to `to`, in cold code.
    pub(in super::super) fn x87_arrive(&mut self, arrive: Arrive, from: usize, to: usize) {
        match arrive {
            Arrive::Publish(model) => self.x87_leave(Leave::Publish(model)),
            Arrive::Reconcile(model, merge) => {
                let eip = self.trace.insts[to].pc;
                self.x87_reconcile(model, merge, self.trace.unused(from), eip);
                self.remark(Part::Cold);
            }
        }
    }

    /// Brings a suspended run from state `from` to the merge state `to`;
    /// a fetch that fails publishes and leaves for `eip` with `refund`.
    fn x87_reconcile(&mut self, from: Model, to: Model, refund: u32, eip: u32) {
        self.remark(Part::X87Publish);
        self.canonicalize();
        self.emit_all(&[mrs_nzcv(X4), ldr(X, X5, RUNTIME, runtime::CPU_STATE)]);
        self.x87_reload(from.delta);
        // Stale values the merge leaves empty go to CpuState first.
        for (r, &slot) in from.slots.iter().enumerate() {
            if let (Slot::Freed(Some(n)), Slot::Freed(None)) = (slot, to.slots[r]) {
                self.x87_store_slot(r, n);
            }
        }
        let mut moves = Vec::new();
        let mut fetches = Vec::new();
        let mut published = to;
        for (r, &slot) in to.slots.iter().enumerate() {
            if let Slot::Dirty(c) = slot {
                match from.slots[r] {
                    Slot::Clean(n) | Slot::Dirty(n) => moves.push((v(c), v(n))),
                    _ => {
                        fetches.push((r, c));
                        published.slots[r] = Slot::Entry;
                    }
                }
            }
        }
        self.x87_moves(moves);
        if let Some(ip) = from.ip {
            self.mov_imm(X16, ip);
            self.emit(str(W, X16, X5, runtime::X87_IP));
        }
        if let Some(opcode) = from.opcode {
            self.emit_all(&[movz(W, X16, opcode, 0), strh(X16, X5, runtime::X87_OPCODE)]);
        }
        if !fetches.is_empty() {
            let fail = self.asm.label();
            self.push_cold(UnitCold::Leave {
                label: fail,
                state: State::CANONICAL,
                simd_dirty: 0,
                refund,
                exit: Exit::Dispatch,
                eip,
                link: false,
                x87: Some(Leave::Restore(Some(published))),
            });
            for (r, c) in fetches {
                self.x87_fetch(r, c, false, fail);
            }
        }
        self.emit(msr_nzcv(X4));
    }

    /// Moves pairs between host registers, all at once.
    fn x87_moves(&mut self, mut moves: Vec<(VReg, VReg)>) {
        moves.retain(|(to, from)| to != from);
        while !moves.is_empty() {
            let free = (0..moves.len()).find(|&i| moves.iter().all(|m| m.1 != moves[i].0));
            match free {
                Some(i) => {
                    let (to, from) = moves.remove(i);
                    self.emit(vmov(true, to, from));
                }
                None => {
                    // A cycle: the first destination's value moves aside.
                    let to = moves[0].0;
                    self.emit(vmov(true, V0, to));
                    for m in &mut moves {
                        if m.1 == to {
                            m.1 = V0;
                        }
                    }
                }
            }
        }
    }

    /// The x87 registers again after integer instructions of the run.
    fn x87_resume(&mut self) {
        self.canonicalize();
        self.cpu();
        self.emit(mrs_nzcv(X4));
        self.x87_reload(self.x87.model.delta);
        self.x87.suspended = false;
    }

    /// w13 from `CpuState`, w15 TOP at entry for a TOP `delta` past it,
    /// and w8.
    fn x87_reload(&mut self, delta: u8) {
        self.x87.guarded = false;
        self.emit_all(&[
            ldrh(X13, X5, runtime::X87_STATUS),
            ldrh(X8, X5, runtime::X87_CONTROL),
            ubfx(W, X15, X13, 11, 3),
        ]);
        // A reload past the run's end (a helper after its last instruction)
        // has no arithmetic left to guard.
        if self
            .trace
            .insts
            .get(self.index..=self.x87.end)
            .unwrap_or_default()
            .iter()
            .any(|inst| {
                matches!(
                    inst.op,
                    Op::X87(X87 {
                        fast: Fast::Arith { .. },
                        ..
                    })
                )
            })
        {
            self.pair_control();
        }
        if delta != 0 {
            self.emit_all(&[
                sub_imm(W, X15, X15, u32::from(delta), false),
                and_imm(W, X15, X15, 7),
            ]);
        }
    }

    /// What a leave from inside a run does before the leave itself.
    pub(in super::super) fn x87_leave(&mut self, leave: Leave) {
        match leave {
            Leave::Restore(model) => {
                self.emit(msr_nzcv(X4));
                if let Some(model) = model {
                    self.x87_leave(Leave::Publish(model));
                }
            }
            Leave::Publish(model) => {
                self.canonicalize();
                self.emit_all(&[mrs_nzcv(X4), ldr(X, X5, RUNTIME, runtime::CPU_STATE)]);
                self.x87_reload(model.delta);
                self.remark(Part::X87Publish);
                self.x87_publish(model, X13);
                self.emit(msr_nzcv(X4));
                self.remark(Part::Cold);
            }
        }
    }

    /// What a jcc leaving the trace here does first.
    pub(in super::super) fn x87_leaving(&self) -> Option<Leave> {
        (self.x87.active && self.x87.suspended).then_some(Leave::Publish(self.x87.model))
    }

    /// The prologue of a run from the current instruction.
    fn x87_open(&mut self) -> bool {
        if self.cacheable(self.index).is_none() {
            return false;
        }
        let start = self.index;
        let Some((end, need, merges)) = self.plan(start, Model::default(), None) else {
            return false;
        };
        self.remark(Part::X87Entry);
        self.flush_simd();
        self.canonicalize();
        self.cpu();
        let count = end - start + 1;
        self.x87.hot = (0..count).map(|_| self.asm.label()).collect();
        self.x87.help = (0..count).map(|_| self.asm.label()).collect();
        self.x87.done = Some(self.asm.label());
        self.x87.start = start;
        self.x87.end = end;
        self.x87.model = Model::default();
        self.x87.active = true;
        self.x87.suspended = false;
        self.x87.merges = merges;
        self.x87.bound = false;
        self.emit(mrs_nzcv(X4));
        let help = self.x87.help[0];
        self.x87_checks(Model::default(), need, help);
        self.remark(Part::Body);
        true
    }

    /// Loads w13, w15 and w8 for entering a run in state `model`,
    /// branching to `fail` unless the run may go on.
    fn x87_checks(&mut self, model: Model, need: Need, fail: Label) {
        self.x87_reload(model.delta);
        self.emit_all(&[
            ldr(W, X16, X5, runtime::MMX_MODE),
            and_imm(W, X17, X13, 0x80),
            orr(W, X16, X16, X17, Lsl, 0),
        ]);
        self.asm.cbz(W, true, X16, fail);
        if need == Need::default() {
            return;
        }
        // w16 bits 2r..2r+1 the tag of the register r past TOP at entry.
        self.emit_all(&[
            ldrh(X16, X5, runtime::X87_TAG),
            orr(W, X16, X16, X16, Lsl, 16),
            lsl_imm(W, X17, X15, 1),
            shiftv(W, Shift::Lsr, X16, X16, X17),
        ]);
        let spread = |mask: u8, bits: u32| {
            (0..8)
                .filter(|r| mask & (1 << r) != 0)
                .fold(0u32, |all, r| all | bits << (2 * r))
        };
        if need.value != 0 {
            self.mov_imm(X17, spread(need.value, 2));
            self.emit(and(W, X17, X17, X16, Lsl, 0));
            self.asm.cbz(W, true, X17, fail);
        }
        if need.empty != 0 {
            self.mov_imm(X17, spread(need.empty, 3));
            self.emit(bic(W, X17, X17, X16, Lsl, 0));
            self.asm.cbz(W, true, X17, fail);
        }
    }

    /// The host address of the register `r` past TOP at entry, in x17;
    /// uses w16.
    fn x87_slot(&mut self, r: usize) {
        self.emit(add_imm(W, X16, X15, r as u32, false));
        self.emit_all(&[
            and_imm(W, X16, X16, 7),
            add_imm(X, X17, X5, runtime::X87_REGS, false),
            add(X, X17, X17, X16, Lsl, 3),
            add(X, X17, X17, X16, Lsl, 1),
        ]);
    }

    /// A host register no slot refers to, now or before the instruction.
    pub(super) fn x87_alloc(&self) -> u8 {
        let busy = self.x87.model.used() | self.x87.reserved;
        (0..8)
            .find(|n| busy & (1 << n) == 0)
            .expect("the plan leaves a register free")
    }

    /// Brings ST(i) into a host register.
    fn x87_materialize(&mut self, i: u8, fail: Label) {
        if self.x87.model.get(i) != Slot::Entry {
            return;
        }
        let n = self.x87_alloc();
        let r = self.x87.model.at(i);
        self.x87_fetch(r, n, false, fail);
        self.x87.model.set(i, Slot::Clean(n));
        self.x87.model.narrow &= !(1 << n);
    }

    /// Loads physical register `r` into host register `n`, branching to
    /// `fail` unless a run may hold it, narrow if `narrow`; uses x0, x1,
    /// x16, x17, d0 and d1.
    fn x87_fetch(&mut self, r: usize, n: u8, narrow: bool, fail: Label) {
        self.x87_slot(r);
        self.emit_all(&[ldr(X, X0, X17, 0), ldrh(X1, X17, 8)]);
        self.pair_from_extended(X0, X1, n, narrow, fail);
    }

    /// Branches to `fail` unless physical register `r` holds what host
    /// register `n` does; uses x0 to x3, x16, x17, d0 and d1.
    fn x87_match(&mut self, r: usize, n: u8, fail: Label) {
        self.pair_to_extended(n, X0, X1);
        self.x87_slot(r);
        self.emit_all(&[ldr(X, X2, X17, 0), ldrh(X3, X17, 8), cmp(X, X0, X2, Lsl, 0)]);
        self.asm.b_cond(Cond::Ne, fail);
        self.emit(cmp(W, X1, X3, Lsl, 0));
        self.asm.b_cond(Cond::Ne, fail);
    }

    pub(super) fn x87_pop(&mut self) {
        self.x87.model.pop();
    }

    pub(super) fn x87_free(&mut self, i: u8) {
        self.x87.model.free(i);
    }

    /// ST(i) = ST(j), by renaming.
    pub(super) fn x87_copy(&mut self, i: u8, j: u8) {
        let n = self.x87.model.reg(j);
        self.x87.model.set(i, Slot::Dirty(n));
    }

    /// Pushes a copy of ST(i), by renaming.
    pub(super) fn x87_push_copy(&mut self, i: u8) {
        let n = self.x87.model.reg(i);
        self.x87.model.push();
        self.x87.model.set(0, Slot::Dirty(n));
    }

    pub(super) fn x87_exchange(&mut self, i: u8) {
        let (a, b) = (self.x87.model.reg(0), self.x87.model.reg(i));
        self.x87.model.set(0, Slot::Dirty(b));
        self.x87.model.set(i, Slot::Dirty(a));
    }

    /// w13 with the current TOP.
    pub(super) fn x87_status(&mut self) {
        self.x87_top(X16);
        self.emit(bfi(W, X13, X16, 11, 3));
    }

    /// The current TOP in `out`.
    fn x87_top(&mut self, out: Reg) {
        let delta = self.x87.model.delta;
        self.emit_all(&[
            add_imm(W, out, X15, u32::from(delta), false),
            and_imm(W, out, out, 7),
        ]);
    }

    /// Stores `model` with status word `status` into `CpuState`; uses x0
    /// to x2, x16, x17, d0 and d1.
    fn x87_publish(&mut self, model: Model, status: Reg) {
        for (r, slot) in model.slots.iter().enumerate() {
            if let Slot::Dirty(n) | Slot::Freed(Some(n)) = *slot {
                self.x87_store_slot(r, n);
            }
        }
        if model
            .slots
            .iter()
            .any(|slot| matches!(slot, Slot::Dirty(_) | Slot::Freed(_)))
        {
            // Rotate the tags to TOP at entry, set them, rotate back.
            self.emit_all(&[
                ldrh(X0, X5, runtime::X87_TAG),
                orr(W, X0, X0, X0, Lsl, 16),
                lsl_imm(W, X1, X15, 1),
                shiftv(W, Shift::Lsr, X0, X0, X1),
            ]);
            for (r, slot) in model.slots.iter().enumerate() {
                let r = r as u32;
                match *slot {
                    Slot::Freed(_) => self.emit(orr_imm(W, X0, X0, 3 << (2 * r))),
                    Slot::Dirty(n) => self.emit_all(&[
                        fmov_from_v(X, X2, v(n)),
                        lsl_imm(X, X2, X2, 1),
                        cmp_imm(X, X2, 0, false),
                        cset(W, X2, Cond::Eq),
                        bfi(W, X0, X2, 2 * r, 2),
                    ]),
                    _ => {}
                }
            }
            self.emit_all(&[
                and_imm(W, X0, X0, 0xffff),
                orr(W, X0, X0, X0, Lsl, 16),
                movz(W, X2, 16, 0),
                sub(W, X2, X2, X1, Lsl, 0),
                shiftv(W, Shift::Lsr, X0, X0, X2),
                strh(X0, X5, runtime::X87_TAG),
            ]);
        }
        let delta = model.delta;
        self.emit_all(&[
            add_imm(W, X16, X15, u32::from(delta), false),
            and_imm(W, X16, X16, 7),
            bfi(W, status, X16, 11, 3),
            strh(status, X5, runtime::X87_STATUS),
        ]);
        if let Some(ip) = model.ip {
            self.mov_imm(X16, ip);
            self.emit(str(W, X16, X5, runtime::X87_IP));
        }
        if let Some(opcode) = model.opcode {
            self.emit_all(&[movz(W, X16, opcode, 0), strh(X16, X5, runtime::X87_OPCODE)]);
        }
    }

    /// Stores host register `n` to physical register `r` and its MMX
    /// alias, leaving the tag; uses x0, x1, x16, x17, d0 and d1.
    fn x87_store_slot(&mut self, r: usize, n: u8) {
        self.pair_to_extended(n, X0, X1);
        self.x87_slot(r);
        self.emit_all(&[
            str(X, X0, X17, 0),
            strh(X1, X17, 8),
            add(X, X17, X5, X16, Lsl, 3),
            str(X, X0, X17, runtime::MMX),
        ]);
    }

    /// Ends the run after its last instruction.
    fn x87_close(&mut self) {
        let model = self.x87.model;
        self.remark(Part::X87Publish);
        self.x87_publish(model, X13);
        self.emit(msr_nzcv(X4));
        let done = self.x87.done.take().expect("an open run");
        self.asm.bind(done);
        self.remark(Part::Body);
        self.x87.active = false;
        self.x87.suspended = false;
        // The stack may have changed under the MMX registers.
        self.vector = super::super::simd::Vector::default();
        self.vector.cpu = true;
    }

    /// Records that a fault at the site just added resumes in a stub that
    /// publishes the state before the instruction.
    pub(in super::super) fn x87_site(&mut self) {
        if !self.x87.active {
            return;
        }
        let label = self.asm.label();
        self.x87_sites.push((self.sites.len() - 1, label));
        self.push_cold(UnitCold::X87Fault {
            label,
            model: self.x87.before,
            saved: self.x87.saved,
            suspended: self.x87.suspended,
            checked: self.checking,
        });
    }

    /// A suspended site holds the canonical guest flags in NZCV, the
    /// status word with TOP in `CpuState` and recipe temporaries in x7
    /// and x8, which this leaves alone.
    pub(in super::super) fn x87_fault(
        &mut self,
        label: Label,
        model: Model,
        saved: bool,
        suspended: bool,
        checked: bool,
    ) {
        self.asm.bind(label);
        self.remark(Part::X87Publish);
        // Checked accesses use x4 as scratch, leaving the guest flags in NZCV.
        if suspended || checked {
            self.emit(mrs_nzcv(X4));
        }
        if suspended {
            self.emit_all(&[
                ldr(X, X5, RUNTIME, runtime::CPU_STATE),
                ldrh(X13, X5, runtime::X87_STATUS),
                ubfx(W, X15, X13, 11, 3),
                sub_imm(W, X15, X15, u32::from(model.delta), false),
                and_imm(W, X15, X15, 7),
            ]);
        }
        self.x87_publish(model, if saved { X14 } else { X13 });
        self.emit(msr_nzcv(X4));
        self.far(self.stubs.fault);
    }

    pub(in super::super) fn x87_cold(&mut self, cold: Cold) {
        if let Some(generic) = cold.generic {
            self.remark(Part::X87Wide);
            self.pair_generic(generic, cold.bail);
        }
        self.asm.bind(cold.bail);
        self.remark(Part::X87Publish);
        self.x87_publish(cold.before, if cold.saved { X14 } else { X13 });
        self.asm.bind(cold.help);
        self.remark(Part::Cold);
        self.index = cold.index;
        self.state = State::CANONICAL;
        self.vector = super::super::simd::Vector::default();
        self.emit(msr_nzcv(X4));
        self.helper(cold.helper);
        self.emit(ldr(X, X5, RUNTIME, runtime::CPU_STATE));
        let Some((hot, help, need)) = cold.next else {
            self.asm.b(cold.done);
            return;
        };
        self.emit(mrs_nzcv(X4));
        self.x87_checks(cold.after, need, help);
        // The helper may have changed any register, and need not have left
        // equal the ones the run shares a host register between: what the
        // run holds comes again from CpuState, live values first, as values
        // a run can hold, and a shared one must match in every register.
        let mut loaded = 0u8;
        for live in [true, false] {
            for (r, slot) in cold.after.slots.iter().enumerate() {
                let n = match *slot {
                    Slot::Clean(n) | Slot::Dirty(n) if live => n,
                    Slot::Freed(Some(n)) if !live => n,
                    _ => continue,
                };
                if loaded & (1 << n) != 0 {
                    self.x87_match(r, n, help);
                } else {
                    let narrow = cold.after.narrow & (1 << n) != 0;
                    self.x87_fetch(r, n, narrow, help);
                }
                loaded |= 1 << n;
            }
        }
        self.asm.b(hot);
    }
}
