//! Translation units (A64-TR-3).
//!
//! `trace` picks the guest instructions of a unit and analyses them; this
//! module emits them in trace order, with side exits, edge trampolines and
//! refusals after the body, and `ops` emits each instruction.
//!
//! Flags follow A64-FLAG-1 to A64-FLAG-5. SF ZF OF live in NZCV. CF lives in NZCV's C in a
//! tracked carry sense, or lazily as a `Carry` over values still in
//! registers; while C holds it, a `derivable` expression may still compute
//! it, so an `inc` or `dec` that keeps CF can let C go. PF and AF stay a
//! recipe over such values until something would overwrite them. Flags no
//! path reads before writing them again are not computed. Every exit makes
//! the flags canonical, an internal edge only those its target reads, and
//! every fault site records sense, carry and recipe for the owner.
//!
//! Memory accesses go straight to the window, where a host fault at a
//! site reaches the owner (A64-MEM-5, A64-MEM-6). Instructions the owner has seen fault
//! too often are checked instead: each access first reads its page's
//! class, and one that stays in a page the window may take directly does;
//! the rest call the `access` stub, so soft pages cost a helper call
//! rather than a fault.
//!
//! An instruction the translator does not handle but the interpreter runs
//! without transferring control calls the `interpret` stub with canonical
//! flags, and the unit goes on unless the helper asks it to leave.
//!
//! Host registers beyond the guest map of A64-REG-1: x16 and x17 address and exit
//! scratch, x6 a value scratch, x7 and x8 the recipe temporaries, which the
//! exit stub saves for fault recovery, x13 and x14 flag scratch without
//! FlagM, and x15 the lazy-carry scratch. Checked accesses also use x3 to
//! x5, which are free inside units; a run of SIMD instructions keeps the
//! `CpuState` pointer in x5, MMX registers in d8-d15 and XMM registers in
//! v16-v23, with v0-v2 as scratch.

mod decode;
pub mod first;
mod flags;
mod integer;
mod ops;
mod simd;
mod strings;
#[cfg(all(test, emulator))]
mod tests;
mod trace;
mod x87;

use iced_x86::ConditionCode;

pub use decode::Helper;
pub use flags::{Carry, Operand, Recipe, Sense};
pub use trace::MAX_INSTRUCTIONS;

use adv_cpu_core::IndirectBranchKind;

use crate::asm::{Asm, Label};
use crate::encode::Extend::Uxtw;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::Exit;
use decode::{EBP, ESP};
use flags::FlagSet;
use trace::{Edge, Next, Trace};

/// Host features that change what the translator emits (A64-ISA-4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub flagm: bool,
}

/// What the owner needs at a host instruction that may fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Site {
    /// The guest instruction, which has committed nothing (A64-FLAG-4).
    pub eip: u32,
    /// Instructions the countdown prepaid that the unit has not completed,
    /// this one included.
    pub refund: u32,
    pub sense: Sense,
    pub carry: Carry,
    pub recipe: Recipe,
    /// Whether the access checks its page first, so a fault here is not
    /// a reason to check it.
    pub checked: bool,
}

/// The last unit translated, in buffers the next translation reuses.
#[derive(Default)]
pub struct Workspace {
    pub(crate) trace: Trace,
    asm: Asm,
    labels: Vec<Label>,
    sites: Vec<(usize, Site)>,
    cold: Vec<Cold>,
    far: Vec<(usize, usize)>,
    links: Vec<Link>,
    retired: u32,
}

/// A link point of a unit: a branch to the owner's helper that may become
/// a direct branch to the unit at `eip`.
#[derive(Clone, Copy, Debug)]
pub struct Link {
    /// Byte offset of the point in the unit.
    pub offset: usize,
    pub eip: u32,
    /// Whether the trace's main path leaves through it: a real call, or
    /// the end of the trace, rather than a side exit.
    pub main: bool,
}

impl Workspace {
    pub fn words(&self) -> &[u32] {
        self.asm.words()
    }

    /// Byte offset of each site in `words`.
    pub fn sites(&self) -> &[(usize, Site)] {
        &self.sites
    }

    /// Instructions the entry prepays: the unit runs only with that much
    /// countdown.
    pub fn retired(&self) -> u32 {
        self.retired
    }

    /// Guest bytes the unit was translated from, as sorted disjoint
    /// (address, length) spans.
    pub fn code(&self) -> &[(u32, u32)] {
        &self.trace.code
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    /// What the unit's `interpret` calls run, from the base index
    /// `translate` was given on.
    pub fn helpers(&mut self) -> std::vec::Drain<'_, Helper> {
        self.trace.helpers.drain(..)
    }
}

/// Host addresses of the `runtime` stubs units branch to.
#[derive(Clone, Copy, Debug)]
pub struct Stubs {
    pub leaf: usize,
    pub exit: usize,
    pub dispatch: usize,
    /// Continues at the guest EIP in w16 through the indirect table.
    pub lookup: usize,
    /// `lookup` after its probe missed.
    pub lookup_slow: usize,
    /// A return whose shadow entry did not match.
    pub ret_miss: usize,
    /// A checked access the window cannot take.
    pub access: usize,
    /// An instruction the interpreter runs.
    pub interpret: usize,
}

const X0: Reg = Reg::new(0);
const X1: Reg = Reg::new(1);
const X2: Reg = Reg::new(2);
const X3: Reg = Reg::new(3);
const X4: Reg = Reg::new(4);
const X5: Reg = Reg::new(5);
const X6: Reg = Reg::new(6);
const X9: Reg = Reg::new(9);
const X10: Reg = Reg::new(10);
const X11: Reg = Reg::new(11);
const X12: Reg = Reg::new(12);
const X13: Reg = Reg::new(13);
const X14: Reg = Reg::new(14);
const X15: Reg = Reg::new(15);
const X16: Reg = Reg::new(16);
const X17: Reg = Reg::new(17);
const WINDOW: Reg = Reg::new(27);
const SHADOW: Reg = Reg::new(28);
const RUNTIME: Reg = Reg::new(29);

/// The host register of guest register `index`; ESP and EBP are biased.
const fn host(index: u8) -> Reg {
    Reg::new(19 + index)
}

const fn temp(index: u8) -> Reg {
    Reg::new(7 + index)
}

const fn biased(index: u8) -> bool {
    index == ESP || index == EBP
}

/// The flags as the code at hand holds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct State {
    /// How C encodes CF while `carry` is `Host`.
    sense: Sense,
    carry: Carry,
    /// CF as an expression over live values, while C also holds it.
    derivable: Option<Carry>,
    recipe: Recipe,
}

impl State {
    /// The block-exit form: also what a region head or merge point assumes.
    const CANONICAL: Self = Self {
        sense: Sense::Subtraction,
        carry: Carry::Host,
        derivable: None,
        recipe: Recipe::Canonical,
    };
}

/// What a conditional branch tests, once `Translator::test` has prepared
/// it.
#[derive(Clone, Copy)]
enum Test {
    Cond(Cond),
    /// PF set (true) or clear, from w16's bit 0.
    Parity(bool),
    EcxZero,
    /// CX, shifted into w16, is zero.
    CxZero,
    /// ECX, counted down, is not zero, and the condition holds if any.
    Count(Option<Cond>),
}

/// Code emitted after the unit body.
enum Cold {
    /// Leaves for a static EIP with canonical flags after refunding the
    /// countdown: through a link point, or through the exit stub.
    Leave {
        label: Label,
        state: State,
        refund: u32,
        exit: Exit,
        eip: u32,
        link: bool,
    },
    /// A forward internal edge that converts flags or settles the budget.
    Edge {
        label: Label,
        state: State,
        from: usize,
        to: usize,
    },
    /// A backward edge whose check failed: undo it, then leave if the
    /// branch is taken.
    Refuse {
        label: Label,
        net: u32,
        test: Test,
        leave: Label,
        resume: Label,
    },
    /// Continues at the EIP in w16 through the indirect table.
    Lookup {
        label: Label,
        state: State,
        refund: u32,
        branch: u64,
    },
    /// A miss of an indirect transfer: loads `branch` into x4, then
    /// branches to the stub at `to`, which may be out of `cbnz` range.
    Miss {
        label: Label,
        to: usize,
        branch: u64,
    },
    /// A real call's first dispatch: the owner patches the `bl` at `point`.
    Call {
        label: Label,
        target: u32,
        point: Label,
    },
    /// A checked access the window cannot take: the `access` stub does it
    /// for the site at `site`, then the unit resumes.
    Access {
        label: Label,
        resume: Label,
        site: Label,
        address: Reg,
        value: Reg,
        kind: u32,
    },
    /// The `interpret` helper asked to leave with the exit + 1 in w0, at
    /// the EIP in `CpuState`, after refunding the countdown.
    Helper { label: Label, refund: u32 },
}

struct Translator<'a> {
    asm: &'a mut Asm,
    features: Features,
    trace: &'a Trace,
    /// The position of each instruction, after any countdown check.
    labels: &'a mut Vec<Label>,
    sites: &'a mut Vec<(usize, Site)>,
    cold: &'a mut Vec<Cold>,
    /// Branches to stubs: word index and target address.
    far: &'a mut Vec<(usize, usize)>,
    links: &'a mut Vec<Link>,
    stubs: Stubs,
    /// Guest instructions whose accesses check their page first.
    checked: &'a dyn Fn(u32) -> bool,
    /// The index of the first helper of the unit.
    helpers: u32,
    /// Whether the instruction being emitted is one.
    checking: bool,
    state: State,
    /// The instruction being emitted.
    index: usize,
    /// Flags some path reads after it.
    out: FlagSet,
    /// Of those, the ones it does not write.
    keep: FlagSet,
    vector: simd::Vector,
}

/// Translates the unit at `eip` into `work`, to be placed at host address
/// `at`. Units end before any of `stops`, the accesses of instructions
/// `checked` accepts check their page first, and the unit's helpers are
/// numbered from `helpers`. Each translated byte is
/// registered with `watch_code`, so a store to it reaches the owner.
#[allow(clippy::too_many_arguments)]
pub fn translate(
    work: &mut Workspace,
    memory: &mut adv_cpu_core::memory::Memory,
    eip: u32,
    stops: &[u32],
    checked: &dyn Fn(u32) -> bool,
    helpers: u32,
    features: Features,
    at: usize,
    stubs: Stubs,
    leaf: &dyn Fn(u32, u32) -> bool,
) {
    work.trace.build(memory, eip, stops, leaf);
    for &(start, len) in &work.trace.code {
        memory.watch_code(start, len as usize);
    }
    let Workspace {
        trace,
        asm,
        labels,
        sites,
        cold,
        far,
        links,
        retired,
    } = work;
    asm.reset();
    labels.clear();
    labels.extend((0..trace.len()).map(|_| asm.label()));
    sites.clear();
    cold.clear();
    far.clear();
    links.clear();
    let mut t = Translator {
        asm,
        features,
        trace,
        labels,
        sites,
        cold,
        far,
        links,
        stubs,
        checked,
        helpers,
        checking: false,
        state: State::CANONICAL,
        index: 0,
        out: FlagSet::ALL,
        keep: FlagSet::ALL,
        vector: simd::Vector::default(),
    };
    if trace.len() == 0 {
        // Nothing to translate: other units may still link here.
        t.exit(Exit::Interpret, eip);
        *retired = 0;
    } else {
        for index in 0..trace.len() {
            t.instruction(index);
        }
        *retired = trace.prepaid(0);
    }
    t.finish(at);
}

impl Translator<'_> {
    fn emit(&mut self, word: u32) {
        self.asm.emit(word);
    }

    fn emit_all(&mut self, words: &[u32]) {
        self.asm.emit_all(words);
    }

    fn mov_imm(&mut self, rd: Reg, value: u32) {
        let (low, high) = (value as u16, (value >> 16) as u16);
        if high == 0 {
            self.emit(movz(W, rd, low, 0));
        } else if low == 0 {
            self.emit(movz(W, rd, high, 16));
        } else {
            self.emit(movz(W, rd, low, 0));
            self.emit(movk(W, rd, high, 16));
        }
    }

    /// A register holding `value`, using `scratch` unless it is zero.
    fn imm(&mut self, value: u32, scratch: Reg) -> Reg {
        if value == 0 {
            return Reg::ZR;
        }
        self.mov_imm(scratch, value);
        scratch
    }

    fn instruction(&mut self, index: usize) {
        let trace = self.trace;
        let inst = trace.insts[index];
        self.index = index;
        self.checking = (self.checked)(inst.pc);
        self.out = trace.live_out[index];
        self.keep = self.out.minus(inst.effects.writes);
        if trace.head(index) || trace.merge[index] {
            // Control may arrive from elsewhere.
            self.vector = simd::Vector::default();
        } else if !matches!(inst.op, decode::Op::Simd(_) | decode::Op::X87(_)) {
            // Other instructions may use x5.
            self.vector.cpu = false;
        }
        if trace.head(index) {
            // Arrivals are canonical: the entry, or a return.
            let prepaid = trace.prepaid(index);
            let refuse = self.asm.label();
            self.emit(sub_imm(X, X9, X9, prepaid, false));
            self.asm.tbz(true, X9, 63, refuse);
            self.cold.push(Cold::Leave {
                label: refuse,
                state: State::CANONICAL,
                refund: prepaid,
                exit: Exit::Dispatch,
                eip: inst.pc,
                link: false,
            });
            self.state = State::CANONICAL;
        } else if trace.merge[index] {
            self.convert(trace.live_in[index]);
            self.state = State::CANONICAL;
        }
        self.asm.bind(self.labels[index]);
        self.op(inst);
        if let Next::Edge(edge) = inst.next {
            self.follow(edge);
        }
    }

    /// Where an instruction's fall-through or jump ends the trace.
    fn follow(&mut self, edge: Edge) {
        let unused = self.trace.unused(self.index);
        match edge {
            Edge::Internal(to) => self.jump(to),
            Edge::Exit(eip) => self.leave(Exit::Dispatch, eip, unused, true, true),
            Edge::Interpret(eip) => self.leave(Exit::Interpret, eip, unused, false, false),
        }
    }

    /// An unconditional internal edge from the current instruction.
    fn jump(&mut self, to: usize) {
        self.convert(self.trace.live_in[to]);
        let net = self.trace.net(self.index, to);
        if net > 0 {
            let refuse = self.asm.label();
            self.emit(sub_imm(X, X9, X9, net as u32, false));
            self.asm.tbz(true, X9, 63, refuse);
            self.cold.push(Cold::Leave {
                label: refuse,
                state: self.state,
                refund: net as u32 + self.trace.unused(self.index),
                exit: Exit::Dispatch,
                eip: self.trace.insts[to].pc,
                link: false,
            });
        } else if net < 0 {
            self.emit(add_imm(X, X9, X9, (-net) as u32, false));
        }
        self.asm.b(self.labels[to]);
    }

    /// A jcc at the current instruction whose taken edge is `taken`.
    fn branch(&mut self, cc: decode::Cc, taken: Edge) {
        let from = self.index;
        match taken {
            Edge::Internal(to) if to <= from => {
                // A loop: convert on both paths, then check inline.
                self.convert(self.trace.live_in[to]);
                let test = self.test(cc);
                let net = self.trace.net(from, to);
                if net > 0 {
                    let net = net as u32;
                    let (refuse, leave, resume) =
                        (self.asm.label(), self.asm.label(), self.asm.label());
                    self.emit(sub_imm(X, X9, X9, net, false));
                    self.asm.tbz(true, X9, 63, refuse);
                    self.jump_if(test, self.labels[to]);
                    self.emit(add_imm(X, X9, X9, net, false));
                    self.asm.bind(resume);
                    self.cold.push(Cold::Refuse {
                        label: refuse,
                        net,
                        test,
                        leave,
                        resume,
                    });
                    self.cold.push(Cold::Leave {
                        label: leave,
                        state: self.state,
                        refund: self.trace.unused(from),
                        exit: Exit::Dispatch,
                        eip: self.trace.insts[to].pc,
                        link: false,
                    });
                } else if net == 0 {
                    self.jump_if(test, self.labels[to]);
                } else {
                    let label = self.asm.label();
                    self.jump_if(test, label);
                    self.cold.push(Cold::Edge {
                        label,
                        state: self.state,
                        from,
                        to,
                    });
                }
            }
            Edge::Internal(to) => {
                let test = self.test(cc);
                if self.trace.net(from, to) == 0 && !self.needs(self.trace.live_in[to]) {
                    self.jump_if(test, self.labels[to]);
                } else {
                    let label = self.asm.label();
                    self.jump_if(test, label);
                    self.cold.push(Cold::Edge {
                        label,
                        state: self.state,
                        from,
                        to,
                    });
                }
            }
            Edge::Exit(eip) => {
                let test = self.test(cc);
                let label = self.asm.label();
                self.jump_if(test, label);
                self.cold.push(Cold::Leave {
                    label,
                    state: self.state,
                    refund: self.trace.unused(from),
                    exit: Exit::Dispatch,
                    eip,
                    link: true,
                });
            }
            Edge::Interpret(_) => unreachable!("a jcc target dispatches"),
        }
    }

    /// Emits what `cc` needs before its test: the decrement of a `loop`,
    /// or PF's parity into w16.
    fn test(&mut self, cc: decode::Cc) -> Test {
        use decode::Cc;
        const ECX: u8 = 1;
        match cc {
            Cc::Flags(ConditionCode::p | ConditionCode::np) => {
                self.convert(FlagSet::PF);
                // Bit 0 folds the parity of x10's low byte: 1 when odd,
                // when PF is clear.
                self.emit_all(&[
                    eor(W, X16, X10, X10, Shift::Lsr, 4),
                    eor(W, X16, X16, X16, Shift::Lsr, 2),
                    eor(W, X16, X16, X16, Shift::Lsr, 1),
                ]);
                Test::Parity(cc == Cc::Flags(ConditionCode::p))
            }
            Cc::Flags(cc) => Test::Cond(self.condition(cc)),
            Cc::EcxZero => Test::EcxZero,
            Cc::CxZero => {
                self.emit(lsl_imm(W, X16, host(ECX), 16));
                Test::CxZero
            }
            Cc::Loop(zero) => {
                self.clobber(Operand::Guest(ECX));
                self.emit(sub_imm(W, host(ECX), host(ECX), 1, false));
                let zero = zero.map(|set| {
                    self.condition(if set {
                        ConditionCode::e
                    } else {
                        ConditionCode::ne
                    })
                });
                Test::Count(zero)
            }
        }
    }

    /// Branches to `label` when `test` holds; repeatable after it.
    fn jump_if(&mut self, test: Test, label: Label) {
        match test {
            Test::Cond(cond) => self.asm.b_cond(cond, label),
            Test::Parity(set) => self.asm.tbz(!set, X16, 0, label),
            Test::EcxZero => self.asm.cbz(W, false, host(1), label),
            Test::CxZero => self.asm.cbz(W, false, X16, label),
            Test::Count(None) => self.asm.cbz(W, true, host(1), label),
            Test::Count(Some(cond)) => {
                let skip = self.asm.label();
                self.asm.cbz(W, false, host(1), skip);
                self.asm.b_cond(cond, label);
                self.asm.bind(skip);
            }
        }
    }

    fn site(&mut self) {
        let inst = self.trace.insts[self.index];
        self.sites.push((
            self.asm.offset(),
            Site {
                eip: inst.pc,
                refund: self.trace.owed(self.index),
                sense: self.state.sense,
                carry: self.state.carry,
                recipe: self.state.recipe,
                checked: self.checking,
            },
        ));
    }

    /// Whether `convert(flags)` would emit anything.
    fn needs(&self, flags: FlagSet) -> bool {
        let state = self.state;
        flags.any(FlagSet::CF) && (state.carry != Carry::Host || state.sense != Sense::Subtraction)
            || flags.any(FlagSet::PF.or(FlagSet::AF)) && state.recipe != Recipe::Canonical
    }

    /// Puts `flags` in canonical form; the others keep their form.
    fn convert(&mut self, flags: FlagSet) {
        if flags.any(FlagSet::CF) {
            self.host_carry();
            self.set_sense(Sense::Subtraction);
        }
        if flags.any(FlagSet::PF.or(FlagSet::AF)) {
            self.materialize();
        }
    }

    fn canonicalize(&mut self) {
        self.convert(FlagSet::ALL);
    }

    /// Every written flag is dead: forget how the old ones were held.
    fn dead(&mut self) {
        self.state.carry = Carry::Host;
        self.state.derivable = None;
        self.state.recipe = Recipe::Canonical;
    }

    /// Called before an instruction writes `operand`: keeps what the flags
    /// still need from it. `forget` must follow the write.
    fn prepare(&mut self, operand: Operand) {
        if self.state.recipe.uses(operand) && self.keep.any(FlagSet::PF.or(FlagSet::AF)) {
            self.materialize();
        }
        if self.state.carry.uses(operand) && self.keep.any(FlagSet::CF) {
            self.host_carry();
        }
    }

    /// After `operand` changed: drops what still reads it, all dead.
    fn forget(&mut self, operand: Operand) {
        let state = &mut self.state;
        if state.recipe.uses(operand) {
            state.recipe = Recipe::Canonical;
        }
        if state.derivable.is_some_and(|carry| carry.uses(operand)) {
            state.derivable = None;
        }
        if state.carry.uses(operand) {
            state.carry = Carry::Host;
        }
    }

    /// `prepare` and `forget` around a write with no site between.
    fn clobber(&mut self, operand: Operand) {
        self.prepare(operand);
        self.forget(operand);
    }

    /// A recipe temporary neither the recipe nor a lazy carry reads.
    fn take_temp(&mut self) -> u8 {
        let free = |state: &State, t: u8| {
            !state.recipe.uses(Operand::Temp(t)) && !state.carry.uses(Operand::Temp(t))
        };
        if !free(&self.state, 0) && !free(&self.state, 1) {
            self.host_carry();
        }
        if free(&self.state, 0) { 0 } else { 1 }
    }

    /// Makes x10 and x11 canonical for the current recipe; uses w17.
    fn materialize(&mut self) {
        use Recipe::*;
        let recipe = std::mem::replace(&mut self.state.recipe, Canonical);
        let value = |t: &mut Self, operand: Operand| match operand {
            Operand::Guest(index) => host(index),
            Operand::Temp(index) => temp(index),
            Operand::Imm(value) => t.imm(value, X17),
        };
        match recipe {
            Canonical => {}
            AddResult(r, b) | SubResult(r, b) => {
                let adding = matches!(recipe, AddResult(..));
                let r = value(self, r);
                self.emit(mov(W, X10, r));
                match b {
                    // The left operand back, XOR the result; bit 4 of a small
                    // right operand is often clear.
                    Operand::Imm(imm) if imm < 4096 => {
                        self.emit(if adding {
                            sub_imm(W, X11, r, imm, false)
                        } else {
                            add_imm(W, X11, r, imm, false)
                        });
                        if imm & 0x10 != 0 {
                            self.emit(eor_imm(W, X11, X11, 0x10));
                        }
                    }
                    _ => {
                        let b = value(self, b);
                        self.emit(if adding {
                            sub(W, X11, r, b, Lsl, 0)
                        } else {
                            add(W, X11, r, b, Lsl, 0)
                        });
                        self.emit(eor(W, X11, X11, b, Lsl, 0));
                    }
                }
                self.emit(eor(W, X11, X11, r, Lsl, 0));
            }
            Add(a, b) | Sub(a, b) => {
                let (a, b) = (value(self, a), value(self, b));
                if matches!(recipe, Add(..)) {
                    self.emit(add(W, X10, a, b, Lsl, 0));
                } else {
                    self.emit(sub(W, X10, a, b, Lsl, 0));
                }
                self.emit(eor(W, X11, a, b, Lsl, 0));
                self.emit(eor(W, X11, X11, X10, Lsl, 0));
            }
            Logic(r) => {
                let r = value(self, r);
                self.emit_all(&[mov(W, X10, r), mov(W, X11, Reg::ZR)]);
            }
            And(a, b) | Or(a, b) | Xor(a, b) => {
                let (a, b) = (value(self, a), value(self, b));
                self.emit(match recipe {
                    And(..) => and(W, X10, a, b, Lsl, 0),
                    Or(..) => orr(W, X10, a, b, Lsl, 0),
                    _ => eor(W, X10, a, b, Lsl, 0),
                });
                self.emit(mov(W, X11, Reg::ZR));
            }
            Neg(r) => {
                let r = value(self, r);
                self.emit_all(&[
                    mov(W, X10, r),
                    sub(W, X11, Reg::ZR, r, Lsl, 0),
                    eor(W, X11, X11, r, Lsl, 0),
                ]);
            }
            Doubled(r) => {
                let r = value(self, r);
                self.emit_all(&[mov(W, X10, r), mov(W, X11, r)]);
            }
            Parity(r) => {
                let r = value(self, r);
                self.emit(mov(W, X10, r));
            }
            Mixed(r, t) => {
                let (r, t) = (value(self, r), value(self, t));
                self.emit_all(&[mov(W, X10, r), eor(W, X11, t, r, Lsl, 0)]);
            }
        }
    }

    /// A zero-extended X register holding `operand`; uses x15.
    fn wide(&mut self, operand: Operand) -> Reg {
        match operand {
            Operand::Guest(index) if biased(index) => {
                self.emit(mov(W, X15, host(index)));
                X15
            }
            Operand::Guest(index) => host(index),
            Operand::Temp(index) => temp(index),
            Operand::Imm(value) => {
                self.mov_imm(X15, value);
                X15
            }
        }
    }

    /// A register whose low half holds `operand`; uses w17.
    fn narrow(&mut self, operand: Operand) -> Reg {
        match operand {
            Operand::Guest(index) => host(index),
            Operand::Temp(index) => temp(index),
            Operand::Imm(value) => self.imm(value, X17),
        }
    }

    /// Moves a lazy CF into C, in addition sense; uses x15 and w17.
    fn host_carry(&mut self) {
        let carry = self.state.carry;
        let bit = match carry {
            Carry::Host => return,
            Carry::Bit(a, n) => {
                let a = self.narrow(a);
                (a, u32::from(n))
            }
            Carry::Below(a, b) => {
                // a - b in 64 bits is negative exactly when a < b.
                let (a, b) = (self.wide(a), self.narrow(b));
                self.emit(sub_ext(X, X15, a, b, Uxtw, 0));
                (X15, 63)
            }
            Carry::Sum(a, b) => {
                let (a, b) = (self.wide(a), self.narrow(b));
                self.emit(add_ext(X, X15, a, b, Uxtw, 0));
                (X15, 32)
            }
            Carry::Nonzero(a) => {
                let a = self.wide(a);
                self.emit(sub(X, X15, Reg::ZR, a, Lsl, 0));
                (X15, 63)
            }
            Carry::Clear => (Reg::ZR, 0),
        };
        self.insert_flags(Some(bit), None);
        self.state = State {
            sense: Sense::Addition,
            carry: Carry::Host,
            derivable: Some(carry),
            recipe: self.state.recipe,
        };
    }

    /// Flips C to `sense`; C must hold CF. Uses x17.
    fn set_sense(&mut self, sense: Sense) {
        if self.state.sense == sense {
            return;
        }
        if self.features.flagm {
            self.emit(CFINV);
        } else {
            self.emit_all(&[mrs_nzcv(X17), eor_imm(X, X17, X17, 1 << 29), msr_nzcv(X17)]);
        }
        self.state.sense = sense;
    }

    /// Sets C to bit `carry.1` of `carry.0` and V to bit `overflow.1` of
    /// `overflow.0`, each if given, keeping the other flags; uses x13 and
    /// x14 without FlagM.
    fn insert_flags(&mut self, carry: Option<(Reg, u32)>, overflow: Option<(Reg, u32)>) {
        if self.features.flagm {
            if let Some((reg, bit)) = carry {
                self.emit(rmif(reg, (bit + 63) % 64, 0b0010));
            }
            if let Some((reg, bit)) = overflow {
                self.emit(rmif(reg, bit, 0b0001));
            }
            return;
        }
        self.emit(mrs_nzcv(X13));
        for ((reg, bit), at) in [(carry, 29), (overflow, 28)]
            .into_iter()
            .filter_map(|(flag, at)| Some((flag?, at)))
        {
            self.emit_all(&[ubfx(X, X14, reg, bit, 1), bfi(X, X13, X14, at, 1)]);
        }
        self.emit(msr_nzcv(X13));
    }

    /// The A64 condition for `cc`, moving CF to C where `cc` reads it and
    /// flipping to subtraction sense where no single condition reads it.
    fn condition(&mut self, cc: ConditionCode) -> Cond {
        use ConditionCode as C;
        if FlagSet::of(cc).any(FlagSet::CF) {
            self.host_carry();
        }
        let addition = self.state.sense == Sense::Addition;
        match cc {
            C::o => Cond::Vs,
            C::no => Cond::Vc,
            C::e => Cond::Eq,
            C::ne => Cond::Ne,
            C::s => Cond::Mi,
            C::ns => Cond::Pl,
            C::l => Cond::Lt,
            C::ge => Cond::Ge,
            C::le => Cond::Le,
            C::g => Cond::Gt,
            C::b if addition => Cond::Hs,
            C::b => Cond::Lo,
            C::ae if addition => Cond::Lo,
            C::ae => Cond::Hs,
            C::be | C::a => {
                self.set_sense(Sense::Subtraction);
                if cc == C::be { Cond::Ls } else { Cond::Hi }
            }
            C::p | C::np | C::None => unreachable!("tested by `test`, or filtered by `supported`"),
        }
    }

    /// Canonical flags, the refund for this path, then the exit: through a
    /// link point if `link`, on the main path if `main`, else to the owner.
    fn leave(&mut self, exit: Exit, eip: u32, refund: u32, link: bool, main: bool) {
        self.canonicalize();
        if refund != 0 {
            self.emit(add_imm(X, X9, X9, refund, false));
        }
        if link {
            self.dispatch(eip, main);
        } else {
            self.exit(exit, eip);
        }
    }

    /// Leaves translated code: w0 the exit, w1 the EIP.
    fn exit(&mut self, exit: Exit, eip: u32) {
        self.emit(movz(W, Reg::new(0), exit as u16, 0));
        self.target(eip);
        self.far(self.stubs.exit);
    }

    /// Continues at `eip` through the owner's helper: w1 the EIP and x2
    /// the link point, the first word, which the helper may patch into a
    /// branch to the target unit.
    fn dispatch(&mut self, eip: u32, main: bool) {
        let point = self.asm.offset();
        self.links.push(Link {
            offset: point,
            eip,
            main,
        });
        self.target(eip);
        let here = self.asm.offset();
        self.emit(adr(Reg::new(2), point as i32 - here as i32));
        self.far(self.stubs.dispatch);
    }

    fn target(&mut self, eip: u32) {
        self.emit(movz(W, Reg::new(1), eip as u16, 0));
        self.emit(movk(W, Reg::new(1), (eip >> 16) as u16, 16));
    }

    /// A branch to the stub at host address `to`, placed by `finish`.
    fn far(&mut self, to: usize) {
        self.far.push((self.asm.offset() / 4, to));
        self.emit(b(0));
    }

    /// A call to the stub at host address `to`, placed by `finish`.
    fn far_call(&mut self, to: usize) {
        self.far.push((self.asm.offset() / 4, to));
        self.emit(bl(0));
    }

    /// A `cbnz` to the miss stub at `to` for an indirect transfer of
    /// `kind` from this instruction, through cold code that loads x4.
    fn miss_cbnz(&mut self, rt: Reg, to: usize, kind: IndirectBranchKind) {
        let label = self.asm.label();
        self.asm.cbz(Size::X, true, rt, label);
        let branch = self.indirect(kind);
        self.cold.push(Cold::Miss { label, to, branch });
    }

    /// `Runtime::branch` for an indirect transfer of `kind` from here.
    fn indirect(&self, kind: IndirectBranchKind) -> u64 {
        u64::from(self.trace.insts[self.index].pc) | (kind as u64) << 32
    }

    /// x4 = `branch`.
    fn mov_branch(&mut self, branch: u64) {
        self.mov_imm(X4, branch as u32);
        self.emit(movk(X, X4, (branch >> 32) as u16, 32));
    }

    fn cold(&mut self, cold: Cold) {
        match cold {
            Cold::Leave {
                label,
                state,
                refund,
                exit,
                eip,
                link,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.leave(exit, eip, refund, link, false);
            }
            Cold::Edge {
                label,
                state,
                from,
                to,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.index = from;
                self.jump(to);
            }
            Cold::Refuse {
                label,
                net,
                test,
                leave,
                resume,
            } => {
                self.asm.bind(label);
                self.emit(add_imm(X, X9, X9, net, false));
                self.jump_if(test, leave);
                self.asm.b(resume);
            }
            Cold::Lookup {
                label,
                state,
                refund,
                branch,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.canonicalize();
                if refund != 0 {
                    self.emit(add_imm(X, X9, X9, refund, false));
                }
                self.mov_branch(branch);
                self.far(self.stubs.lookup);
            }
            Cold::Miss { label, to, branch } => {
                self.asm.bind(label);
                self.mov_branch(branch);
                self.far(to);
            }
            Cold::Call {
                label,
                target,
                point,
            } => {
                self.asm.bind(label);
                self.links.push(Link {
                    offset: self.asm.position(point),
                    eip: target,
                    main: true,
                });
                self.target(target);
                self.asm.adr(Reg::new(2), point);
                self.far(self.stubs.dispatch);
            }
            Cold::Access {
                label,
                resume,
                site,
                address,
                value,
                kind,
            } => {
                self.asm.bind(label);
                self.emit(mov(W, X1, address));
                self.asm.adr(X2, site);
                let store = kind & crate::runtime::ACCESS_STORE != 0;
                if store {
                    self.emit(mov(W, X3, value));
                }
                self.emit(movz(W, X4, kind as u16, 0));
                self.far_call(self.stubs.access);
                if !store {
                    self.emit(mov(W, value, X0));
                }
                self.asm.b(resume);
            }
            Cold::Helper { label, refund } => {
                self.asm.bind(label);
                if refund != 0 {
                    self.emit(add_imm(X, X9, X9, refund, false));
                }
                self.emit_all(&[
                    sub_imm(W, X0, X0, 1, false),
                    ldr(X, X17, RUNTIME, crate::runtime::CPU_STATE),
                    ldr(W, X1, X17, crate::runtime::EIP),
                ]);
                self.far(self.stubs.exit);
            }
        }
    }

    fn finish(mut self, at: usize) {
        while let Some(cold) = self.cold.pop() {
            self.cold(cold);
        }
        self.asm.resolve();
        let words = self.asm.words_mut();
        for &(index, to) in self.far.iter() {
            let offset = (to as i64 - (at + 4 * index) as i64) as i32;
            words[index] = if words[index] == bl(0) {
                bl(offset)
            } else {
                b(offset)
            };
        }
    }
}
