//! Tier-1 lowering (A64A-ROLE-4, A64-TR-3).
//!
//! The front end builds a [`Unit`]: `trace` picks the guest instructions
//! and analyses them, and `tables` finds the switch tables they index. A
//! [`Jit`] lowers the front end's unit (A64A-ROLE-2). It emits the
//! instructions in trace order, with side exits, edge trampolines and
//! refusals after the body, and `ops` emits each instruction.
//!
//! Flags follow A64-FLAG-1 to A64-FLAG-5. SF ZF OF live in NZCV or recipes.
//! CF lives in NZCV's C in a
//! tracked carry sense, or lazily as a `Carry` over values still in
//! registers; while C holds it, a `derivable` expression may still compute
//! it, so an `inc` or `dec` that keeps CF can let C go. PF and AF stay a
//! recipe over such values until something would overwrite them. Flags no
//! path reads before writing them again are not computed. Every exit makes
//! the flags canonical, an internal edge only those its target reads, and
//! every fault site records sense, carry, flag recipes and the SIMD dirty mask for
//! the owner. SIMD values are published before observers and at edges
//! into merges that do not keep them dirty (A64F-FP-4, A64-REG-5).
//!
//! Memory accesses go straight to the window, where a host fault at a
//! site reaches the owner (A64-MEM-5, A64-MEM-6). Instructions the owner has seen fault
//! too often are checked instead: each access first reads its page's
//! class, and one that stays in a page the window may take directly does;
//! the rest call the `access` stub, so soft pages cost a helper call
//! rather than a fault. Integer and SIMD code reach either form through one
//! emitter (`access`), and a refused access faults like a host fault, so
//! dirty SIMD registers stay in place.
//!
//! Value cases pass prepared operands to core semantics. Architectural
//! events return to the execution owner at their precise guest boundary.
//!
//! Beyond the guest map of A64-REG-1, x16 and x17 hold address and exit
//! scratch, x6 holds value scratch, and x7 and x8 hold flag recipe
//! temporaries captured for fault recovery. Without FlagM, x13 and x14
//! are flag scratch and x15 is lazy-carry scratch. Checked accesses also
//! use x3 to x5. The SIMD cache keeps the `CpuState` pointer in x5, MMX
//! values in d8-d15 and XMM values in v16-v23, with v0-v6 as scratch.
//! An x87 run keeps its stack values in v24-v31.

mod body;
mod channels;
mod flags;
mod integer;
mod memory;
mod native_region;
mod ops;
mod parts;
mod residency;
mod simd;
mod strings;
mod tables;
#[cfg(all(test, emulator))]
pub(crate) mod tests;
mod x87;

#[cfg(native)]
use crate::translate::front::forms::Division;
use crate::translate::front::forms::{SimdKind, biased};
use crate::translate::front::tags::ConditionCode;
use crate::translate::front::{self, Unit, decode, trace};
use crate::translate::lower::access;
#[cfg(native)]
use crate::translate::lower::access::FrameCache;
use crate::translate::lower::flags::{Carry, Operand, Overflow, Recipe, Sense};
use crate::translate::lower::stubs;
use crate::translate::lower::{
    InternalLink, Link, RUNTIME, Site, Stubs, VersionLayout, WINDOW, X0, X1, X2, X3, X4, X5, X9,
    X12, X16, emit_back_count, emit_exit, emit_far, emit_link_point, emit_link_transition,
    emit_miss_slot, emit_target, emit_version_miss, emit_version_prologue, finish_version,
};

use adv_cpu_core::IndirectBranchKind;

use crate::asm::{Asm, Label};
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::facts::{self, Key};
use crate::jit_map::{Mark, Part};
use crate::runtime::Exit;
use front::flags::FlagSet;
use std::collections::VecDeque;
use trace::{Edge, Next, Trace};

/// Host features that change what the translator emits (A64-ISA-4).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Features {
    pub flagm: bool,
}

impl Unit {
    pub(crate) fn fact_mask(&self) -> u64 {
        let mut mask = x87::entry_facts(&self.trace);
        for inst in &self.trace.insts {
            match inst.op {
                decode::Op::Simd(simd) => {
                    if simd.floating() {
                        mask |= facts::MXCSR;
                    }
                    if simd.mmx() {
                        mask |= facts::MMX | facts::ES;
                    }
                }
                decode::Op::Fild64(..) => mask |= facts::MMX | facts::ES,
                _ => {}
            }
        }
        mask
    }

    /// Exit proofs conservatively keep only fields no instruction can write.
    pub(crate) fn preserved_facts(&self) -> u64 {
        use adv_cpu_core::floating::fast::Fast;
        let mut mask = facts::ALL;
        for inst in &self.trace.insts {
            if inst.call.is_some() || inst.mode == trace::Mode::Real {
                return 0;
            }
            match inst.op {
                decode::Op::X87(x) => match x.fast {
                    Fast::StoreControl | Fast::StoreStatus | Fast::Wait => {}
                    Fast::LoadControl => mask &= facts::MXCSR,
                    _ => mask &= facts::CONTROL | facts::MXCSR,
                },
                decode::Op::Fild64(..) => mask &= facts::CONTROL | facts::MXCSR,
                decode::Op::Simd(simd) => {
                    if simd.kind == SimdKind::LoadControl {
                        mask &= !facts::MXCSR;
                    }
                    if simd.mmx() {
                        mask &= facts::CONTROL | facts::MXCSR;
                    }
                }
                decode::Op::Value(_) | decode::Op::Body(_) | decode::Op::Syscall => return 0,
                _ => {}
            }
        }
        mask
    }
}

/// The JIT back end's output, in buffers the next unit reuses.
#[derive(Default)]
pub struct Code {
    pub(crate) asm: Asm,
    x87: x87::Scratch,
    labels: Vec<Label>,
    pub(crate) sites: Vec<(usize, Site)>,
    /// Out-of-line paths with the guest instruction they serve.
    cold: Vec<(usize, Cold)>,
    far: Vec<(usize, usize)>,
    pub(crate) links: Vec<Link>,
    pub(crate) internal_links: Vec<InternalLink>,
    pub(crate) marks: Vec<Mark>,
    pub(crate) retired: u32,
    pub(crate) uses_permissions: bool,
    resident_links: bool,
    pub(crate) table_positions: Vec<(usize, usize)>,
    pub(crate) version: VersionLayout,
}

const X6: Reg = Reg::new(6);
const X10: Reg = Reg::new(10);
const X11: Reg = Reg::new(11);
const X13: Reg = Reg::new(13);
const X14: Reg = Reg::new(14);
const X15: Reg = Reg::new(15);
const X17: Reg = Reg::new(17);
const SHADOW: Reg = Reg::new(28);

/// The host register of guest register `index`; ESP and EBP are biased.
const fn host(index: u8) -> Reg {
    Reg::new(19 + index)
}

const fn temp(index: u8) -> Reg {
    Reg::new(7 + index)
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
    nz: Option<Operand>,
    overflow: Overflow,
}

impl State {
    /// The block-exit form: also what a region head or merge point assumes.
    const CANONICAL: Self = Self {
        sense: Sense::Subtraction,
        carry: Carry::Host,
        derivable: None,
        recipe: Recipe::Canonical,
        nz: None,
        overflow: Overflow::Host,
    };
}

/// What a conditional branch tests, once `Translator::test` has prepared
/// it.
#[derive(Clone, Copy)]
enum Test {
    Cond(Cond),
    Zero(Reg, bool),
    /// PF set (true) or clear, from w16's bit 0.
    Parity(bool),
    EcxZero,
    /// CX, shifted into w16, is zero.
    CxZero,
    /// ECX, counted down, is not zero, and the condition holds if any.
    Count(Option<Cond>),
}

impl Test {
    fn inverse(self) -> Option<Self> {
        Some(match self {
            Self::Cond(cond) => Self::Cond(cond.invert()),
            Self::Zero(reg, nonzero) => Self::Zero(reg, !nonzero),
            Self::Parity(set) => Self::Parity(!set),
            Self::EcxZero => Self::Zero(host(1), true),
            Self::CxZero => Self::Zero(X16, true),
            Self::Count(None) => Self::EcxZero,
            Self::Count(Some(_)) => return None,
        })
    }
}

/// Code emitted after the unit body.
enum Cold {
    InlineBody {
        label: Label,
        resume: Label,
        body: adv_cpu_core::body::DirectBody,
        len: u32,
        vector: simd::Vector,
        pushes: Vec<decode::Src>,
    },
    BodyFallback {
        label: Label,
        state: State,
        vector: simd::Vector,
        pushes: Vec<decode::Src>,
        thunk: u32,
    },
    BodyYield {
        label: Label,
        vector: simd::Vector,
        entry: bool,
        release: u16,
        eip: u32,
        refund: u32,
    },
    /// A short branch's way to distant cold code: x87 arithmetic to its
    /// generic path, or a parity jcc to an exit that publishes a run.
    Bridge {
        label: Label,
        to: Label,
    },
    VersionMiss {
        label: Label,
        eip: u32,
    },
    FactTransfer {
        label: Label,
        state: State,
        simd_dirty: u16,
        eip: u32,
        refund: u32,
        x87: Option<x87::Leave>,
    },
    Table {
        index: usize,
        table: Label,
        stub: Label,
    },
    /// Leaves for a static EIP with canonical flags after refunding the
    /// countdown: through a link point, or through the exit stub.
    Leave {
        label: Label,
        state: State,
        simd_dirty: u16,
        refund: u32,
        exit: Exit,
        eip: u32,
        link: bool,
        /// What an x87 run inside which it leaves does first.
        x87: Option<x87::Leave>,
    },
    /// A forward internal edge that converts flags or settles the budget.
    Edge {
        label: Label,
        state: State,
        vector: simd::Vector,
        from: usize,
        to: usize,
        /// What an x87 run it leaves from does first.
        x87: Option<x87::Arrive>,
    },
    ResidentEdge {
        point: Label,
        count: Option<Label>,
        inline: bool,
        target: Label,
        state: State,
        simd_dirty: u16,
        x87: Option<x87::Leave>,
        eip: u32,
        refund: u32,
        source_pc: u32,
    },
    /// A backward edge arriving without some of its merge's entry set.
    Load {
        label: Label,
        vector: simd::Vector,
        entry: residency::Entry,
        target: Label,
        /// Where a register it cannot load leaves, the edge charged.
        leave: Label,
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
    /// A nonzero head-exit count: leave once it counts down to zero.
    Count {
        label: Label,
        leave: Label,
        resume: Label,
    },
    /// A proved slice exhausted its credit. Refill it only under the same lease.
    Checkpoint {
        label: Label,
        leave: Label,
        target: Label,
        refund: u32,
        debt: u32,
        /// Whether a recall waits for the form to return at its head.
        defer: bool,
    },
    /// Continues at the EIP in w16 through the indirect table.
    Lookup {
        label: Label,
        state: State,
        refund: u32,
        branch: u64,
        simd_dirty: u16,
        x87: Option<x87::Leave>,
    },
    X87Backedge {
        label: Label,
        state: State,
        vector: simd::Vector,
        from: usize,
        to: usize,
        arrive: x87::Arrive,
    },
    X87End {
        label: Label,
        state: State,
        vector: simd::Vector,
        leave: x87::Leave,
        done: Label,
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
        value: access::Value,
        kind: u32,
    },
    /// The host call asked to leave with the exit + 1 in w0, at
    /// the EIP in `CpuState`, after refunding the countdown.
    HostCall {
        label: Label,
        refund: u32,
    },
    /// The slow path of an SSE floating instruction.
    Floating(simd::Slow),
    /// The cold paths of a cached x87 instruction.
    X87(x87::Cold),
    /// Publishes a cached x87 run as it was before the instruction of a
    /// faulting site, then leaves through the fault stub.
    X87Fault {
        label: Label,
        model: x87::Model,
        saved: bool,
        suspended: bool,
        /// Its access is checked, leaving the guest flags in NZCV only.
        checked: bool,
    },
}

struct Translator<'a> {
    asm: &'a mut Asm,
    features: Features,
    trace: &'a Trace,
    /// The position of each instruction, after any countdown check.
    labels: &'a mut Vec<Label>,
    sites: &'a mut Vec<(usize, Site)>,
    cold: &'a mut Vec<(usize, Cold)>,
    marks: &'a mut Vec<Mark>,
    /// Branches to stubs: word index and target address.
    far: &'a mut Vec<(usize, usize)>,
    links: &'a mut Vec<Link>,
    internal_links: &'a mut Vec<InternalLink>,
    internal_targets: Vec<Label>,
    tables: &'a [front::tables::Table],
    table_positions: &'a mut [(usize, usize)],
    stubs: Stubs,
    /// Guest instructions whose accesses check their page first.
    checked: &'a dyn Fn(u32) -> bool,
    regions: &'a [(usize, adv_cpu_core::native_region::NativeRegion)],
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
    /// What each merge holds on arrival (`residency`).
    entry: Vec<residency::Entry>,
    uses_permissions: &'a mut bool,
    /// The x87 run whose stack lives in host registers, if any.
    x87: x87::Cache,
    x87_scratch: &'a mut x87::Scratch,
    /// Sites inside such runs and the stubs that publish before faulting.
    x87_sites: Vec<(usize, Label)>,
    /// Where an MMX register the cache cannot load leaves, when not before
    /// the current instruction (`simd::Translator::register`).
    unloadable: Option<Label>,
    /// MMX registers checked for loading (`simd::Translator::register`).
    aliased: u8,
    facts: Key,
    tier1: bool,
    computation: bool,
    resident_links: bool,
    #[cfg(native)]
    computation_facts: Option<ComputationFacts<'a>>,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_unit(
    code: &mut Code,
    unit: &Unit,
    eip: u32,
    checked: &dyn Fn(u32) -> bool,
    helpers: u32,
    features: Features,
    at: usize,
    stubs: Stubs,
    control: u16,
    pe: bool,
    resident_links: bool,
) {
    code.resident_links = resident_links;
    Jit {
        code,
        features,
        at,
        stubs,
        checked,
        helpers,
        facts: Key::x87(control, pe),
        tier1: false,
        arrivals: 0,
        #[cfg(native)]
        computation_facts: None,
    }
    .lower_form(unit, eip, false);
}

#[cfg(native)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_tier1(
    code: &mut Code,
    unit: &Unit,
    eip: u32,
    checked: &dyn Fn(u32) -> bool,
    helpers: u32,
    features: Features,
    at: usize,
    stubs: Stubs,
    resident_links: bool,
    facts: Key,
    arrivals: usize,
) {
    code.resident_links = resident_links;
    Jit {
        code,
        features,
        at,
        stubs,
        checked,
        helpers,
        facts,
        tier1: true,
        arrivals,
        computation_facts: None,
    }
    .lower_form(unit, eip, false);
}

/// The Tier-1 back end emits AArch64 code for host address `at`.
pub struct Jit<'a> {
    pub code: &'a mut Code,
    pub features: Features,
    pub at: usize,
    pub stubs: Stubs,
    pub checked: &'a dyn Fn(u32) -> bool,
    pub helpers: u32,
    facts: Key,
    tier1: bool,
    arrivals: usize,
    #[cfg(native)]
    computation_facts: Option<ComputationFacts<'a>>,
}

impl Jit<'_> {
    fn lower_form(&mut self, unit: &Unit, eip: u32, computation: bool) {
        self.code.asm.reset();
        self.code.x87.reset(unit.trace.len());
        while self.emit_unit(unit, eip, computation) {
            self.code.asm.rewind();
        }
    }

    /// Emits the unit, reporting whether a wider branch requires a new layout.
    fn emit_unit(&mut self, unit: &Unit, eip: u32, computation: bool) -> bool {
        let Code {
            asm,
            x87: x87_scratch,
            labels,
            sites,
            cold,
            far,
            links,
            internal_links,
            marks,
            retired,
            uses_permissions,
            resident_links,
            table_positions,
            version,
        } = &mut *self.code;
        let trace = unit.jit_trace(*resident_links && !computation);
        let tables = if computation { &[][..] } else { &unit.tables };
        let regions = if computation { &[][..] } else { &unit.regions };
        table_positions.clear();
        table_positions.resize(tables.len(), (0, 0));
        *uses_permissions = false;
        labels.clear();
        labels.extend((0..trace.len()).map(|_| asm.label()));
        sites.clear();
        cold.clear();
        far.clear();
        links.clear();
        internal_links.clear();
        marks.clear();
        let (body, miss) = emit_version_prologue(asm, self.tier1.then_some(self.facts));
        let mut entries = residency::entries(trace, self.checked, self.tier1.then_some(self.facts));
        if self.tier1 {
            let mxcsr = self.facts.mxcsr();
            for (index, entry) in entries.iter_mut().enumerate() {
                if trace.head(index) {
                    let masked = mxcsr.is_some_and(facts::masked_mxcsr);
                    let default = mxcsr.is_some_and(facts::default_mxcsr);
                    if trace.merge[index] {
                        entry.guards.fp_masked &= masked;
                        entry.guards.fp_default &= default;
                    } else {
                        entry.guards.fp_masked = masked;
                        entry.guards.fp_default = default;
                    }
                }
            }
        }
        let mut t = Translator {
            asm,
            features: self.features,
            trace,
            labels,
            sites,
            cold,
            marks,
            far,
            links,
            internal_links,
            internal_targets: Vec::new(),
            tables,
            table_positions,
            stubs: self.stubs,
            checked: self.checked,
            regions,
            helpers: self.helpers,
            checking: false,
            state: State::CANONICAL,
            index: 0,
            out: FlagSet::ALL,
            keep: FlagSet::ALL,
            vector: simd::Vector::default(),
            entry: entries,
            uses_permissions,
            x87: x87::Cache::for_trace(trace),
            x87_scratch,
            x87_sites: Vec::new(),
            unloadable: None,
            aliased: 0,
            facts: self.facts,
            tier1: self.tier1,
            computation,
            resident_links: *resident_links && !computation,
            #[cfg(native)]
            computation_facts: self.computation_facts,
        };
        if self.arrivals != 0 {
            // A proved link enters past the guards and still counts the arrival.
            t.mov_address(X16, self.arrivals);
            t.emit_all(&[
                ldr(X, X17, X16, 0),
                add_imm(X, X17, X17, 1, false),
                str(X, X17, X16, 0),
            ]);
        }
        if let Some(label) = miss {
            t.push_cold(Cold::VersionMiss { label, eip });
        }
        t.x87_prepare();
        if trace.len() == 0 {
            // Nothing to translate: other units may still link here.
            t.exit(Exit::Event(crate::runtime::Event::Fetch), eip);
            *retired = 0;
        } else {
            let mut index = 0;
            while index < trace.len() {
                if let Some((end, peel_entry)) = t.mmx_peel_plan(index) {
                    t.lower_peeled_mmx_loop(index, end, peel_entry);
                    index = end + 1;
                } else {
                    t.instruction(index);
                    index += 1;
                }
            }
            *retired = trace.prepaid(0);
        }
        let again = t.finish(self.at);
        *version = finish_version(asm, body, miss);
        again
    }
}

/// Facts from job admission used by Tier-1 instruction selection.
#[cfg(native)]
#[derive(Clone, Copy)]
pub(crate) struct ComputationFacts<'a> {
    pub frame_cache: Option<&'a FrameCache>,
    pub divisions: &'a [Option<Division>],
}

#[cfg(native)]
pub(crate) struct ComputationCode<'a> {
    pub words: &'a [u32],
    pub sites: &'a [(usize, Site)],
    pub marks: &'a [Mark],
}

#[cfg(native)]
pub(crate) fn lower_computation<'a>(
    unit: &Unit,
    code: &'a mut Code,
    eip: u32,
    features: Features,
    at: usize,
    stubs: Stubs,
    (control, pe, computation_facts): (u16, bool, Option<ComputationFacts<'_>>),
) -> Option<ComputationCode<'a>> {
    Jit {
        code,
        features,
        at,
        stubs,
        checked: &|_| false,
        helpers: 0,
        facts: Key::x87(control, pe),
        tier1: false,
        arrivals: 0,
        computation_facts,
    }
    .lower_form(unit, eip, true);
    (!code.uses_permissions
        && code.links.is_empty()
        && code
            .far
            .iter()
            .all(|&(_, to)| to == stubs.exit || to == stubs.fault))
    .then_some(ComputationCode {
        words: code.asm.words(),
        sites: &code.sites,
        marks: &code.marks,
    })
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

    /// Queues an out-of-line path of the current instruction.
    fn push_cold(&mut self, cold: Cold) {
        self.cold.push((self.index, cold));
    }

    /// Code from here on does `part` for the guest instruction at `eip`.
    fn mark(&mut self, eip: u32, part: Part) {
        let offset = self.asm.offset() as u32;
        if let Some(last) = self.marks.last_mut()
            && last.offset == offset
        {
            *last = Mark { offset, eip, part };
            return;
        }
        self.marks.push(Mark { offset, eip, part });
    }

    /// Code from here on does `part` for the instruction marked last.
    fn remark(&mut self, part: Part) {
        let eip = self.marks.last().map_or(0, |mark| mark.eip);
        self.mark(eip, part);
    }

    /// Marks the code `emit` adds as `part`, then returns to the part before.
    fn as_part(&mut self, part: Part, emit: impl FnOnce(&mut Self)) {
        let before = self.marks.last().map_or(Part::Body, |mark| mark.part);
        self.remark(part);
        emit(self);
        self.remark(before);
    }

    fn instruction(&mut self, index: usize) {
        self.instruction_prologue(index);
        self.asm.bind(self.labels[index]);
        self.instruction_body(index, true);
    }

    fn instruction_prologue(&mut self, index: usize) {
        let trace = self.trace;
        let inst = trace.insts[index];
        self.index = index;
        self.mark(inst.pc, Part::Body);
        self.checking = (self.checked)(inst.pc);
        self.out = trace.observe_out[index];
        self.keep = trace.observe_out[index].minus(inst.effects.writes);
        // Other flag writers use their existing exact hardware forms.
        if !matches!(
            inst.op,
            decode::Op::Alu(..) | decode::Op::Step(..) | decode::Op::Shift(..)
        ) && inst.effects.writes.any(FlagSet::NZV)
        {
            self.host_nzv(FlagSet::NZV.minus(inst.effects.writes));
        }
        if self.x87.active && trace.merge[index] {
            self.x87_merge(index);
        }
        if trace.head(index) || trace.merge[index] {
            // Control may arrive from elsewhere, holding the entry; edges
            // arrive through `jump` and `branch`. A region head's code
            // establishes its entry itself.
            let entry = if trace.head(index) {
                residency::Entry::default()
            } else {
                self.entry[index]
            };
            if index > 0 && trace.insts[index - 1].next == Next::Fall {
                self.coerce(entry);
            } else {
                self.vector = simd::Vector::at(entry);
            }
        } else if !matches!(inst.op, decode::Op::Simd(_) | decode::Op::X87(_)) {
            // Other instructions may use x5.
            self.vector.cpu = false;
        }
        if self.resident_links
            && index > 0
            && trace.insts[index - 1].next == Next::Fall
            && trace.resident_edge(index - 1, index)
        {
            let next = self.asm.label();
            let target = self.resident_edge(index - 1, index, next, self.x87_leaving(), true);
            self.asm.b(target);
            self.asm.bind(next);
        }
        if trace.head(index) {
            // Arrivals are canonical: the entry, or a return.
            let prepaid = trace.prepaid(index);
            self.state = State::CANONICAL;
            if self.tier1 && index != 0 && self.facts.mask() != 0 {
                let miss = self.fact_transfer(inst.pc, 0, None);
                self.facts.guard(self.asm, miss);
            }
            let leave = self.asm.label();
            let resume = self.computation.then(|| self.asm.label());
            let refuse = self.checkpoint(leave, resume, prepaid, prepaid);
            self.emit(sub_imm(X, X9, X9, prepaid, false));
            self.asm.tbz(true, X9, 63, refuse);
            if let Some(resume) = resume {
                self.asm.bind(resume);
            }
            self.push_cold(Cold::Leave {
                label: leave,
                state: State::CANONICAL,
                simd_dirty: self.vector.dirty,
                refund: prepaid,
                exit: self.budget_exit(),
                eip: inst.pc,
                link: false,
                x87: None,
            });
            self.enter_simd(self.entry[index], prepaid);
        } else if trace.merge[index] {
            self.convert(trace.live_in[index]);
            self.state = State::CANONICAL;
        }
        if index == 0 && !self.computation {
            self.body_stack_guard();
        }
        #[cfg(native)]
        if let Some(cache) = self.computation_facts.and_then(|facts| facts.frame_cache)
            && cache.head == index
        {
            self.load_frame_cache(&cache.offsets);
        }
    }

    fn instruction_body(&mut self, index: usize, batch: bool) {
        let inst = self.trace.insts[index];
        if batch && !self.computation {
            self.native_batch(index);
            self.channel_batch(index);
        }
        if !matches!(inst.op, decode::Op::X87(_)) {
            self.x87_start_integer();
            if self.x87.active {
                self.x87_integer();
            }
        }
        self.op(inst);
        if let Next::Edge(edge) = inst.next {
            self.x87_before_edge();
            self.follow(edge);
        }
        self.x87_end_integer();
    }

    /// Detects a short straight-line MMX loop whose peeled first iteration
    /// avoids per-iteration x87 guards, dead pre-loads, or backedge flushes.
    fn mmx_peel_plan(&self, head: usize) -> Option<(usize, residency::Entry)> {
        if self.computation || self.x87.active || !self.trace.merge[head] {
            return None;
        }
        let insts = &self.trace.insts;
        let end = (head + 1..insts.len())
            .take(64)
            .find(|&i| insts[i].taken == Some(Edge::Internal(head)))?;
        let has_next = match insts[end].next {
            Next::Fall => end + 1 < insts.len(),
            Next::Edge(Edge::Internal(to)) => !(head..=end).contains(&to),
            Next::Edge(_) => true,
            Next::Dynamic => false,
        };
        if !has_next {
            return None;
        }
        let mut written = 0u16;
        let mut read_before_write = 0u16;
        let mut has_mmx = false;
        let mut body_guards = simd::Guards::default();
        for i in head..=end {
            if i != head && (self.trace.head(i) || self.trace.merge[i]) {
                return None;
            }
            if i != end
                && (insts[i].next != Next::Fall
                    || matches!(insts[i].taken, Some(Edge::Internal(_))))
            {
                return None;
            }
            if self.regions.iter().any(|&(at, _)| at == i) {
                return None;
            }
            let (reads, writes) = residency::footprint(self.trace, i, self.checked)?;
            read_before_write |= reads & !written;
            written |= writes;
            if let decode::Op::Simd(s) = insts[i].op {
                if s.kind == SimdKind::Emms || self.tier1 && s.control_helper(self.facts) {
                    return None;
                }
                has_mmx |= s.mmx();
                body_guards = s.guarding(body_guards);
            }
        }
        if !has_mmx {
            return None;
        }
        let write_first = written & !read_before_write;
        let orig = self.entry[head];
        let peel_entry = residency::Entry {
            regs: orig.regs & !write_first,
            dirty: orig.dirty & !write_first,
            guards: orig.guards,
        };
        let removes_guard = !orig.guards.ready && body_guards.ready;
        let removes_dead_loads = (orig.regs & write_first) != 0;
        let removes_publish = (!orig.dirty & written) != 0;
        (removes_guard || removes_dead_loads || removes_publish).then_some((end, peel_entry))
    }

    fn lower_peeled_mmx_loop(&mut self, head: usize, end: usize, peel_entry: residency::Entry) {
        let orig_entry = self.entry[head];
        self.entry[head] = peel_entry;
        for i in head..end {
            self.instruction(i);
        }
        self.instruction_prologue(end);
        self.asm.bind(self.labels[end]);
        let warm_entry = self.vector.entry();
        let warm_head = self.asm.label();
        let orig_head_label = self.labels[head];
        self.labels[head] = warm_head;
        self.entry[head] = warm_entry;
        self.instruction_body(end, true);
        if self.trace.insts[end].next == Next::Fall {
            self.asm.b(self.labels[end + 1]);
        }
        let (peel_fall_state, peel_fall_vector) = (self.state, self.vector);

        let saved_labels: Vec<_> = (head + 1..=end)
            .map(|i| {
                let prev = self.labels[i];
                self.labels[i] = self.asm.label();
                prev
            })
            .collect();

        self.asm.bind(warm_head);
        let head_inst = self.trace.insts[head];
        self.index = head;
        self.mark(head_inst.pc, Part::Body);
        self.checking = (self.checked)(head_inst.pc);
        self.out = self.trace.observe_out[head];
        self.keep = self.out.minus(head_inst.effects.writes);
        self.state = State::CANONICAL;
        self.vector = simd::Vector::at(warm_entry);
        self.instruction_body(head, false);

        for i in head + 1..=end {
            self.instruction(i);
        }
        debug_assert_eq!(self.state, peel_fall_state);
        debug_assert_eq!(self.vector.dirty, peel_fall_vector.dirty);
        debug_assert_eq!(self.vector.guards, peel_fall_vector.guards);
        let _ = (peel_fall_state, peel_fall_vector);

        self.labels[head] = orig_head_label;
        for (i, label) in (head + 1..=end).zip(saved_labels) {
            self.labels[i] = label;
        }
        // Any later arrival targeting `head` enters the peeled pass.
        self.entry[head] = peel_entry;
        let _ = orig_entry;
    }

    fn budget_exit(&self) -> Exit {
        if self.computation {
            Exit::Recall
        } else {
            Exit::Budget
        }
    }

    /// The original refusal publishes a precise exit. Only the computation
    /// form can resume from the same live host registers instead.
    fn checkpoint(&mut self, leave: Label, target: Option<Label>, refund: u32, debt: u32) -> Label {
        if !self.computation {
            return leave;
        }
        let target = target.expect("computation checkpoint continuation");
        let label = self.asm.label();
        self.push_cold(Cold::Checkpoint {
            label,
            leave,
            target,
            refund,
            debt,
            defer: self.trace.returns_at_head(),
        });
        label
    }

    /// For an edge back to the head of a form that can stop there, loads
    /// the head-exit word into w0 and returns the label of its exit.
    fn head_exit(&mut self, to: usize) -> Option<Label> {
        if !self.computation || to != 0 || !self.trace.returns_at_head() {
            return None;
        }
        self.emit(ldr(W, X0, RUNTIME, crate::runtime::HEAD_EXIT));
        Some(self.asm.label())
    }

    /// Returns at the head with canonical state, as an ordinary exit, so the
    /// owner can admit the context again (A64-CTL-8).
    fn leave_at_head(&mut self, label: Label, refund: u32, x87: Option<x87::Leave>) {
        self.push_cold(Cold::Leave {
            label,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund,
            exit: Exit::Dispatch,
            eip: self.trace.insts[0].pc,
            link: false,
            x87,
        });
    }

    /// Where an instruction's fall-through or jump ends the trace.
    fn follow(&mut self, edge: Edge) {
        let unused = self.trace.unused(self.index);
        match edge {
            Edge::Internal(to) => {
                self.spin_edge(to, None);
                self.jump(to, self.x87_arriving(to));
            }
            Edge::Exit(eip) => self.follow_exit(Exit::Dispatch, eip, unused, true),
            Edge::Event(eip) => self.follow_exit(
                Exit::Event(crate::runtime::Event::Fetch),
                eip,
                unused,
                false,
            ),
        }
    }

    fn follow_exit(&mut self, exit: Exit, eip: u32, refund: u32, link: bool) {
        let back =
            self.resident_links && link && self.trace.insts[self.index].resident_backedge(eip);
        if let Some(x87) = self.x87_leaving() {
            let label = self.asm.label();
            self.asm.b(label);
            self.push_cold(Cold::Leave {
                label,
                state: self.state,
                simd_dirty: self.vector.dirty,
                refund,
                exit,
                eip,
                link,
                x87: Some(x87),
            });
        } else {
            self.leave(exit, eip, refund, link, link, back);
        }
    }

    /// An unconditional internal edge from the current instruction.
    fn jump(&mut self, to: usize, arrive: Option<x87::Arrive>) {
        let (target, x87) = match arrive {
            Some(x87::Arrive::Reconcile(_, model, target)) => {
                (target, Some(x87::Leave::Publish(model)))
            }
            _ => (self.labels[to], None),
        };
        if let Some(arrive) = arrive {
            self.x87_arrive(arrive, self.index, to);
        }
        self.coerce(self.entry[to]);
        self.convert(self.trace.live_in[to]);
        let net = self.trace.net(self.index, to);
        if let Some(done) = self.head_exit(to) {
            let (count, resume) = (self.asm.label(), self.asm.label());
            self.asm.cbz(W, true, X0, count);
            self.asm.bind(resume);
            self.push_cold(Cold::Count {
                label: count,
                leave: done,
                resume,
            });
            let refund = self.trace.unused(self.index);
            self.leave_at_head(done, refund, x87);
        }
        if net > 0 {
            let leave = self.asm.label();
            let refund = net as u32 + self.trace.unused(self.index);
            let refuse = self.checkpoint(leave, Some(target), refund, self.trace.owed(to));
            self.emit(sub_imm(X, X9, X9, net as u32, false));
            self.asm.tbz(true, X9, 63, refuse);
            self.push_cold(Cold::Leave {
                label: leave,
                state: self.state,
                simd_dirty: self.vector.dirty,
                refund,
                exit: self.budget_exit(),
                eip: self.trace.insts[to].pc,
                link: false,
                x87,
            });
        } else if net < 0 {
            self.emit(add_imm(X, X9, X9, (-net) as u32, false));
        }
        let x87 = x87.or_else(|| self.x87_leaving());
        let target = self.resident_edge(self.index, to, target, x87, true);
        self.asm.b(target);
    }

    fn resident_edge(
        &mut self,
        from: usize,
        to: usize,
        target: Label,
        x87: Option<x87::Leave>,
        inline: bool,
    ) -> Label {
        let eip = self.trace.insts[to].pc;
        if !self.resident_links || !self.trace.resident_edge(from, to) {
            return target;
        }
        let point = self.asm.label();
        let source = self.trace.insts[from];
        let count = source.resident_backedge(eip).then(|| self.asm.label());
        if inline {
            self.asm.bind(point);
        }
        self.push_cold(Cold::ResidentEdge {
            point,
            count,
            inline,
            target,
            state: self.state,
            simd_dirty: self.vector.dirty,
            x87,
            eip,
            refund: self.trace.owed(to),
            source_pc: source.pc,
        });
        if inline {
            count.unwrap_or(target)
        } else {
            point
        }
    }

    /// A jcc at the current instruction whose taken edge is `taken`.
    fn branch(&mut self, cc: decode::Cc, taken: Edge) {
        if self.trace.insts[self.index].spin
            && let Edge::Internal(to) = taken
        {
            let test = self.test(cc);
            self.spin_edge(to, Some(test));
        }
        // Existing internal edges publish on both paths. Side exits and
        // jumps into an appended path publish only on the taken path.
        if let Edge::Internal(to) = taken
            && (to < self.trace.primary || to <= self.index)
        {
            self.publish(!self.entry[to].dirty);
        }
        let from = self.index;
        match taken {
            Edge::Internal(to) if to <= from => {
                // A loop: convert on both paths, then check inline.
                self.convert(self.trace.live_in[to]);
                let x87 = self.x87_arriving(to);
                let mut target = self.labels[to];
                if let Some(arrive) = x87 {
                    let label = self.asm.label();
                    self.push_cold(Cold::X87Backedge {
                        label,
                        state: self.state,
                        vector: self.vector,
                        from,
                        to,
                        arrive,
                    });
                    target = label;
                }
                let mut unloadable = None;
                if !self.holds(self.entry[to]) {
                    // Only paths that dropped the cache get here.
                    let (load, leave) = (self.asm.label(), self.asm.label());
                    self.push_cold(Cold::Load {
                        label: load,
                        vector: self.vector,
                        entry: self.entry[to],
                        target,
                        leave,
                    });
                    target = load;
                    unloadable = Some(leave);
                }
                let test = self.test(cc);
                if let Some(label) = unloadable {
                    self.push_cold(Cold::Leave {
                        label,
                        state: self.state,
                        simd_dirty: self.vector.dirty,
                        refund: self.trace.owed(to),
                        exit: Exit::Dispatch,
                        eip: self.trace.insts[to].pc,
                        link: false,
                        x87: self.x87_leaving(),
                    });
                }
                if let Some(done) = self.head_exit(to) {
                    let (check, count, back) =
                        (self.asm.label(), self.asm.label(), self.asm.label());
                    self.asm.cbz(W, true, X0, check);
                    self.asm.bind(back);
                    let refund = self.trace.unused(from);
                    let x87 = self.x87_leaving();
                    self.leave_at_head(done, refund, x87);
                    self.push_cold(Cold::Refuse {
                        label: check,
                        net: 0,
                        test,
                        leave: count,
                        resume: back,
                    });
                    self.push_cold(Cold::Count {
                        label: count,
                        leave: done,
                        resume: back,
                    });
                }
                let net = self.trace.net(from, to);
                if net > 0 {
                    let net = net as u32;
                    let (refuse, leave, resume) =
                        (self.asm.label(), self.asm.label(), self.asm.label());
                    self.emit(sub_imm(X, X9, X9, net, false));
                    self.asm.tbz(true, X9, 63, refuse);
                    let target = self.resident_edge(from, to, target, self.x87_leaving(), false);
                    self.jump_if(test, target);
                    self.emit(add_imm(X, X9, X9, net, false));
                    self.asm.bind(resume);
                    let refund = self.trace.unused(from);
                    let taken = self.checkpoint(leave, Some(target), refund, self.trace.owed(to));
                    self.push_cold(Cold::Refuse {
                        label: refuse,
                        net,
                        test,
                        leave: taken,
                        resume,
                    });
                    self.push_cold(Cold::Leave {
                        label: leave,
                        state: self.state,
                        simd_dirty: self.vector.dirty,
                        refund,
                        exit: self.budget_exit(),
                        eip: self.trace.insts[to].pc,
                        link: false,
                        x87: self.x87_leaving(),
                    });
                } else if net == 0 {
                    let target = self.resident_edge(from, to, target, self.x87_leaving(), false);
                    self.jump_if(test, target);
                } else {
                    let label = self.asm.label();
                    self.jump_if(test, label);
                    self.push_cold(Cold::Edge {
                        label,
                        state: self.state,
                        vector: self.vector,
                        from,
                        to,
                        x87,
                    });
                }
            }
            Edge::Internal(to) => {
                let test = self.test(cc);
                let x87 = self.x87_arriving(to);
                let net = self.trace.net(from, to);
                if (net == 0 || (net < 0 && -net < 4096))
                    && !(self.resident_links && self.trace.resident_edge(from, to))
                    && !self.needs(self.trace.live_in[to])
                    && x87.is_none()
                    && self.vector.dirty & !self.entry[to].dirty == 0
                    && self.holds(self.entry[to])
                {
                    if net == 0 {
                        self.jump_if(test, self.labels[to]);
                        return;
                    }
                    if let Some(inverse) = test.inverse() {
                        let skip = self.asm.label();
                        self.jump_if(inverse, skip);
                        self.emit(add_imm(X, X9, X9, (-net) as u32, false));
                        self.asm.b(self.labels[to]);
                        self.asm.bind(skip);
                        return;
                    }
                }
                let label = self.asm.label();
                self.jump_if(test, label);
                self.push_cold(Cold::Edge {
                    label,
                    state: self.state,
                    vector: self.vector,
                    from,
                    to,
                    x87,
                });
            }
            Edge::Exit(eip) => {
                let test = self.test(cc);
                let x87 = self.x87_leaving();
                let label = self.asm.label();
                if matches!(test, Test::Parity(_)) && x87.is_some() {
                    // tbz reaches only short cold code.
                    let bridge = self.asm.label();
                    self.jump_if(test, bridge);
                    self.push_cold(Cold::Bridge {
                        label: bridge,
                        to: label,
                    });
                } else {
                    self.jump_if(test, label);
                }
                self.push_cold(Cold::Leave {
                    label,
                    state: self.state,
                    simd_dirty: self.vector.dirty,
                    refund: self.trace.unused(from),
                    exit: Exit::Dispatch,
                    eip,
                    link: true,
                    x87,
                });
            }
            Edge::Event(_) => unreachable!("a jcc target dispatches"),
        }
    }

    /// Only proved spin sites test whether the owner has another ready context.
    fn spin_edge(&mut self, to: usize, test: Option<Test>) {
        if self.computation || !self.trace.insts[self.index].spin {
            return;
        }
        let leave = self.asm.label();
        let keep = self.asm.label();
        self.emit(ldr(W, X3, RUNTIME, crate::runtime::SPIN_YIELD));
        self.asm.cbz(W, false, X3, keep);
        if let Some(test) = test {
            self.jump_if(test, leave);
        } else {
            self.asm.b(leave);
        }
        self.asm.bind(keep);
        self.push_cold(Cold::Leave {
            label: leave,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund: self.trace.unused(self.index),
            exit: Exit::Reschedule,
            eip: self.trace.insts[to].pc,
            link: false,
            x87: self.x87_leaving(),
        });
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
            Cc::Flags(cc @ (ConditionCode::e | ConditionCode::ne)) if self.state.nz.is_some() => {
                let value = self.narrow(self.state.nz.unwrap());
                Test::Zero(value, cc == ConditionCode::ne)
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
            Test::Zero(reg, nonzero) => self.asm.cbz(W, nonzero, reg, label),
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
        self.site_with_refund(self.trace.owed(self.index));
    }

    fn site_with_refund(&mut self, refund: u32) {
        let inst = self.trace.insts[self.index];
        self.sites.push((
            self.asm.offset(),
            Site {
                eip: inst.pc,
                refund,
                sense: self.state.sense,
                carry: self.state.carry,
                recipe: self.state.recipe,
                nz: self.state.nz,
                overflow: self.state.overflow,
                checked: self.checking,
                simd_dirty: self.vector.dirty,
                resume: 0,
                opt_record: None,
            },
        ));
        self.x87_site();
    }

    /// Whether `convert(flags)` would emit anything.
    fn architectural_exception(&mut self, vector: u8) -> Label {
        self.leave_for_event(crate::runtime::Event::Exception(vector))
    }

    fn leave_for_event(&mut self, cause: crate::runtime::Event) -> Label {
        let label = self.asm.label();
        let inst = self.trace.insts[self.index];
        self.push_cold(Cold::Leave {
            label,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund: self.trace.owed(self.index),
            exit: Exit::Event(cause),
            eip: inst.pc,
            link: false,
            x87: self.x87_leaving(),
        });
        label
    }

    /// Canonical flags, the refund for this path, then the exit: through a
    /// link point if `link`, on the main path if `main`, else to the owner.
    fn leave(&mut self, exit: Exit, eip: u32, refund: u32, link: bool, main: bool, back: bool) {
        self.flush_simd();
        self.canonicalize();
        if refund != 0 {
            self.emit(add_imm(X, X9, X9, refund, false));
        }
        if link {
            self.dispatch(eip, main, back);
        } else {
            self.exit(exit, eip);
        }
    }

    /// Leaves translated code: w0 the exit, w1 the EIP.
    fn exit(&mut self, exit: Exit, eip: u32) {
        emit_exit(self.asm, self.far, exit, eip, self.stubs.exit);
    }

    /// Continues at `eip` through the owner's helper: w1 the EIP and x2
    /// the link point, the first word, which the helper may patch into a
    /// branch to the target unit.
    fn dispatch(&mut self, eip: u32, main: bool, back: bool) {
        if self.computation {
            self.exit(Exit::Dispatch, eip);
            return;
        }
        let offset = emit_link_point(self.asm, eip, back);
        self.far(self.stubs.dispatch);
        let direct = if back {
            emit_back_count(self.asm);
            self.exit(Exit::Plan(crate::runtime::PlanEvent::DiscoverBackEdge), eip);
            None
        } else if self.resident_links {
            Some(emit_link_transition(self.asm, offset))
        } else {
            None
        };
        self.links.push(Link {
            offset,
            eip,
            main,
            back,
            direct,
        });
    }

    fn mov_address(&mut self, reg: Reg, value: usize) {
        let value = value as u64;
        self.emit(movz(X, reg, value as u16, 0));
        for shift in [16, 32, 48] {
            let part = (value >> shift) as u16;
            if part != 0 {
                self.emit(movk(X, reg, part, shift));
            }
        }
    }

    fn fact_transfer(&mut self, eip: u32, refund: u32, x87: Option<x87::Leave>) -> Label {
        let label = self.asm.label();
        self.push_cold(Cold::FactTransfer {
            label,
            state: self.state,
            simd_dirty: self.vector.dirty,
            eip,
            refund,
            x87,
        });
        label
    }

    fn version_miss(&mut self, eip: u32) {
        emit_version_miss(self.asm, self.far, eip, self.stubs.dispatch);
    }

    fn target(&mut self, eip: u32) {
        emit_target(self.asm, eip);
    }

    /// A branch to the stub at host address `to`, placed by `finish`.
    fn far(&mut self, to: usize) {
        emit_far(self.asm, self.far, to, b(0));
    }

    /// A call to the stub at host address `to`, placed by `finish`.
    fn far_call(&mut self, to: usize) {
        emit_far(self.asm, self.far, to, bl(0));
    }

    /// A `cbnz` to the miss stub at `to` for an indirect transfer of
    /// `kind` from this instruction, through cold code that loads x4.
    fn miss_cbnz(&mut self, rt: Reg, to: usize, kind: IndirectBranchKind) {
        let label = self.asm.label();
        self.asm.cbz(Size::X, true, rt, label);
        let branch = self.indirect(kind);
        self.push_cold(Cold::Miss { label, to, branch });
    }

    /// Descriptor for an indirect transfer of `kind` from here.
    fn indirect(&self, kind: IndirectBranchKind) -> u64 {
        u64::from(self.trace.insts[self.index].pc) | (kind as u64) << 32
    }

    /// x4 = `branch`.
    fn mov_branch(&mut self, branch: u64) {
        self.mov_imm(X4, branch as u32);
        self.emit(movk(X, X4, (branch >> 32) as u16, 32));
    }

    /// Leaves after a completed transfer whose guest target is in w16.
    fn computation_transfer(&mut self, branch: u64) {
        let kind = (branch >> 32) as u8;
        let reason = Exit::Plan(crate::runtime::PlanEvent::WorkerBranch(kind));
        self.emit_all(&[
            movz(X, X0, reason.word(), 0),
            movk(X, X0, branch as u16, 32),
            movk(X, X0, (branch >> 16) as u16, 48),
            mov(W, X1, X16),
        ]);
        self.far(self.stubs.exit);
    }

    fn cold(&mut self, cold: Cold, at: usize, source: usize) {
        match cold {
            Cold::InlineBody {
                label,
                resume,
                body,
                len,
                vector,
                pushes,
            } => {
                self.asm.bind(label);
                self.vector = vector;
                // Materialize before the body can observe or overwrite arguments.
                self.body_stores(&pushes);
                self.mov_imm(X3, len);
                self.body_call(body);
                self.asm.b(resume);
            }
            Cold::BodyFallback {
                label,
                state,
                vector,
                pushes,
                thunk,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.vector = vector;
                self.body_fallback(source, &pushes, thunk);
            }
            Cold::BodyYield {
                label,
                vector,
                entry,
                release,
                eip,
                refund,
            } => {
                self.asm.bind(label);
                self.vector = vector;
                self.flush_simd();
                self.body_yield(entry, release, eip, refund);
            }
            Cold::Bridge { label, to } => {
                self.asm.bind(label);
                self.asm.b(to);
            }
            Cold::VersionMiss { label, eip } => {
                emit_miss_slot(self.asm, label);
                self.version_miss(eip);
            }
            Cold::FactTransfer {
                label,
                state,
                simd_dirty,
                eip,
                refund,
                x87,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.vector.dirty = simd_dirty;
                if let Some(x87) = x87 {
                    self.x87_leave(x87);
                }
                self.flush_simd();
                self.canonicalize();
                if refund != 0 {
                    self.emit(add_imm(X, X9, X9, refund, false));
                }
                self.version_miss(eip);
            }
            Cold::Table { index, table, stub } => self.table_cold(index, table, stub, at),
            Cold::Leave {
                label,
                state,
                simd_dirty,
                refund,
                exit,
                eip,
                link,
                x87,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.vector.dirty = simd_dirty;
                if let Some(x87) = x87 {
                    self.x87_leave(x87);
                }
                let back = self.resident_links
                    && link
                    && self
                        .trace
                        .insts
                        .get(source)
                        .is_some_and(|inst| inst.resident_backedge(eip));
                self.leave(exit, eip, refund, link, false, back);
            }
            Cold::Edge {
                label,
                state,
                vector,
                from,
                to,
                x87,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.vector = vector;
                self.index = from;
                self.jump(to, x87);
            }
            Cold::ResidentEdge {
                point,
                count,
                inline,
                target,
                state,
                simd_dirty,
                x87,
                eip,
                refund,
                source_pc,
            } => {
                if !inline {
                    self.asm.bind(point);
                    self.asm.b(count.unwrap_or(target));
                }
                let counter = count.map(|count| {
                    const SP: Reg = Reg::new(31);
                    let counter = self.asm.label();
                    let hot = self.asm.label();
                    self.asm.bind(count);
                    crate::runtime::emit_sensor(self.asm, counter, X0, X1, hot);
                    self.asm.b(target);
                    self.asm.bind(hot);
                    stubs::emit_save_light(self.asm);
                    self.emit(stp_v(
                        VSize::Q,
                        Index::Pre,
                        VReg::new(24),
                        VReg::new(25),
                        SP,
                        -128,
                    ));
                    for i in 1..4 {
                        self.emit(stp_v(
                            VSize::Q,
                            Index::Offset,
                            VReg::new(24 + i * 2),
                            VReg::new(25 + i * 2),
                            SP,
                            i32::from(i) * 32,
                        ));
                    }
                    self.state = state;
                    self.vector.dirty = simd_dirty;
                    if let Some(x87) = x87 {
                        self.x87_leave(x87);
                    }
                    self.flush_simd();
                    self.canonicalize();
                    self.emit(ldr(X, X0, RUNTIME, crate::runtime::CPU_STATE));
                    stubs::emit_store_guest(self.asm, X0);
                    self.target(eip);
                    self.asm.adr(X2, point);
                    self.emit_all(&[
                        mov(X, X0, RUNTIME),
                        movz(W, X3, crate::runtime::DISCOVER_INTERNAL, 0),
                        ldr(X, X16, RUNTIME, crate::runtime::PLAN_OBSERVATION),
                        blr(X16),
                    ]);
                    for i in (1..4).rev() {
                        self.emit(ldp_v(
                            VSize::Q,
                            Index::Offset,
                            VReg::new(24 + i * 2),
                            VReg::new(25 + i * 2),
                            SP,
                            i32::from(i) * 32,
                        ));
                    }
                    self.emit(ldp_v(
                        VSize::Q,
                        Index::Post,
                        VReg::new(24),
                        VReg::new(25),
                        SP,
                        128,
                    ));
                    stubs::emit_restore_light(self.asm);
                    self.asm.b(target);
                    counter
                });

                let transition = self.asm.offset();
                self.state = state;
                self.vector.dirty = simd_dirty;
                if let Some(x87) = x87 {
                    self.x87_leave(x87);
                }
                self.flush_simd();
                self.canonicalize();
                self.emit(add_imm(X, X9, X9, refund, false));
                self.asm.adr(X14, point);
                let link = self.asm.offset();
                self.emit(b(0));
                let counter = counter.map(|counter| {
                    self.asm.bind(counter);
                    self.emit_all(&[0, 0]);
                    self.asm.position(counter)
                });
                self.internal_links.push(InternalLink {
                    eip,
                    point: self.asm.position(point),
                    target: 0,
                    transition,
                    link,
                    counter,
                    sensor: count.map(|sensor| self.asm.position(sensor)),
                    source_pc,
                });
                self.internal_targets.push(target);
            }
            Cold::Load {
                label,
                vector,
                entry,
                target,
                leave,
            } => {
                self.asm.bind(label);
                self.vector = vector;
                self.unloadable = Some(leave);
                self.coerce(entry);
                self.unloadable = None;
                self.asm.b(target);
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
            Cold::Count {
                label,
                leave,
                resume,
            } => {
                // NZCV may hold guest flags: count without setting them.
                self.asm.bind(label);
                self.emit_all(&[
                    ldr(W, X0, RUNTIME, crate::runtime::HEAD_EXIT),
                    sub_imm(W, X0, X0, 1, false),
                    str(W, X0, RUNTIME, crate::runtime::HEAD_EXIT),
                ]);
                self.asm.cbz(W, false, X0, leave);
                self.asm.b(resume);
            }
            Cold::Checkpoint {
                label,
                leave,
                target,
                refund,
                debt,
                defer,
            } => {
                // A form that can return at its head finishes the iteration,
                // so the owner can admit the context again at once. Past
                // RECALL_GRACE checkpoints it leaves where it is.
                let recalled = if defer { self.asm.label() } else { leave };
                let refill = self.asm.label();
                self.asm.bind(label);
                self.emit_all(&[
                    ldr(X, X0, RUNTIME, crate::runtime::RECALL),
                    ldr(W, X0, X0, 0),
                    ldr(W, X1, RUNTIME, crate::runtime::RECALL_MASK),
                    and(W, X0, X0, X1, Lsl, 0),
                ]);
                self.asm.cbz(W, true, X0, recalled);
                self.emit_all(&[
                    ldr(X, X0, RUNTIME, crate::runtime::COMPUTATION_PENDING),
                    ldr(W, X0, X0, 0),
                    ldr(W, X1, RUNTIME, crate::runtime::COMPUTATION_PENDING_MASK),
                    and(W, X0, X0, X1, Lsl, 0),
                ]);
                self.asm.cbz(W, true, X0, recalled);
                if defer {
                    self.asm.b(refill);
                    self.asm.bind(recalled);
                    self.emit_all(&[
                        movz(W, X0, 1, 0),
                        str(W, X0, RUNTIME, crate::runtime::HEAD_EXIT),
                        ldr(W, X0, RUNTIME, crate::runtime::DEFERRED),
                        add_imm(W, X0, X0, 1, false),
                        str(W, X0, RUNTIME, crate::runtime::DEFERRED),
                    ]);
                    // NZCV may hold guest flags: test the count's bit instead.
                    const GRACE: u32 = crate::runtime::RECALL_GRACE;
                    const { assert!(GRACE.is_power_of_two()) };
                    self.asm.tbz(true, X0, GRACE.trailing_zeros(), leave);
                }
                self.asm.bind(refill);
                self.mov_imm(X1, crate::runtime::COMPUTATION_SLICE - refund);
                self.emit_all(&[
                    sub(X, X1, X1, X9, Lsl, 0),
                    ldr(X, X0, RUNTIME, crate::runtime::COMPLETED),
                    add(X, X0, X0, X1, Lsl, 0),
                    str(X, X0, RUNTIME, crate::runtime::COMPLETED),
                ]);
                self.mov_imm(X9, crate::runtime::COMPUTATION_SLICE - debt);
                self.asm.b(target);
            }
            Cold::Lookup {
                label,
                state,
                refund,
                branch,
                simd_dirty,
                x87,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.canonicalize();
                if let Some(x87) = x87 {
                    self.emit(mov(W, X6, X16));
                    self.x87_leave(x87);
                    self.emit(mov(W, X16, X6));
                    self.vector.dirty = simd_dirty;
                    self.flush_simd();
                }
                if refund != 0 {
                    self.emit(add_imm(X, X9, X9, refund, false));
                }
                if self.computation {
                    self.computation_transfer(branch);
                } else {
                    self.mov_branch(branch);
                    self.far(self.stubs.lookup);
                }
            }
            Cold::X87Backedge {
                label,
                state,
                vector,
                from,
                to,
                arrive,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.vector = vector;
                self.index = from;
                self.x87_arrive(arrive, from, to);
                let target = match arrive {
                    x87::Arrive::Reconcile(_, _, target) => target,
                    x87::Arrive::Publish(_) => self.labels[to],
                };
                self.asm.b(target);
            }
            Cold::X87End {
                label,
                state,
                vector,
                leave,
                done,
            } => {
                self.asm.bind(label);
                self.state = state;
                self.vector = vector;
                self.x87_leave(leave);
                self.flush_simd();
                self.asm.b(done);
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
                self.target(target);
                self.asm.adr(Reg::new(2), point);
                self.far(self.stubs.dispatch);
                let offset = self.asm.position(point);
                let direct = self
                    .resident_links
                    .then(|| emit_link_transition(self.asm, offset));
                self.links.push(Link {
                    offset,
                    eip: target,
                    main: true,
                    back: false,
                    direct,
                });
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
                    self.emit(match value {
                        access::Value::Scalar(reg) => mov(W, X3, reg),
                        access::Value::Vector(VMem::S, reg) => fmov_from_v(W, X3, reg),
                        access::Value::Vector(size, reg) => {
                            vstr(size, reg, RUNTIME, crate::runtime::VALUE_OPERAND)
                        }
                    });
                }
                self.emit(movz(W, X4, kind as u16, 0));
                self.far_call(self.stubs.access);
                if !store {
                    self.emit(match value {
                        access::Value::Scalar(reg) => mov(W, reg, X0),
                        access::Value::Vector(VMem::S, reg) => fmov_to_v(W, reg, X0),
                        access::Value::Vector(size, reg) => {
                            vldr(size, reg, RUNTIME, crate::runtime::VALUE_OPERAND)
                        }
                    });
                }
                self.asm.b(resume);
            }
            Cold::Floating(slow) => {
                self.index = source;
                self.floating_cold(slow);
            }
            Cold::X87(cold) => self.x87_cold(cold),
            Cold::X87Fault {
                label,
                model,
                saved,
                suspended,
                checked,
            } => self.x87_fault(label, model, saved, suspended, checked),
            Cold::HostCall { label, refund } => {
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

    fn finish(mut self, at: usize) -> bool {
        // Short cold paths follow the body in emission order. Long x87
        // paths and exits that publish a run follow, then the tables.
        let rank = |cold: &Cold| match cold {
            Cold::Table { .. } => 2,
            Cold::X87(_)
            | Cold::X87Fault { .. }
            | Cold::X87End { .. }
            | Cold::Leave { x87: Some(_), .. }
            | Cold::Lookup { x87: Some(_), .. } => 1,
            _ => 0,
        };
        let mut queues: [VecDeque<(usize, Cold)>; 3] = Default::default();
        loop {
            for (source, cold) in self.cold.drain(..) {
                queues[rank(&cold)].push_back((source, cold));
            }
            let Some((source, cold)) = queues.iter_mut().find_map(VecDeque::pop_front) else {
                break;
            };
            let pc = self.trace.insts.get(source).map_or(0, |inst| inst.pc);
            self.mark(pc, Part::Cold);
            self.cold(cold, at, source);
        }
        // Any unit can reach past a bit test's 32 KiB range: long
        // straight-line traces and their cold paths as well as loops.
        if self.asm.widen_bit_tests() {
            return true;
        }
        for &(index, label) in &self.x87_sites {
            let (at, site) = &mut self.sites[index];
            site.resume = (self.asm.position(label) - *at) as i32;
        }
        for (link, target) in self.internal_links.iter_mut().zip(self.internal_targets) {
            link.target = self.asm.position(target);
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
        false
    }
}
