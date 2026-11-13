//! x87 runs whose stack lives in host registers (A64F-FP-7).
//!
//! A run holds its stack in v24-v31 as exact pairs of doubles. Its
//! entry checks for pending exceptions, MMX state and the tags it relies on.
//! The model tracks TOP, tags and register placement, including the payloads
//! of empty slots. Copies and exchanges rename values without moving them.
//!
//! Loop arrivals agree on TOP and on which slots hold values. They move
//! pairs to the merge's placement and enter past the guards. v7 carries
//! IP and opcode across merges; w13, w8 and w15 hold status, control and
//! entry TOP during x87 operations. When integer code borrows these
//! registers, a cyclic run saves them in v7 and restores guest NZCV.
//! Straight-line runs suspend status in CpuState.
//!
//! A fault or refused access publishes the state before the instruction.
//! Exits and interpreter helpers publish the state at their boundary, and
//! checked accesses preserve the run and record the dirty SIMD mask.
//! Integer tails end only where clearing SIMD state still supplies the guards
//! of subsequent merges. Outside arrivals pass cold entry checks and fetch
//! the merge model; resident arrivals retain their reconciliation code.
//! Integer-only loops carry the same model around their backedges. Native
//! paths can resume a run beyond intervening fallback segments in trace order.
//!
//! Planning reuses the back end's scratch and indexes arrivals and ownership.
//! Component reachability and prefix facts reject impossible loop attempts.
//! Memo keys retain the start, effective bound and ownership epoch; starts
//! with different entry constraints still receive their own fixed points.

use adv_cpu_core::floating::fast::{Fast, Source};

use crate::asm::Label;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::jit_map::Part;
use crate::runtime;

use super::super::decode::{Alu, Mem, Narrow8or16, Op, Part as OperandPart, Src};
use super::super::simd::{Kind, Loc};
use super::super::trace::{Edge, Mode, Next, Trace};
use super::super::{
    Cold as UnitCold, RUNTIME, State, Translator, X0, X1, X2, X3, X13, X14, X15, X16, X17,
};
use super::pair::Generic;
use super::{V0, X4, X5, X6, X8, X87};
use crate::runtime::Exit;

/// Lane 0 carries suspended status and entry TOP, lane 1 control, and
/// lanes 2 and 3 the IP and opcode at the last loop merge.
const RUN: VReg = VReg::new(7);

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
    cyclic: bool,
    /// Publication bodies shared by the additional arrival plans.
    shared: bool,
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
            cyclic: false,
            shared: false,
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
    pe: bool,
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
        Fast::Leaf(_) => {}
        Fast::Load80 => {
            list.push(Push);
            fresh = true;
        }
        Fast::Store80 => list.extend([Read(0), Pop]),
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
    Reconcile(Model, Model, Label),
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
pub(in super::super) struct InstructionCold {
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
    dirty: u16,
    dispatch: bool,
}

/// Cold continuations of an instruction or a canonical merge arrival.
pub(in super::super) enum Cold {
    Instruction(InstructionCold),
    Tags {
        label: Label,
        done: Label,
        fail: Label,
        value: u8,
    },
    Entry {
        index: usize,
        model: Model,
        need: Need,
        label: Label,
        hot: Label,
        run: std::ops::RangeInclusive<usize>,
    },
}

/// Internal edges from an instruction, independent of its fall-through.
fn edges(trace: &Trace, index: usize) -> impl Iterator<Item = usize> {
    let inst = trace.insts[index];
    [
        inst.taken,
        match inst.next {
            Next::Edge(edge) => Some(edge),
            _ => None,
        },
    ]
    .into_iter()
    .flatten()
    .filter_map(|edge| match edge {
        Edge::Internal(to) => Some(to),
        _ => None,
    })
}

fn backedge(trace: &Trace, index: usize) -> impl Iterator<Item = usize> {
    edges(trace, index).filter(move |&to| to <= index)
}

/// A loop merge keeps even the contents of empty slots. Unknown slots
/// still refer to the entry image, whose tag can be checked in the prologue.
fn loop_meet(a: Known, b: Known) -> Option<Known> {
    use Known::*;
    match (a, b) {
        (Unknown, k) | (k, Unknown) => Some(k),
        (Stale, Empty | Stale) | (Empty, Stale) => Some(Stale),
        (a, b) if a == b => Some(a),
        _ => None,
    }
}

fn loop_model(slots: [Known; 8], delta: u8) -> Model {
    let mut model = canonical(slots, delta);
    model.cyclic = true;
    for (r, k) in slots.into_iter().enumerate() {
        if k == Known::Stale {
            model.slots[r] = Slot::Freed(Some(r as u8));
        }
    }
    model
}

/// Stack addresses relative to a still-known register value. Alignment starts
/// a new origin; the frame pointer retains the old one until LEAVE.
#[derive(Default)]
struct MemoryRun {
    addresses: [Option<(usize, u32)>; 8],
    words: [Option<u16>; 8],
    saved: Vec<((usize, u32), u16)>,
    control: Option<u16>,
    transfers: Vec<((usize, u32), usize)>,
}

impl MemoryRun {
    fn address(&self, mem: Mem) -> Option<(usize, u32)> {
        if mem.fs || mem.address_bits != 32 || mem.index.is_some() {
            return None;
        }
        let (origin, offset) = self.addresses[usize::from(mem.base?)]?;
        Some((origin, offset.wrapping_add(mem.disp)))
    }

    fn store(&mut self, mem: Mem, width: u32, word: Option<u16>, transfer: Option<usize>) {
        let address = self.address(mem);
        let disjoint = |other: (usize, u32)| {
            address.is_some_and(|(origin, offset)| {
                origin == other.0
                    && other.1.wrapping_sub(offset) >= width
                    && offset.wrapping_sub(other.1) >= 8
            })
        };
        self.saved.retain(|&(at, _)| disjoint(at));
        self.transfers.retain(|&(at, _)| disjoint(at));
        if let Some(at) = address {
            if let Some(word) = word {
                self.saved.push((at, word));
            }
            if let Some(index) = transfer {
                self.transfers.push((at, index));
            }
        }
    }

    fn word(&self, mem: Mem) -> Option<u16> {
        let at = self.address(mem)?;
        self.saved.iter().find(|w| w.0 == at).map(|w| w.1)
    }
}

/// Identify memory exchanges and saved-control restores along native paths.
/// Stores remain real accesses; these facts only extend run planning and
/// remove the unmask check when every possible loaded mask is unchanged.
fn memory_runs(trace: &Trace) -> (Vec<usize>, Vec<usize>) {
    let (mut crossings, mut controls) = (Vec::new(), Vec::new());
    let mut facts = MemoryRun::default();
    for (index, inst) in trace.insts.iter().enumerate() {
        if trace.head(index)
            || trace.merge[index]
            || index > 0 && trace.insts[index - 1].next != Next::Fall
        {
            facts = MemoryRun::default();
            facts.addresses[4] = Some((index, 0));
        }
        let transfer = |facts: &MemoryRun, mem| {
            let address = facts.address(mem)?;
            facts.transfers.iter().find(|t| t.0 == address).map(|t| t.1)
        };
        match inst.op {
            Op::X87(x) => match x.fast {
                Fast::StoreControl => {
                    // A fresh save establishes the identity of the current word.
                    if facts.control.is_none() {
                        facts.saved.clear();
                        facts.words = [None; 8];
                    }
                    let control = *facts.control.get_or_insert(0);
                    if let Some(mem) = x.mem {
                        facts.store(mem, 2, Some(control), None);
                    }
                    crossings.push(index);
                }
                Fast::LoadControl => {
                    let word = x.mem.and_then(|m| facts.word(m));
                    if word.is_some() && facts.control.is_some() {
                        controls.push(index);
                    }
                    facts.control = word;
                }
                Fast::Load(Source::F64) => {
                    if let Some(from) = x.mem.and_then(|mem| transfer(&facts, mem)) {
                        crossings.extend([from, index]);
                    }
                }
                Fast::Store { width, integer, .. } => {
                    if let Some(mem) = x.mem {
                        facts.store(
                            mem,
                            u32::from(width),
                            None,
                            (width == 8 && !integer).then_some(index),
                        );
                    }
                }
                Fast::Store80 | Fast::StoreStatus => {
                    if x.mem.is_none() {
                        facts.words[0] = None;
                        facts.addresses[0] = None;
                    }
                    facts.saved.clear();
                    facts.transfers.clear();
                }
                _ => {}
            },
            Op::Simd(simd) if !simd.mmx() => {
                if simd.src == Loc::Mem
                    && simd.width == 8
                    && let Some(from) = transfer(&facts, simd.mem)
                    && matches!(trace.insts[from].op, Op::X87(_))
                {
                    crossings.extend([from, index]);
                }
                if simd.kind == Kind::MaskMove {
                    facts.saved.clear();
                    facts.transfers.clear();
                }
                if simd.dst == Loc::Mem {
                    facts.store(
                        simd.mem,
                        u32::from(simd.width),
                        None,
                        (simd.width == 8).then_some(index),
                    );
                }
                if let Loc::Gpr(r) = simd.dst {
                    facts.words[usize::from(r)] = None;
                    facts.addresses[usize::from(r)] = None;
                }
            }
            Op::Mov(Src::Reg(to), Src::Reg(from)) => {
                facts.addresses[usize::from(to)] = facts.addresses[usize::from(from)];
                facts.words[usize::from(to)] = facts.words[usize::from(from)];
            }
            Op::Mov(Src::Reg(r), _) => {
                facts.addresses[usize::from(r)] = None;
                facts.words[usize::from(r)] = None;
            }
            Op::Mov(Src::Mem(mem), Src::Reg(r)) => {
                facts.store(mem, 4, facts.words[usize::from(r)], None);
            }
            Op::Extend(r, Narrow8or16::Mem(mem, true), false) => {
                facts.words[usize::from(r)] = facts.word(mem);
                facts.addresses[usize::from(r)] = None;
            }
            Op::Alu(Alu::Or, Src::Reg(r), Src::Imm(0xc00)) => {
                facts.words[usize::from(r)] = facts.words[usize::from(r)].map(|w| w | 0xc00);
                facts.addresses[usize::from(r)] = None;
            }
            Op::Alu(op @ (Alu::Add | Alu::Sub), Src::Reg(r), Src::Imm(n)) => {
                let offset = if op == Alu::Add { n } else { n.wrapping_neg() };
                facts.addresses[usize::from(r)] = facts.addresses[usize::from(r)]
                    .map(|(origin, at)| (origin, at.wrapping_add(offset)));
                facts.words[usize::from(r)] = None;
            }
            Op::Lea(r, mem) => {
                facts.addresses[usize::from(r)] = facts.address(mem);
                facts.words[usize::from(r)] = None;
            }
            Op::Alu(Alu::And, Src::Reg(4), Src::Imm(_)) => {
                facts.addresses[4] = Some((index, 0));
                facts.words[4] = None;
            }
            Op::Push(_) | Op::Call(_) => {
                facts.words[4] = None;
                facts.addresses[4] = facts.addresses[4].map(|(o, n)| (o, n.wrapping_sub(4)));
                facts.saved.clear();
                facts.transfers.clear();
            }
            Op::Ret(pop) => {
                facts.words[4] = None;
                facts.addresses[4] =
                    facts.addresses[4].map(|(o, n)| (o, n.wrapping_add(4 + u32::from(pop))));
            }
            Op::Leave => {
                facts.words[4] = None;
                facts.addresses[4] = facts.addresses[5].map(|(o, n)| (o, n.wrapping_add(4)));
                facts.addresses[5] = None;
                facts.words[5] = None;
            }
            Op::Nop | Op::Jcc(..) | Op::Jmp(_) | Op::Alu(Alu::Cmp | Alu::Test, ..) => {}
            Op::Narrow(Some(Alu::Cmp | Alu::Test), ..) => {}
            Op::Alu(_, Src::Reg(r), _) | Op::Step(_, r) | Op::Neg(r) | Op::Not(r) => {
                facts.words[usize::from(r)] = None;
                facts.addresses[usize::from(r)] = None;
            }
            Op::Narrow(op, 2, OperandPart::Reg(r), _) => {
                facts.words[usize::from(r)] = None;
                facts.addresses[usize::from(r)] = None;
                if op.is_some() {
                    facts.transfers.clear();
                }
            }
            _ => {
                // An unmodelled writer cannot contribute to a saved-word proof.
                facts.words = [None; 8];
                facts.control = None;
                facts.addresses = [None; 8];
                facts.saved.clear();
                facts.transfers.clear();
            }
        }
    }
    crossings.sort_unstable();
    crossings.dedup();
    (crossings, controls)
}

type Stack = ([Known; 8], u8);
type Plan = (usize, Need, Vec<(usize, Model)>);
type Plans = std::collections::HashMap<(usize, usize), (usize, Option<Plan>)>;

/// Forward arrivals retain insertion order, with a chain per target.
#[derive(Default)]
struct Pending {
    at: Vec<Option<(usize, usize)>>,
    edges: Vec<(Stack, Option<usize>)>,
    targets: Vec<usize>,
    last: usize,
}

impl Pending {
    fn reset(&mut self, len: usize) {
        self.at.clear();
        self.at.resize(len, None);
        self.edges.clear();
        self.edges.reserve(2 * len);
        self.targets.clear();
        self.targets.reserve(len);
        self.last = 0;
    }

    fn clear(&mut self) {
        self.edges.clear();
        for target in self.targets.drain(..) {
            self.at[target] = None;
        }
        self.last = 0;
    }

    fn push(&mut self, to: usize, state: Stack) {
        let index = self.edges.len();
        self.edges.push((state, None));
        if let Some((first, last)) = self.at[to] {
            self.edges[last].1 = Some(index);
            self.at[to] = Some((first, index));
        } else {
            self.targets.push(to);
            self.at[to] = Some((index, index));
        }
        self.last = self.last.max(to);
    }
}

/// Work buffers and results for one translation, including layout retries.
/// Loop keys retain start and bound: both affect reachable backedges and stack
/// requirements. Loop entry is always Unknown at delta zero; control, PE,
/// access policy and residency are fixed for the translation. Only ownership
/// changes. Successes require the same ownership epoch; failures also hold
/// after more instructions become owned. Helper suffixes use their actual
/// entry model and stored merges and do not share the loop memo tables.
#[derive(Default)]
pub(in super::super) struct Scratch {
    states: Vec<Option<([Known; 8], u8)>>,
    touched: Vec<usize>,
    work: std::collections::VecDeque<usize>,
    models: Vec<(usize, Model)>,
    member: Vec<usize>,
    heads: Vec<(usize, Option<Stack>)>,
    head_at: Vec<Option<usize>>,
    pending: Pending,
    seen: Vec<bool>,
    order: Vec<usize>,
    stack: Vec<(usize, usize)>,
    component: Vec<usize>,
    members: Vec<usize>,
    component_ends: Vec<usize>,
    component_max: Vec<usize>,
    component_head: Vec<Option<usize>>,
    arrivals: Plans,
    ordered: Plans,
}

impl Scratch {
    pub(in super::super) fn reset(&mut self, len: usize) {
        self.states.clear();
        self.states.resize(len, None);
        self.touched.clear();
        self.touched.reserve(len);
        self.work.clear();
        self.work.reserve(len);
        self.models.clear();
        self.models.reserve(len);
        self.member.clear();
        self.member.resize(len, 0);
        self.heads.clear();
        self.heads.reserve(len);
        self.head_at.clear();
        self.head_at.resize(len, None);
        self.pending.reset(len);
        self.arrivals.clear();
        self.ordered.clear();
    }
}

/// A cyclic run can have several emission segments, separated by paths
/// that leave it. Its models and hot labels survive those intervening paths.
#[derive(Clone)]
struct Run {
    models: Vec<(usize, Model)>,
    entries: Vec<(usize, Label)>,
    continuations: Vec<(usize, Option<Label>)>,
    need: Need,
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
    /// Cyclic instruction models, or a straight run's merge models.
    merges: Vec<(usize, Model)>,
    /// Whether the current instruction's entry is bound already.
    bound: bool,
    /// Backward edges in emission order, as (head, latch).
    loops: Vec<(usize, usize)>,
    loop_head: Vec<bool>,
    /// Separate hot labels where canonical arrivals need a cold entry.
    entries: Vec<(usize, Label)>,
    /// Cyclic runs with later emission segments.
    runs: Vec<Run>,
    /// The sparse run that owns each instruction, if any.
    owners: Vec<Option<usize>>,
    /// Largest forward-edge source or memory crossing through each end.
    arrival_from: Vec<Option<usize>>,
    /// Native suffix endpoint after each possible loop latch.
    extended: Vec<usize>,
    /// Prefix counts of instructions an ordered loop cannot contain.
    barriers: Vec<usize>,
    incoming: Vec<Vec<usize>>,
    native: Vec<bool>,
    successors: Vec<[Option<usize>; 2]>,
    /// Reachability ceiling before applying a start's lower bound.
    furthest: Vec<usize>,
    /// Largest unowned backedge target reachable within that ceiling.
    cycle_head: Vec<Option<usize>>,
    /// The canonical label to restore after binding a merge's hot label.
    binding: Option<(usize, Label)>,
    /// Where an internal arrival enters a run that starts at a loop head.
    loop_entry: Option<Label>,
    /// An x87 helper leaves rather than trying to rebuild loop or SIMD state.
    dispatch: bool,
    pub(super) pe: bool,
    pub(super) control_stable: bool,
    publishers: Vec<(Model, Reg, Label)>,
    crossings: Vec<usize>,
    controls: Vec<usize>,
}

impl Cache {
    pub(in super::super) fn for_trace(trace: &Trace) -> Self {
        let mut loops = Vec::new();
        let mut loop_head = vec![false; trace.len()];
        let mut incoming = vec![Vec::new(); trace.len()];
        for from in 0..trace.len() {
            for to in edges(trace, from) {
                incoming[to].push(from);
                if to <= from {
                    loops.push((to, from));
                    loop_head[to] = true;
                }
            }
        }
        let (crossings, controls) = memory_runs(trace);
        Self {
            crossings,
            controls,
            loops,
            loop_head,
            incoming,
            owners: vec![None; trace.len()],
            ..Self::default()
        }
    }
}

impl Translator<'_> {
    pub(in super::super) fn x87_prepare(&mut self) {
        let len = self.trace.len();
        self.x87_scratch.member.fill(0);
        self.x87.cycle_head.resize(len, None);
        if self.x87.loops.is_empty()
            || !(0..len).any(|index| {
                self.cacheable(index)
                    .is_some_and(|x| !matches!(x.fast, Fast::Leaf(_)))
            })
        {
            return;
        }
        self.x87.arrival_from.resize(len, None);
        let mut last = None;
        let mut crossing = 0;
        for index in 0..len {
            if self.x87.crossings.get(crossing) == Some(&index) {
                last = Some(index);
                crossing += 1;
            }
            for &from in &self.x87.incoming[index] {
                if from < index {
                    last = Some(last.map_or(from, |previous| previous.max(from)));
                }
            }
            self.x87.arrival_from[index] = last;
        }
        self.x87.extended.resize(len, 0);
        for index in (0..len).rev() {
            self.x87.extended[index] = if index + 1 < len
                && !self.trace.head(index + 1)
                && (self.cacheable(index + 1).is_some() || self.joins(index + 1))
            {
                self.x87.extended[index + 1]
            } else {
                index
            };
        }
        self.x87.native = (0..len)
            .map(|index| {
                self.cacheable(index)
                    .is_some_and(|x| !matches!(x.fast, Fast::Leaf(_)))
                    || self.joins(index)
            })
            .collect();
        self.x87_reachability();
    }

    /// Compute native reachability on the component DAG. Heads have no
    /// incoming edges: returning to the starting head cannot enlarge reach.
    /// Owned nodes are terminal failures, so bounds cannot hide a refusal.
    fn x87_reachability(&mut self) {
        let len = self.trace.len();
        self.x87.barriers.clear();
        self.x87.barriers.push(0);
        self.x87.successors.clear();
        for index in 0..len {
            let barrier = usize::from(self.trace.head(index))
                + usize::from(!self.x87.native[index] || self.x87.owners[index].is_some());
            self.x87.barriers.push(self.x87.barriers[index] + barrier);
            let mut successors = [None; 2];
            if self.x87.native[index] && self.x87.owners[index].is_none() {
                let fall = (self.trace.insts[index].next == Next::Fall).then_some(index + 1);
                for (slot, to) in successors
                    .iter_mut()
                    .zip(edges(self.trace, index).chain(fall))
                {
                    if to < len && self.x87.native[to] && !self.trace.head(to) {
                        *slot = Some(to);
                    }
                }
            }
            self.x87.successors.push(successors);
        }
        let scratch = &mut *self.x87_scratch;
        scratch.seen.clear();
        scratch.seen.resize(len, false);
        scratch.order.clear();
        scratch.stack.clear();
        for root in 0..len {
            if scratch.seen[root] {
                continue;
            }
            scratch.seen[root] = true;
            scratch.stack.push((root, 0));
            while let Some((index, edge)) = scratch.stack.last_mut() {
                if *edge == 2 {
                    scratch.order.push(*index);
                    scratch.stack.pop();
                } else {
                    let to = self.x87.successors[*index][*edge];
                    *edge += 1;
                    if let Some(to) = to
                        && !scratch.seen[to]
                    {
                        scratch.seen[to] = true;
                        scratch.stack.push((to, 0));
                    }
                }
            }
        }
        scratch.component.clear();
        scratch.component.resize(len, usize::MAX);
        scratch.members.clear();
        scratch.component_ends.clear();
        scratch.component_max.clear();
        scratch.component_head.clear();
        scratch.work.clear();
        for &root in scratch.order.iter().rev() {
            if scratch.component[root] != usize::MAX {
                continue;
            }
            let component = scratch.component_ends.len();
            scratch.component[root] = component;
            scratch.work.push_back(root);
            let mut furthest = root;
            let mut head = None;
            while let Some(index) = scratch.work.pop_front() {
                scratch.members.push(index);
                furthest = furthest.max(index);
                if self.x87.native[index] && self.x87.owners[index].is_none() {
                    for to in backedge(self.trace, index) {
                        if self.x87.native[to] && self.x87.owners[to].is_none() {
                            head = Some(head.map_or(to, |previous: usize| previous.max(to)));
                        }
                    }
                }
                if self.trace.head(index) || !self.x87.native[index] {
                    continue;
                }
                let fall = (index > 0 && self.trace.insts[index - 1].next == Next::Fall)
                    .then_some(index.wrapping_sub(1));
                for from in self.x87.incoming[index].iter().copied().chain(fall) {
                    if self.x87.native[from]
                        && self.x87.owners[from].is_none()
                        && scratch.component[from] == usize::MAX
                    {
                        scratch.component[from] = component;
                        scratch.work.push_back(from);
                    }
                }
            }
            scratch.component_ends.push(scratch.members.len());
            scratch.component_max.push(furthest);
            scratch.component_head.push(head);
        }
        for component in (0..scratch.component_ends.len()).rev() {
            let begin = if component == 0 {
                0
            } else {
                scratch.component_ends[component - 1]
            };
            for &index in &scratch.members[begin..scratch.component_ends[component]] {
                for to in self.x87.successors[index].into_iter().flatten() {
                    let target = scratch.component[to];
                    scratch.component_max[component] =
                        scratch.component_max[component].max(scratch.component_max[target]);
                    if let Some(head) = scratch.component_head[target] {
                        scratch.component_head[component] = Some(
                            scratch.component_head[component]
                                .map_or(head, |previous| previous.max(head)),
                        );
                    }
                }
            }
        }
        self.x87.furthest.clear();
        self.x87.cycle_head.clear();
        for component in scratch.component.iter().copied() {
            self.x87.furthest.push(scratch.component_max[component]);
            self.x87.cycle_head.push(scratch.component_head[component]);
        }
    }

    pub(super) fn x87_saved_control(&self) -> bool {
        self.x87.active && self.x87.controls.contains(&self.index)
    }

    /// The cacheable x87 instruction at `index`, if it is one.
    fn cacheable(&self, index: usize) -> Option<X87> {
        let inst = self.trace.insts[index];
        match inst.op {
            Op::X87(x) if !matches!(x.fast, Fast::Examine | Fast::Round | Fast::Scale) => Some(x),
            _ => None,
        }
    }

    /// Forms that leave the x87 stack and its host registers alone.
    fn joins(&self, index: usize) -> bool {
        let inst = self.trace.insts[index];
        if inst.locked.is_some() || inst.mode == Mode::Real {
            return false;
        }
        match inst.op {
            Op::Ret(_) => matches!(inst.mode, Mode::Inline(_)),
            Op::Simd(simd) => {
                !simd.mmx()
                    && !((self.checked)(inst.pc)
                        && simd.memory()
                        && matches!(simd.kind, Kind::InsertWord))
            }
            Op::Body(_)
            | Op::CallIndirect(_)
            | Op::JmpIndirect(_)
            | Op::Syscall
            | Op::Helper(_)
            | Op::X87(_)
            | Op::Fild64(..)
            | Op::Timestamp => false,
            _ => true,
        }
    }

    /// The state of the run at its merge `index`, if it is one.
    fn merge_model(&self, index: usize) -> Option<Model> {
        if !self.trace.merge[index] {
            return None;
        }
        self.x87.merges.iter().find(|m| m.0 == index).map(|m| m.1)
    }

    /// The last instruction of a run from `start` in state `from`, what
    /// it needs, and the states of its merges, which are `stored` once the
    /// run is open; `None` when not even the first fits.
    fn plan(
        &mut self,
        start: usize,
        from: Model,
        stored: Option<&[(usize, Model)]>,
    ) -> Option<Plan> {
        let mut scratch = std::mem::take(self.x87_scratch);
        let result = self.plan_with(start, from, stored, &mut scratch);
        *self.x87_scratch = scratch;
        result
    }

    fn plan_with(
        &self,
        start: usize,
        from: Model,
        stored: Option<&[(usize, Model)]>,
        scratch: &mut Scratch,
    ) -> Option<Plan> {
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
        // A helper can resume into the integer-only tail of an open run.
        let mut end = stored.map(|_| start);
        let merges = &mut scratch.models;
        merges.clear();
        let pending = &mut scratch.pending;
        pending.clear();
        let mut stored_at = 0;
        // Whether the instruction before jumps instead of falling through.
        let mut gap = false;
        let mut after_leaf = false;
        let mut pe_at = None;
        'run: for index in start..self.trace.len() {
            if self.x87.owners[index].is_some() {
                break;
            }
            if index != start && self.trace.head(index) {
                break;
            }
            if gap && !self.trace.merge[index] {
                break;
            }
            if index != start && self.trace.merge[index] {
                let met = match stored {
                    Some(stored) => {
                        while stored_at < stored.len() && stored[stored_at].0 < index {
                            stored_at += 1;
                        }
                        match stored.get(stored_at) {
                            Some(&(at, model)) if at == index => model,
                            _ => break,
                        }
                    }
                    None => {
                        let first = pending.at[index].map(|at| at.0);
                        if gap && first.is_none() {
                            break;
                        }
                        if gap {
                            (known, delta) = pending.edges[first.unwrap()].0;
                        }
                        let mut met = known;
                        let mut arrival = first;
                        while let Some(at) = arrival {
                            let ((slots, top), next) = pending.edges[at];
                            if top != delta {
                                break 'run;
                            }
                            for r in 0..8 {
                                let Some(k) = meet(met[r], slots[r]) else {
                                    break 'run;
                                };
                                met[r] = k;
                            }
                            arrival = next;
                        }
                        let model = canonical(met, delta);
                        merges.push((index, model));
                        model
                    }
                };
                // An arrival that has not read a value the merge holds
                // fetches it.
                for (r, slot) in met.slots.iter().enumerate() {
                    let mut unread = !gap && known[r] == Known::Unknown;
                    let mut arrival = pending.at[index].map(|at| at.0);
                    while let Some(at) = arrival {
                        unread |= pending.edges[at].0.0[r] == Known::Unknown;
                        arrival = pending.edges[at].1;
                    }
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
                    if matches!(x.fast, Fast::Leaf(_)) {
                        if pending.last > index {
                            break;
                        }
                        known = [Known::Unknown; 8];
                        delta = 0;
                        after_leaf = true;
                    }
                    let mut local = Need::default();
                    let needed = if after_leaf { &mut local } else { &mut need };
                    if step(&mut known, &mut delta, needed, x.fast).is_none() {
                        break;
                    }
                    if pe_at.is_none() && self.pe_candidate(index, x.fast, known, delta) {
                        pe_at = Some(index);
                    }
                    end = Some(index);
                }
                None if self.joins(index) => {
                    if end.is_some()
                        && self
                            .x87
                            .crossings
                            .iter()
                            .any(|&at| at >= start && at <= index)
                        && super::super::residency::can_drop_after(
                            self.trace,
                            &self.entry,
                            self.checked,
                            index,
                        )
                    {
                        end = Some(index);
                    }
                    if backedge(self.trace, index).next().is_some() {
                        break;
                    }
                    if let Some(Edge::Internal(to)) = self.trace.insts[index].taken {
                        pending.push(to, (known, delta));
                    }
                    if let Next::Edge(Edge::Internal(to)) = self.trace.insts[index].next
                        && to > index
                    {
                        pending.push(to, (known, delta));
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
        need.pe = pe_at.is_some_and(|index| index < end);
        merges.retain(|m| m.0 <= end);
        Some((end, need, merges.clone()))
    }

    /// Memory exchanges and loops with forward arrivals use a model at every
    /// arrival point. Other loops retain their ordered plan and emitted code.
    fn plan_loop(&mut self, start: usize, end: usize) -> Option<Plan> {
        if self.x87.arrival_from[end].is_some_and(|from| from >= start) {
            let extended = self.x87.extended[end];
            self.plan_arrivals(start, extended)
                .or_else(|| self.plan_arrivals(start, end))
                .or_else(|| self.plan_ordered_loop(start, end))
        } else {
            self.plan_ordered_loop(start, end)
        }
    }

    /// Follow native edges, retaining a model at each member. Unsupported
    /// paths leave the run; native backedges can reach later emission segments.
    fn plan_arrivals(&mut self, start: usize, end: usize) -> Option<Plan> {
        // Bounds beyond native reach see exactly the same graph. Keep one
        // extra index to preserve the strict PE-candidate comparison.
        let end = end.min(self.x87.furthest[start] + 1);
        let epoch = self.x87.runs.len();
        if let Some((previous, plan)) = self.x87_scratch.arrivals.get(&(start, end))
            && (plan.is_none() && *previous <= epoch || *previous == epoch)
        {
            return plan.clone();
        }
        let mut scratch = std::mem::take(self.x87_scratch);
        let result = self.plan_arrivals_with(start, end, &mut scratch);
        scratch
            .arrivals
            .insert((start, end), (epoch, result.clone()));
        *self.x87_scratch = scratch;
        result
    }

    fn plan_arrivals_with(&self, start: usize, end: usize, scratch: &mut Scratch) -> Option<Plan> {
        for index in scratch.touched.drain(..) {
            scratch.states[index] = None;
        }
        let states = &mut scratch.states;
        states[start] = Some(([Known::Unknown; 8], 0));
        scratch.touched.push(start);
        let work = &mut scratch.work;
        work.clear();
        work.push_back(start);
        let mut need = Need::default();
        let mut seen_x87 = false;
        while let Some(index) = work.pop_front() {
            if self.x87.owners[index].is_some() {
                return None;
            }
            let (mut slots, mut delta) = states[index]?;
            if let Some(x) = self.cacheable(index) {
                if matches!(x.fast, Fast::Leaf(_)) {
                    return None;
                }
                step(&mut slots, &mut delta, &mut need, x.fast)?;
                seen_x87 = true;
                need.pe |= index < end && self.pe_candidate(index, x.fast, slots, delta);
            } else if !self.joins(index) {
                return None;
            }
            let fall = (self.trace.insts[index].next == Next::Fall).then_some(index + 1);
            for to in edges(self.trace, index).chain(fall) {
                if to < start
                    || to > end
                    || to != start && self.trace.head(to)
                    || !(self
                        .cacheable(to)
                        .is_some_and(|x| !matches!(x.fast, Fast::Leaf(_)))
                        || self.joins(to))
                {
                    continue;
                }
                let mut incoming = (slots, delta);
                if let Some((previous, top)) = states[to] {
                    if top != delta {
                        return None;
                    }
                    for r in 0..8 {
                        let next = loop_meet(previous[r], slots[r])?;
                        if previous[r] == Known::Unknown || slots[r] == Known::Unknown {
                            match next {
                                Known::Value => need.value |= 1 << r,
                                Known::Empty | Known::Stale => need.empty |= 1 << r,
                                Known::Unknown => {}
                            }
                        }
                        incoming.0[r] = next;
                    }
                }
                if states[to] != Some(incoming) {
                    if states[to].is_none() {
                        scratch.touched.push(to);
                    }
                    states[to] = Some(incoming);
                    work.push_back(to);
                }
            }
        }
        if !seen_x87
            || need.value & need.empty != 0
            || !self.x87.loops.iter().any(|&(to, from)| {
                from <= end && to >= start && states[to].is_some() && states[from].is_some()
            })
        {
            return None;
        }
        let end = (start..=end).rfind(|&index| states[index].is_some())?;
        for index in start..=end {
            if states[index].is_some()
                && (index == end || states[index + 1].is_none())
                && !super::super::residency::can_drop_after(
                    self.trace,
                    &self.entry,
                    self.checked,
                    index,
                )
            {
                return None;
            }
        }
        scratch.models.clear();
        scratch.models.extend((start..=end).filter_map(|index| {
            let (slots, delta) = states[index]?;
            Some((
                index,
                Model {
                    shared: true,
                    ..loop_model(slots, delta)
                },
            ))
        }));
        Some((end, need, scratch.models.clone()))
    }

    /// Proves every arrival at each loop head has the same TOP and tag
    /// model. Starting with forward arrivals, widen the slots to retain
    /// until the backedges no longer add a value to the entry model.
    fn plan_ordered_loop(&mut self, start: usize, end: usize) -> Option<Plan> {
        if self.x87.barriers[end + 1] - self.x87.barriers[start]
            > usize::from(self.trace.head(start))
        {
            return None;
        }
        let epoch = self.x87.runs.len();
        if let Some((previous, plan)) = self.x87_scratch.ordered.get(&(start, end))
            && (plan.is_none() && *previous <= epoch || *previous == epoch)
        {
            return plan.clone();
        }
        let mut scratch = std::mem::take(self.x87_scratch);
        let result = self.plan_ordered_loop_with(start, end, &mut scratch);
        scratch
            .ordered
            .insert((start, end), (epoch, result.clone()));
        *self.x87_scratch = scratch;
        result
    }

    fn plan_ordered_loop_with(
        &self,
        start: usize,
        end: usize,
        scratch: &mut Scratch,
    ) -> Option<Plan> {
        let heads = &mut scratch.heads;
        for &(index, _) in heads.iter() {
            scratch.head_at[index] = None;
        }
        heads.clear();
        for &(to, from) in &self.x87.loops {
            if to >= start && from <= end {
                scratch.head_at[to] = Some(usize::MAX);
            }
        }
        for (index, head) in (start..=end).zip(&mut scratch.head_at[start..=end]) {
            if head.is_some() {
                *head = Some(heads.len());
                heads.push((index, None));
            }
        }
        let mut changed = true;
        while changed {
            changed = false;
            let mut need = Need::default();
            let merges = &mut scratch.models;
            merges.clear();
            let pending = &mut scratch.pending;
            pending.clear();
            let mut model = Some(([Known::Unknown; 8], 0));
            let mut seen_x87 = false;
            for index in start..=end {
                if self.x87.owners[index].is_some() {
                    return None;
                }
                if index != start && self.trace.head(index) {
                    return None;
                }
                let head = scratch.head_at[index];
                if self.trace.merge[index] || head.is_some() {
                    let mut meet = |incoming: ([Known; 8], u8)| -> Option<()> {
                        let Some((ref mut slots, delta)) = model else {
                            model = Some(incoming);
                            return Some(());
                        };
                        if delta != incoming.1 {
                            return None;
                        }
                        for (r, (&b, a)) in incoming.0.iter().zip(slots).enumerate() {
                            let next = loop_meet(*a, b)?;
                            if *a == Known::Unknown || b == Known::Unknown {
                                match next {
                                    Known::Value => need.value |= 1 << r,
                                    Known::Stale | Known::Empty => need.empty |= 1 << r,
                                    Known::Unknown => {}
                                }
                            }
                            *a = next;
                        }
                        Some(())
                    };
                    let mut arrival = pending.at[index].map(|at| at.0);
                    while let Some(at) = arrival {
                        let (incoming, next) = pending.edges[at];
                        meet(incoming)?;
                        arrival = next;
                    }
                    if let Some(h) = head
                        && let Some(seed) = heads[h].1
                    {
                        meet(seed)?;
                    }
                    let (slots, delta) = model?;
                    if let Some(h) = head {
                        heads[h].1 = model;
                    }
                    merges.push((index, loop_model(slots, delta)));
                }
                let (mut slots, mut delta) = model?;
                if !self.trace.merge[index] && head.is_none() {
                    merges.push((index, loop_model(slots, delta)));
                }
                if let Some(x) = self.cacheable(index) {
                    if matches!(x.fast, Fast::Leaf(_)) {
                        return None;
                    }
                    step(&mut slots, &mut delta, &mut need, x.fast)?;
                    seen_x87 = true;
                    need.pe |= index < end && self.pe_candidate(index, x.fast, slots, delta);
                } else if !self.joins(index) {
                    return None;
                }
                let state = (slots, delta);
                for to in edges(self.trace, index) {
                    if to > index && to <= end {
                        pending.push(to, state);
                    } else if to >= start && to <= index {
                        let h = scratch.head_at[to]?;
                        let (seed, top) = heads[h].1.as_mut()?;
                        if *top != delta {
                            return None;
                        }
                        for (a, b) in seed.iter_mut().zip(slots) {
                            let next = loop_meet(*a, b)?;
                            changed |= next != *a;
                            *a = next;
                        }
                    }
                }
                model = (self.trace.insts[index].next == Next::Fall).then_some(state);
            }
            if need.value & need.empty != 0 || !seen_x87 {
                return None;
            }
            if !changed {
                if !super::super::residency::can_drop_after(
                    self.trace,
                    &self.entry,
                    self.checked,
                    end,
                ) {
                    return None;
                }
                return Some((end, need, merges.clone()));
            }
        }
        unreachable!()
    }

    fn pe_candidate(&self, index: usize, fast: Fast, slots: [Known; 8], delta: u8) -> bool {
        self.pe
            && (matches!(fast, Fast::Store { .. })
                || matches!(fast, Fast::Arith { .. }) && self.pair_precision())
            && self.c1_dead_after(index, slots, delta)
    }

    /// Whether the C1 that the instruction at `index` computes is dead:
    /// the next one sets C1 and can neither bail nor fault, its operands
    /// already in host registers once `model` has run `x`.
    fn c1_dead(&self, index: usize, x: X87, model: Model) -> bool {
        if index >= self.x87.end {
            return false;
        }
        let mut known = known(model);
        let (mut delta, mut need) = (model.delta, Need::default());
        step(&mut known, &mut delta, &mut need, x.fast).is_some()
            && self.c1_dead_after(index, known, delta)
    }

    fn c1_dead_after(&self, index: usize, known: [Known; 8], delta: u8) -> bool {
        if self.trace.insts[index].next != Next::Fall || self.trace.merge[index + 1] {
            return false;
        }
        let Some(next) = self.cacheable(index + 1) else {
            return false;
        };
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
        let (metadata, generic) = self.pair_body(x, bail, c1_dead);
        let mut model = self.x87.model;
        if metadata {
            model.ip = Some(self.trace.insts[index].pc);
            if x.opcode.is_some() {
                model.opcode = x.opcode;
            }
        }
        self.x87.model = model;
        let done = self.x87.done.expect("an open run");
        let next = (!self.x87.dispatch && index < self.x87.end).then(|| {
            // A merge next reconciles from this state first.
            let merges = std::mem::take(&mut self.x87.merges);
            let (_, mut need, _) = self
                .plan(index + 1, model, Some(&merges))
                .expect("a planned run");
            self.x87.merges = merges;
            need.pe = self.x87.pe;
            (self.x87.hot[offset + 1], self.x87.help[offset + 1], need)
        });
        if matches!(x.fast, Fast::Leaf(_))
            && let Some((_, help, need)) = next
        {
            self.x87_checks(model, need, help);
        }
        self.push_cold(UnitCold::X87(Cold::Instruction(InstructionCold {
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
            dirty: self.vector.dirty,
            dispatch: self.x87.model.cyclic || (self.x87.dispatch && index < self.x87.end),
        })));
        if index == self.x87.end && !self.x87.model.cyclic {
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
        if !self.x87.dispatch && index > self.x87.start && self.cacheable(index - 1).is_some() {
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
        if self.x87.model.cyclic {
            self.emit_all(&[
                bfi(W, X13, X15, 11, 3),
                vins_general(2, RUN, 0, X13),
                vins_general(2, RUN, 1, X8),
                msr_nzcv(X4),
            ]);
        } else {
            self.x87_status();
            self.emit_all(&[strh(X13, X5, runtime::X87_STATUS), msr_nzcv(X4)]);
        }
        self.x87.suspended = true;
    }

    /// The fall-through arrival at merge `index` of the run, before the
    /// merge's own code, which runs suspended in the merge's state.
    pub(in super::super) fn x87_merge(&mut self, index: usize) {
        let to = self.merge_model(index).expect("a planned merge");
        // Resident edges target the hot label. Other edges keep the original
        // label, which is bound by the cold entry after body emission.
        if let Some(&(_, hot)) = self.x87.entries.iter().find(|e| e.0 == index) {
            let canonical = std::mem::replace(&mut self.labels[index], hot);
            self.x87.binding = Some((index, canonical));
        }
        if self.trace.insts[index - 1].next != Next::Fall {
            // Only jumps arrive, each reconciled already.
            self.x87.model = to;
            if to.cyclic {
                self.x87.suspended = true;
            }
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
            Some(model) => Arrive::Reconcile(
                from,
                model,
                if to == self.x87.start {
                    self.x87.loop_entry.expect("an internal loop entry")
                } else {
                    self.x87
                        .entries
                        .iter()
                        .find(|e| e.0 == to)
                        .map_or(self.labels[to], |e| e.1)
                },
            ),
            None => Arrive::Publish(from),
        })
    }

    /// The x87 part of an internal edge from `from` to `to`, in cold code.
    pub(in super::super) fn x87_arrive(&mut self, arrive: Arrive, from: usize, to: usize) {
        match arrive {
            Arrive::Publish(model) => self.x87_leave(Leave::Publish(model)),
            Arrive::Reconcile(model, merge, _) => {
                let eip = self.trace.insts[to].pc;
                self.x87_reconcile(model, merge, self.trace.unused(from), eip);
                if !merge.cyclic {
                    self.remark(Part::Cold);
                }
            }
        }
    }

    /// Brings a suspended run from state `from` to the merge state `to`;
    /// a fetch that fails publishes and leaves for `eip` with `refund`.
    fn x87_reconcile(&mut self, from: Model, to: Model, refund: u32, eip: u32) {
        if to.cyclic {
            self.x87_loop_reconcile(from, to, refund, eip);
            return;
        }
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
                simd_dirty: self.vector.dirty,
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

    /// Reconciles a loop arrival without writing the guest image. Only
    /// entries from before the loop can lack a resident value.
    fn x87_loop_reconcile(&mut self, from: Model, to: Model, refund: u32, eip: u32) {
        let mut moves = Vec::new();
        let mut fetches = Vec::new();
        for (r, slot) in to.slots.iter().enumerate() {
            let n = match *slot {
                Slot::Dirty(n) | Slot::Freed(Some(n)) => n,
                _ => continue,
            };
            match from.slots[r] {
                Slot::Clean(m) | Slot::Dirty(m) | Slot::Freed(Some(m)) => moves.push((v(n), v(m))),
                _ => fetches.push((r, n)),
            }
        }
        if !fetches.is_empty() {
            self.canonicalize();
            self.emit_all(&[mrs_nzcv(X4), ldr(X, X5, RUNTIME, runtime::CPU_STATE)]);
            self.x87_unspill_status();
            let mut published = to;
            published.ip = from.ip;
            published.opcode = from.opcode;
            for (r, _) in &fetches {
                published.slots[*r] = from.slots[*r];
            }
            // Once the old registers move, a failed load publishes their
            // new placement and leaves all unfetched slots in CpuState.
            if !moves.is_empty() {
                self.x87_moves(moves);
            }
            let moved_fail = self.asm.label();
            self.push_cold(UnitCold::Leave {
                label: moved_fail,
                state: State::CANONICAL,
                simd_dirty: self.vector.dirty,
                refund,
                exit: Exit::Dispatch,
                eip,
                link: false,
                x87: Some(Leave::Restore(Some(published))),
            });
            for (r, n) in fetches {
                self.x87_fetch(r, n, false, moved_fail);
            }
            self.emit(msr_nzcv(X4));
        } else {
            self.x87_moves(moves);
        }
        if let Some(ip) = from.ip {
            self.mov_imm(X16, ip);
            self.emit(vins_general(2, RUN, 2, X16));
        }
        if let Some(opcode) = from.opcode {
            self.emit_all(&[movz(W, X16, opcode, 0), vins_general(2, RUN, 3, X16)]);
        }
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
        if self.x87.model.cyclic {
            self.x87_unspill_status();
            self.emit(vumov(2, X8, RUN, 1));
            self.x87.guarded = false;
        } else {
            self.x87_reload(self.x87.model.delta);
        }
        self.x87.suspended = false;
    }

    fn x87_unspill_status(&mut self) {
        self.emit_all(&[vumov(2, X13, RUN, 0), ubfx(W, X15, X13, 11, 3)]);
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
                if model.cyclic {
                    self.x87_unspill_status();
                } else {
                    self.x87_reload(model.delta);
                }
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

    pub(in super::super) fn x87_start_integer(&mut self) {
        if !self.x87.active
            && (self.x87.loop_head[self.index] || self.x87.owners[self.index].is_some())
        {
            self.x87_open();
        }
    }

    /// The prologue of a run from the current instruction.
    fn x87_open(&mut self) -> bool {
        let start = self.index;
        if let Some(owner) = self.x87.owners[start] {
            let run = self.x87.runs[owner].clone();
            let position = run.models.iter().position(|m| m.0 == start).unwrap();
            let end = run.models[position..]
                .iter()
                .enumerate()
                .take_while(|&(offset, m)| m.0 == start + offset)
                .last()
                .unwrap()
                .1
                .0;
            self.x87_segment(start, end);
            self.x87.model = run.models[position].1;
            self.x87.suspended = true;
            self.x87.pe = run.need.pe;
            self.x87.control_stable =
                self.stable_control(run.models[0].0, run.models.last().unwrap().0);
            self.x87.dispatch = true;
            self.x87.entries = run.entries;
            self.x87.merges = run.models;
            let hot = self
                .x87
                .entries
                .iter()
                .find(|e| e.0 == start)
                .map_or(self.labels[start], |e| e.1);
            if let Some(label) = run.continuations.iter().find(|c| c.0 == start).unwrap().1 {
                self.asm.b(label);
            }
            if hot != self.labels[start] {
                self.asm.bind(hot);
            }
            self.x87.loop_entry = Some(hot);
            return true;
        }
        let mut loop_plan = None;
        let candidates = if self.x87.cycle_head[start].is_some_and(|head| head >= start) {
            self.x87.loops.len()
        } else {
            0
        };
        for index in (0..candidates).rev() {
            let (to, end) = self.x87.loops[index];
            if to >= start {
                loop_plan = self.plan_loop(start, end);
                if loop_plan.is_some() {
                    break;
                }
            }
        }
        let cyclic = loop_plan.is_some();
        let Some((end, need, merges)) = loop_plan.or_else(|| {
            self.cacheable(start)?;
            self.plan(start, Model::default(), None)
        }) else {
            return false;
        };
        self.remark(Part::X87Entry);
        self.flush_simd();
        self.canonicalize();
        self.cpu();
        let sparse = cyclic && merges.windows(2).any(|m| m[1].0 > m[0].0 + 1);
        let segment_end = if sparse {
            merges
                .iter()
                .enumerate()
                .take_while(|&(offset, m)| m.0 == start + offset)
                .last()
                .unwrap()
                .1
                .0
        } else {
            end
        };
        self.x87_segment(start, segment_end);
        self.x87.model = Model {
            shared: merges.iter().any(|m| m.1.shared),
            cyclic,
            ..Model::default()
        };
        self.x87.suspended = false;
        self.x87.merges = merges;
        self.x87.pe = need.pe;
        self.x87.control_stable = cyclic && self.stable_control(start, end);
        self.x87.entries.clear();
        if sparse {
            for &(index, _) in &self.x87.merges {
                self.x87_scratch.member[index] = start + 1;
            }
        }
        let mut continuations = Vec::new();
        for (index, model) in self.x87.merges.clone() {
            if index == start || !self.trace.merge[index] {
                continue;
            }
            let segment = sparse && self.x87_scratch.member[index - 1] != start + 1;
            let outside = (segment && self.trace.insts[index - 1].next == Next::Fall)
                || self.x87.incoming[index].iter().any(|&from| {
                    if sparse {
                        self.x87_scratch.member[from] != start + 1
                    } else {
                        from < start || from > end
                    }
                });
            if !outside && !segment {
                continue;
            }
            let hot = self.asm.label();
            self.x87.entries.push((index, hot));
            let label = if segment && outside {
                self.asm.label()
            } else {
                self.labels[index]
            };
            if segment {
                continuations.push((index, outside.then_some(label)));
            }
            if !outside {
                continue;
            }
            let mut needed = need;
            for (r, slot) in model.slots.iter().enumerate() {
                match slot {
                    Slot::Clean(_) | Slot::Dirty(_) => {
                        needed.value |= 1 << r;
                        needed.empty &= !(1 << r);
                    }
                    Slot::Freed(_) => {
                        needed.empty |= 1 << r;
                        needed.value &= !(1 << r);
                    }
                    Slot::Entry => {}
                }
            }
            self.push_cold(UnitCold::X87(Cold::Entry {
                index,
                model,
                need: needed,
                label,
                hot,
                run: start..=end,
            }));
        }
        self.x87.dispatch = cyclic
            || self.trace.insts[start..=end]
                .iter()
                .any(|inst| matches!(inst.op, Op::Simd(simd) if simd.footprint().1 != 0));
        self.emit(mrs_nzcv(X4));
        let help = self.x87.help[0];
        self.x87.end = end;
        self.x87_checks(self.x87.model, need, help);
        self.x87.end = segment_end;
        if self.x87.control_stable {
            self.x87_require_bit(X8, 16, help);
        }
        if cyclic {
            self.emit_all(&[
                ldr(W, X16, X5, runtime::X87_IP),
                vins_general(2, RUN, 2, X16),
                ldrh(X16, X5, runtime::X87_OPCODE),
                vins_general(2, RUN, 3, X16),
            ]);
            if let Some(model) = self.merge_model(start) {
                for (r, slot) in model.slots.iter().enumerate() {
                    if let Slot::Dirty(n) | Slot::Freed(Some(n)) = *slot {
                        self.x87_fetch(r, n, false, help);
                    }
                }
                self.x87.model = model;
                self.x87_suspend();
                let entry = self.asm.label();
                self.asm.bind(entry);
                self.x87.loop_entry = Some(entry);
                if sparse {
                    self.x87.entries.push((start, entry));
                }
            }
            if self.cacheable(start).is_none() {
                self.push_cold(UnitCold::Leave {
                    label: help,
                    state: State::CANONICAL,
                    simd_dirty: 0,
                    refund: self.trace.owed(start),
                    exit: Exit::Interpret,
                    eip: self.trace.insts[start].pc,
                    link: false,
                    x87: Some(Leave::Restore(None)),
                });
            }
        }
        if sparse {
            let owner = self.x87.runs.len();
            for &(index, _) in &self.x87.merges {
                self.x87.owners[index] = Some(owner);
            }
            self.x87.runs.push(Run {
                models: self.x87.merges.clone(),
                entries: self.x87.entries.clone(),
                continuations,
                need,
            });
            self.x87_reachability();
        }
        self.remark(Part::Body);
        true
    }

    fn x87_segment(&mut self, start: usize, end: usize) {
        let count = end - start + 1;
        self.x87.hot = (0..count).map(|_| self.asm.label()).collect();
        self.x87.help = (0..count).map(|_| self.asm.label()).collect();
        self.x87.done = Some(self.asm.label());
        self.x87.start = start;
        self.x87.end = end;
        self.x87.active = true;
        self.x87.bound = false;
        self.x87.loop_entry = None;
    }

    fn stable_control(&self, start: usize, end: usize) -> bool {
        let models = self
            .x87
            .runs
            .iter()
            .map(|run| run.models.as_slice())
            .chain(std::iter::once(self.x87.merges.as_slice()))
            .find(|models| {
                models.first().is_some_and(|m| m.0 == start)
                    && models.last().is_some_and(|m| m.0 == end)
            });
        let mut arithmetic = false;
        for (index, inst) in (start..=end).zip(&self.trace.insts[start..=end]) {
            if models.is_some_and(|models| models.binary_search_by_key(&index, |m| m.0).is_err()) {
                continue;
            }
            match inst.op {
                Op::X87(X87 {
                    fast: Fast::Arith { .. },
                    ..
                }) => arithmetic = true,
                Op::X87(X87 {
                    fast: Fast::LoadControl,
                    ..
                }) => return false,
                _ => {}
            }
        }
        self.pair_precision() && arithmetic
    }

    pub(in super::super) fn x87_require_bit(&mut self, reg: Reg, bit: u32, to: Label) {
        let label = self.asm.label();
        self.asm.tbz(false, reg, bit, label);
        self.push_cold(UnitCold::Bridge { label, to });
    }

    /// Loads w13, w15 and w8 for entering a run in state `model`,
    /// branching to `fail` unless the run may go on.
    fn x87_checks(&mut self, model: Model, need: Need, fail: Label) {
        self.x87_reload(model.delta);
        if need.pe {
            self.x87_require_bit(X13, 5, fail);
        }
        self.emit_all(&[
            ldr(W, X16, X5, runtime::MMX_MODE),
            and_imm(W, X17, X13, 0x80),
            orr(W, X16, X16, X17, Lsl, 0),
        ]);
        self.asm.cbz(W, true, X16, fail);
        if need.value | need.empty == 0 {
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
            let tags = self.asm.label();
            let done = self.asm.label();
            self.mov_imm(X17, spread(need.value, 3));
            self.emit(and(W, X17, X17, X16, Lsl, 0));
            self.asm.cbz(W, true, X17, tags);
            self.asm.bind(done);
            self.push_cold(UnitCold::X87(Cold::Tags {
                label: tags,
                done,
                fail,
                value: need.value,
            }));
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
        let (zero, done) = self.pair_from_extended(X0, X1, n, narrow, fail);
        self.asm.bind(zero);
        // A resident zero can become dirty at a merge. Keep a canonical
        // normal tag with a zero payload on the interpreter path instead.
        self.emit_all(&[
            add_imm(W, X16, X15, r as u32, false),
            and_imm(W, X16, X16, 7),
            lsl_imm(W, X16, X16, 1),
            ldrh(X17, X5, runtime::X87_TAG),
            shiftv(W, Shift::Lsr, X17, X17, X16),
        ]);
        self.x87_require_bit(X17, 0, fail);
        self.pair_zero(X1, n, fail);
        self.asm.bind(done);
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
    /// to x2, x16, x17, d0 and d1, and x3 for shared cold bodies.
    pub(super) fn x87_publish(&mut self, model: Model, status: Reg) {
        if model.shared {
            let state = Model {
                ip: None,
                opcode: None,
                cyclic: false,
                ..model
            };
            let entry = self
                .x87
                .publishers
                .iter()
                .find(|&&(m, s, _)| m == state && s == status)
                .map(|&(_, _, label)| label);
            let entry = entry.unwrap_or_else(|| {
                let (entry, skip) = (self.asm.label(), self.asm.label());
                self.x87.publishers.push((state, status, entry));
                self.asm.b(skip);
                self.asm.bind(entry);
                self.x87_publish_state(state, status);
                self.emit(br(X3));
                self.asm.bind(skip);
                entry
            });
            let done = self.asm.label();
            self.asm.adr(X3, done);
            self.asm.b(entry);
            self.asm.bind(done);
        } else {
            self.x87_publish_state(model, status);
        }
        if let Some(ip) = model.ip {
            self.mov_imm(X16, ip);
            self.emit(str(W, X16, X5, runtime::X87_IP));
        } else if model.cyclic {
            self.emit_all(&[vumov(2, X16, RUN, 2), str(W, X16, X5, runtime::X87_IP)]);
        }
        if let Some(opcode) = model.opcode {
            self.emit_all(&[movz(W, X16, opcode, 0), strh(X16, X5, runtime::X87_OPCODE)]);
        } else if model.cyclic {
            self.emit_all(&[vumov(2, X16, RUN, 3), strh(X16, X5, runtime::X87_OPCODE)]);
        }
    }

    /// The stack, tags and status part, using x3 only as its cold return address.
    fn x87_publish_state(&mut self, model: Model, status: Reg) {
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
        self.flush_simd();
        self.x87_end();
    }

    pub(in super::super) fn x87_before_edge(&mut self) {
        if self.x87.active {
            self.x87_suspend();
        }
    }

    pub(in super::super) fn x87_end_integer(&mut self) {
        if let Some((index, label)) = self.x87.binding.take() {
            self.labels[index] = label;
        }
        if !self.x87.active || self.x87.end != self.index {
            return;
        }
        if self.trace.insts[self.index].next == Next::Fall {
            self.x87_suspend();
            let label = self.asm.label();
            self.asm.b(label);
            self.push_cold(UnitCold::X87End {
                label,
                state: self.state,
                vector: self.vector,
                leave: self.x87_leaving().expect("a suspended run"),
                done: self.x87.done.expect("an open run"),
            });
        }
        self.x87_end();
        self.vector.cpu = false;
    }

    fn x87_end(&mut self) {
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

    /// A suspended site holds guest flags in NZCV, and status and TOP in
    /// v7 for a cyclic run or in `CpuState` otherwise. The recipe
    /// temporaries in x7 and x8 must survive publication.
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
        if suspended && model.cyclic {
            self.emit(ldr(X, X5, RUNTIME, runtime::CPU_STATE));
            self.x87_unspill_status();
        } else if suspended {
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
        let cold = match cold {
            Cold::Instruction(cold) => cold,
            Cold::Tags {
                label,
                done,
                fail,
                value,
            } => {
                self.asm.bind(label);
                // Merges can make a read entry value dirty. A zero tag then
                // requires a zero payload, so publication preserves its tag.
                let mask = (0..8)
                    .filter(|r| value & (1 << r) != 0)
                    .fold(0, |mask, r| mask | 2 << (2 * r));
                self.mov_imm(X17, mask);
                self.emit(and(W, X17, X17, X16, Lsl, 0));
                self.asm.cbz(W, true, X17, fail);
                self.emit(mov(W, X2, X16));
                for r in (0..8).filter(|r| value & (1 << r) != 0) {
                    let normal = self.asm.label();
                    self.asm.tbz(false, X2, 2 * r as u32, normal);
                    self.x87_slot(r);
                    self.emit(ldr(X, X0, X17, 0));
                    self.asm.cbz(X, true, X0, fail);
                    self.emit_all(&[ldrh(X0, X17, 8), and_imm(W, X0, X0, 0x7fff)]);
                    self.asm.cbz(W, true, X0, fail);
                    self.asm.bind(normal);
                }
                self.emit(mov(W, X16, X2));
                self.asm.b(done);
                return;
            }
            Cold::Entry {
                index,
                model,
                need,
                label,
                hot,
                run,
            } => {
                self.asm.bind(label);
                self.index = index;
                self.state = State::CANONICAL;
                self.vector = super::super::simd::Vector::at(self.entry[index]);
                self.mark(self.trace.insts[index].pc, Part::X87Entry);
                self.emit_all(&[mrs_nzcv(X4), ldr(X, X5, RUNTIME, runtime::CPU_STATE)]);
                let fail = self.asm.label();
                self.push_cold(UnitCold::Leave {
                    label: fail,
                    state: State::CANONICAL,
                    simd_dirty: self.vector.dirty,
                    refund: self.trace.owed(index),
                    exit: Exit::Dispatch,
                    eip: self.trace.insts[index].pc,
                    link: false,
                    x87: Some(Leave::Restore(None)),
                });
                let end = std::mem::replace(&mut self.x87.end, *run.end());
                self.x87_checks(model, need, fail);
                self.x87.end = end;
                if model.cyclic && self.stable_control(*run.start(), *run.end()) {
                    self.x87_require_bit(X8, 16, fail);
                }
                for (r, slot) in model.slots.iter().enumerate() {
                    if let Slot::Dirty(n) | Slot::Freed(Some(n)) = *slot {
                        self.x87_fetch(r, n, false, fail);
                    }
                }
                if model.cyclic {
                    self.emit_all(&[
                        ldr(W, X16, X5, runtime::X87_IP),
                        vins_general(2, RUN, 2, X16),
                        ldrh(X16, X5, runtime::X87_OPCODE),
                        vins_general(2, RUN, 3, X16),
                        bfi(W, X13, X15, 11, 3),
                        vins_general(2, RUN, 0, X13),
                        vins_general(2, RUN, 1, X8),
                    ]);
                }
                self.emit(msr_nzcv(X4));
                self.asm.b(hot);
                return;
            }
        };
        if let Some(generic) = cold.generic {
            self.remark(Part::X87Wide);
            self.pair_generic(generic, cold.bail);
        }
        let leaf = match self.trace.insts[cold.index].op {
            Op::X87(
                x @ X87 {
                    fast: Fast::Leaf(m),
                    ..
                },
            ) => Some((m, x)),
            _ => None,
        };
        let fallback = leaf.map(|_| self.asm.label());
        self.asm.bind(cold.bail);
        if let Some(fallback) = fallback {
            // The leaf published its input before calling; its pair registers are dead.
            self.asm.b(fallback);
        } else {
            self.remark(Part::X87Publish);
            self.x87_publish(cold.before, if cold.saved { X14 } else { X13 });
            if cold.dirty != 0 {
                self.vector.dirty = cold.dirty;
                self.emit(msr_nzcv(X4));
                self.flush_simd();
                self.emit(mrs_nzcv(X4));
            }
        }
        self.asm.bind(cold.help);
        self.remark(Part::Cold);
        self.index = cold.index;
        self.state = State::CANONICAL;
        self.vector = super::super::simd::Vector::default();
        if let Some((m, x)) = leaf {
            let fallback = fallback.unwrap();
            let resume = self.asm.label();
            // A preceding helper may have left values outside the pair format.
            // The arithmetic leaf reads their canonical raw80 image directly.
            self.x87_checks(Model::default(), Need::default(), fallback);
            self.x87_leaf_call(m, x, fallback);
            self.emit(msr_nzcv(X4));
            self.asm.b(resume);
            self.asm.bind(fallback);
            self.emit(msr_nzcv(X4));
            self.helper(cold.helper);
            self.asm.bind(resume);
        } else {
            self.emit(msr_nzcv(X4));
            self.helper(cold.helper);
        }
        if cold.dispatch {
            let inst = self.trace.insts[cold.index];
            self.leave(
                Exit::Dispatch,
                inst.next_pc,
                self.trace.unused(cold.index),
                false,
                false,
            );
            return;
        }
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
