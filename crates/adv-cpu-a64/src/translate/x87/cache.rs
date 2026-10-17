//! x87 runs whose stack lives in host registers (A64F-FP-7).
//!
//! A run is a stretch of consecutive x87 instructions of one block, none of
//! them a merge or head after the first. Its prologue checks, once, what
//! each instruction would check: no pending exception, no MMX state, and
//! the tags of the slots the run reads before writing (not empty) or
//! pushes into (empty). A value it loads must also be a zero or a normal,
//! which the tags need not tell. TOP at entry stays in w15; the run tracks TOP,
//! tags and the place of every value at translation time, keeping values
//! in v24-v31 as an extended encoding (significand in the low 64 bits,
//! sign and exponent in halfword 4). Copies, exchanges and stores between
//! slots only rename. w13 holds the status word without TOP; IP and opcode
//! are those of the last instruction that records them.
//!
//! The run publishes at its end, and before anything that observes
//! `CpuState`: an instruction whose fast path does not apply publishes the
//! state before it, has its interpreter helper run it and enters the rest
//! of the run again through the same checks, reloading every value and
//! requiring the registers a rename shares to still match, or runs every later
//! instruction through its helper when they fail; a fault at a site
//! resumes in a stub that publishes the state before the instruction and
//! then leaves as the fault stub does. Instructions that may change w13
//! before such an exit keep the status word before them in w14.

use adv_cpu_core::floating::fast::{Fast, Source};

use crate::asm::Label;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime;

use super::super::decode::Op;
use super::super::{
    Cold as UnitCold, RUNTIME, State, Translator, X0, X1, X2, X13, X14, X15, X16, X17,
};
use super::{X4, X5, X6, X8, X87};

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
}

impl Default for Model {
    fn default() -> Self {
        Self {
            slots: [Slot::Entry; 8],
            delta: 0,
            ip: None,
            opcode: None,
        }
    }
}

impl Model {
    fn at(&self, i: u8) -> usize {
        usize::from((self.delta + i) & 7)
    }

    fn get(&self, i: u8) -> Slot {
        self.slots[self.at(i)]
    }

    fn set(&mut self, i: u8, slot: Slot) {
        let at = self.at(i);
        self.slots[at] = slot;
    }

    fn reg(&self, i: u8) -> u8 {
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

    fn push(&mut self) {
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
        Fast::Negate | Fast::Absolute | Fast::Round => {
            list.extend([Read(0), Write(0)]);
            fresh = true;
        }
        Fast::Scale => {
            list.extend([Read(0), Read(1), Write(0)]);
            fresh = true;
        }
        Fast::Examine => unreachable!("not cached"),
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
}

/// The run being emitted.
#[derive(Default)]
pub(in super::super) struct Cache {
    pub(in super::super) active: bool,
    end: usize,
    model: Model,
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
}

impl Translator<'_> {
    /// The cacheable x87 instruction at `index`, if it is one.
    fn cacheable(&self, index: usize) -> Option<X87> {
        let inst = self.trace.insts[index];
        match inst.op {
            Op::X87(x)
                if x.fast != Fast::Examine && !(x.mem.is_some() && (self.checked)(inst.pc)) =>
            {
                Some(x)
            }
            _ => None,
        }
    }

    /// The last instruction of a run from `start`, and what it needs;
    /// `None` when not even the first fits.
    fn plan(&self, start: usize, from: Model) -> Option<(usize, Need)> {
        let mut known = from.slots.map(|slot| match slot {
            Slot::Entry => Known::Unknown,
            Slot::Clean(_) | Slot::Dirty(_) => Known::Value,
            Slot::Freed(Some(_)) => Known::Stale,
            Slot::Freed(None) => Known::Empty,
        });
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
        for index in start..self.trace.len() {
            if index != start && (self.trace.head(index) || self.trace.merge[index]) {
                break;
            }
            let Some(x) = self.cacheable(index) else {
                break;
            };
            if step(&mut known, &mut delta, &mut need, x.fast).is_none() {
                break;
            }
            end = Some(index);
            if self.trace.insts[index].next != super::super::trace::Next::Fall {
                break;
            }
        }
        end.map(|end| (end, need))
    }

    /// Emits instruction `x` in a run, opening one if none is; false when
    /// it is not cacheable here.
    pub(in super::super) fn x87_cached(&mut self, x: X87) -> bool {
        if !self.x87.active && !self.x87_open() {
            return false;
        }
        let index = self.index;
        let offset = index - self.x87.start;
        self.asm.bind(self.x87.hot[offset]);
        let mut model = self.x87.model;
        self.x87.before = model;
        self.x87.reserved = model.used();
        self.x87.saved = matches!(
            x.fast,
            Fast::Arith { .. } | Fast::Store { .. } | Fast::Round | Fast::Scale
        );
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
        if matches!(
            x.fast,
            Fast::Arith { .. } | Fast::Store { .. } | Fast::Round | Fast::StoreControl
        ) {
            self.emit(ldrh(X8, X5, runtime::X87_CONTROL));
        }
        let (dp, metadata) = self.x87_body(x, bail);
        if dp {
            self.emit(str(W, X6, X5, runtime::X87_DP));
        }
        model = self.x87.model;
        if metadata {
            model.ip = Some(self.trace.insts[index].pc);
            if x.opcode.is_some() {
                model.opcode = x.opcode;
            }
        }
        self.x87.model = model;
        let done = self.x87.done.expect("an open run");
        let next = (index < self.x87.end).then(|| {
            let (_, need) = self.plan(index + 1, model).expect("a planned run");
            (self.x87.hot[offset + 1], self.x87.help[offset + 1], need)
        });
        self.cold.push(UnitCold::X87(Cold {
            index,
            helper: x.helper,
            before: self.x87.before,
            saved: self.x87.saved,
            after: model,
            bail,
            help: self.x87.help[offset],
            next,
            done,
        }));
        if index == self.x87.end {
            self.x87_close();
        }
        true
    }

    /// The prologue of a run from the current instruction.
    fn x87_open(&mut self) -> bool {
        if self.pixel.is_some() || self.cacheable(self.index).is_none() {
            return false;
        }
        let start = self.index;
        let Some((end, need)) = self.plan(start, Model::default()) else {
            return false;
        };
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
        self.emit(mrs_nzcv(X4));
        let help = self.x87.help[0];
        self.x87_checks(Model::default(), need, help);
        true
    }

    /// Loads w13 and w15 for entering a run in state `model`, branching to
    /// `fail` unless the run may go on.
    fn x87_checks(&mut self, model: Model, need: Need, fail: Label) {
        self.emit_all(&[
            ldrh(X13, X5, runtime::X87_STATUS),
            ldr(W, X16, X5, runtime::MMX_MODE),
        ]);
        self.asm.tbz(true, X13, 7, fail);
        self.asm.cbz(W, true, X16, fail);
        self.emit(ubfx(W, X15, X13, 11, 3));
        if model.delta != 0 {
            self.emit_all(&[
                sub_imm(W, X15, X15, u32::from(model.delta), false),
                and_imm(W, X15, X15, 7),
            ]);
        }
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
    fn x87_alloc(&self) -> u8 {
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
        self.x87_fetch(r, n, fail);
        self.x87.model.set(i, Slot::Clean(n));
    }

    /// Loads physical register `r` into host register `n`, branching to
    /// `fail` unless it is a zero or a normal: a run holds only those, as
    /// the tags may not tell; uses x16 and x17.
    fn x87_fetch(&mut self, r: usize, n: u8, fail: Label) {
        self.x87_slot(r);
        self.emit_all(&[
            vldr(VMem::D, v(n), X17, 0),
            ldrh(X16, X17, 8),
            vins_general(1, v(n), 4, X16),
            and_imm(W, X17, X16, 0x7fff),
            fmov_from_v(X, X16, v(n)),
        ]);
        let (zero, ok) = (self.asm.label(), self.asm.label());
        self.asm.tbz(false, X16, 63, zero);
        self.asm.cbz(W, false, X17, fail);
        self.emit(add_imm(W, X17, X17, 1, false));
        self.asm.tbz(true, X17, 15, fail);
        self.asm.b(ok);
        self.asm.bind(zero);
        self.emit(orr(X, X16, X16, X17, Lsl, 0));
        self.asm.cbz(X, true, X16, fail);
        self.asm.bind(ok);
    }

    /// Branches to `fail` unless physical register `r` holds what host
    /// register `n` does; uses x0, x1, x16 and x17.
    fn x87_match(&mut self, r: usize, n: u8, fail: Label) {
        self.x87_slot(r);
        self.emit_all(&[
            ldr(X, X0, X17, 0),
            ldrh(X1, X17, 8),
            fmov_from_v(X, X16, v(n)),
            vumov(1, X17, v(n), 4),
            cmp(X, X0, X16, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Ne, fail);
        self.emit(cmp(W, X1, X17, Lsl, 0));
        self.asm.b_cond(Cond::Ne, fail);
    }

    /// ST(i) in `sig` and `sexp`.
    pub(super) fn x87_get(&mut self, i: u8, sig: Reg, sexp: Reg) {
        let n = self.x87.model.reg(i);
        self.emit_all(&[fmov_from_v(X, sig, v(n)), vumov(1, sexp, v(n), 4)]);
    }

    /// ST(i) = `sig` and `sexp`, in a new host register.
    pub(super) fn x87_put(&mut self, i: u8, sig: Reg, sexp: Reg) {
        let n = self.x87_alloc();
        self.emit_all(&[fmov_to_v(X, v(n), sig), vins_general(1, v(n), 4, sexp)]);
        self.x87.model.set(i, Slot::Dirty(n));
    }

    /// Pushes `sig` and `sexp`.
    pub(super) fn x87_push(&mut self, sig: Reg, sexp: Reg) {
        self.x87.model.push();
        self.x87_put(0, sig, sexp);
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
    /// to x2, x16 and x17.
    fn x87_publish(&mut self, model: Model, status: Reg) {
        for (r, slot) in model.slots.iter().enumerate() {
            if let Slot::Dirty(n) | Slot::Freed(Some(n)) = *slot {
                self.x87_slot(r);
                self.emit_all(&[
                    vstr(VMem::D, v(n), X17, 0),
                    vumov(1, X0, v(n), 4),
                    strh(X0, X17, 8),
                    add(X, X17, X5, X16, Lsl, 3),
                    vstr(VMem::D, v(n), X17, runtime::MMX),
                ]);
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

    /// Ends the run after its last instruction.
    fn x87_close(&mut self) {
        let model = self.x87.model;
        self.x87_publish(model, X13);
        self.emit(msr_nzcv(X4));
        let done = self.x87.done.take().expect("an open run");
        self.asm.bind(done);
        self.x87.active = false;
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
        self.cold.push(UnitCold::X87Fault {
            label,
            model: self.x87.before,
            saved: self.x87.saved,
        });
    }

    pub(in super::super) fn x87_fault(&mut self, label: Label, model: Model, saved: bool) {
        self.asm.bind(label);
        self.x87_publish(model, if saved { X14 } else { X13 });
        self.emit(msr_nzcv(X4));
        self.far(self.stubs.fault);
    }

    pub(in super::super) fn x87_cold(&mut self, cold: Cold) {
        self.asm.bind(cold.bail);
        self.x87_publish(cold.before, if cold.saved { X14 } else { X13 });
        self.asm.bind(cold.help);
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
        // run holds comes again from CpuState, live values first, and a
        // shared one must match in every register.
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
                } else if live {
                    self.x87_fetch(r, n, help);
                } else {
                    self.x87_slot(r);
                    self.emit_all(&[
                        vldr(VMem::D, v(n), X17, 0),
                        ldrh(X16, X17, 8),
                        vins_general(1, v(n), 4, X16),
                    ]);
                }
                loaded |= 1 << n;
            }
        }
        self.asm.b(hot);
    }
}
