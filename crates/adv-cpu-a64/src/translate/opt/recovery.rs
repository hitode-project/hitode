//! Recovery records (A64-OPT-6). A record describes the guest state at a
//! region start: where each component lives, or `None` when no execution
//! from there reads it or the form never changes it, so that the state's
//! own value stands. The IR names values; emission replaces them by host
//! locations. A normal inline exit writes the record directly into canonical
//! Tier-1 state. The owner's materializer uses the captured host registers
//! to rebuild `CpuState` and frame bytes for jobs and cold exits.

use super::ir::{FpOp, Value};

/// Where a component's value lives at the region start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Location<L> {
    Constant(u64),
    At(L),
    Recipe(Recipe<L>),
}

/// A component recomputed from live values instead of held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recipe<L> {
    /// C1 after `a op b` rounded under the form's control word.
    C1 { op: FpOp, a: L, b: L },
    /// EFLAGS of a `Flags` value: its kind and operands, as the interpreter
    /// computes them.
    Eflags(L),
}

/// A flag producer with its inputs, evaluated only during cold recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FlagExpr<L> {
    At(L),
    Constant(u128),
    Produce {
        kind: super::ir::FlagKind,
        width: u8,
        a: Box<Self>,
        b: Box<Self>,
        prior: Box<Self>,
        /// Recover the original input from the arithmetic result.
        adjust: u32,
    },
}

impl<L: Copy> FlagExpr<L> {
    pub(crate) fn map<M>(&self, map: &mut impl FnMut(L) -> M) -> FlagExpr<M> {
        match self {
            Self::At(v) => FlagExpr::At(map(*v)),
            Self::Constant(bits) => FlagExpr::Constant(*bits),
            Self::Produce {
                kind,
                width,
                a,
                b,
                prior,
                adjust,
            } => FlagExpr::Produce {
                kind: *kind,
                width: *width,
                a: Box::new(a.map(map)),
                b: Box::new(b.map(map)),
                prior: Box::new(prior.map(map)),
                adjust: *adjust,
            },
        }
    }

    pub(crate) fn leaves(&self, out: &mut Vec<L>) {
        match self {
            Self::At(v) => out.push(*v),
            Self::Constant(_) => {}
            Self::Produce { a, b, prior, .. } => {
                a.leaves(out);
                b.leaves(out);
                prior.leaves(out);
            }
        }
    }

    fn evaluate(&self, get: &impl Fn(L) -> u128) -> u128 {
        match self {
            Self::At(v) => get(*v),
            Self::Constant(bits) => *bits,
            Self::Produce {
                kind,
                width,
                a,
                b,
                prior,
                adjust,
            } => {
                let a = a.evaluate(get);
                let a = if *adjust == 0 {
                    a
                } else {
                    u128::from((a as u32).wrapping_add(*adjust))
                };
                u128::from(compute_flags(
                    *kind,
                    *width,
                    a,
                    b.evaluate(get),
                    prior.evaluate(get) as u32,
                ))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FlagSelect<L> {
    Same,
    Tag(L),
    /// The first arm is the form entry, the second is the latch.
    Induction {
        current: L,
        initial: L,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FlagGroup<L> {
    pub mask: u32,
    pub select: FlagSelect<L>,
    pub arms: Vec<(u32, FlagExpr<L>)>,
}

impl<L: Copy> FlagGroup<L> {
    pub(crate) fn map<M>(&self, map: &mut impl FnMut(L) -> M) -> FlagGroup<M> {
        FlagGroup {
            mask: self.mask,
            select: match self.select {
                FlagSelect::Same => FlagSelect::Same,
                FlagSelect::Tag(v) => FlagSelect::Tag(map(v)),
                FlagSelect::Induction { current, initial } => FlagSelect::Induction {
                    current: map(current),
                    initial: map(initial),
                },
            },
            arms: self
                .arms
                .iter()
                .map(|(tag, expr)| (*tag, expr.map(map)))
                .collect(),
        }
    }

    pub(crate) fn uses(&self) -> impl Iterator<Item = L> {
        let mut leaves = Vec::new();
        match self.select {
            FlagSelect::Same => {}
            FlagSelect::Tag(v) => leaves.push(v),
            FlagSelect::Induction { current, initial } => leaves.extend([current, initial]),
        }
        for (_, expr) in &self.arms {
            expr.leaves(&mut leaves);
        }
        leaves.into_iter()
    }

    pub(crate) fn bits(&self, get: impl Fn(L) -> u128) -> u32 {
        let arm = match self.select {
            FlagSelect::Same => &self.arms[0],
            FlagSelect::Tag(v) => self
                .arms
                .iter()
                .find(|(tag, _)| u128::from(*tag) == get(v))
                .unwrap(),
            FlagSelect::Induction { current, initial } => {
                &self.arms[usize::from(get(current) != get(initial))]
            }
        };
        arm.1.evaluate(&get) as u32 & self.mask
    }
}

/// The x87 state at a region start. TOP, occupancy, the control word and
/// the status bits are proven constants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct X87<L> {
    pub top: u8,
    /// Bit `i` set when physical register `i` is valid. The materializer
    /// derives each valid register's tag from its value; empty registers
    /// keep their contents and tags from the job's entry state.
    pub valid: u8,
    pub control: u16,
    /// Physical registers 0–7: valid slots hold binary64 values.
    pub stack: [Option<Location<L>>; 8],
    /// Condition and exception bits other than C1 and TOP.
    pub status: u16,
    pub c1: Option<Location<L>>,
    /// Last instruction pointer and opcode, as I32 values when arrivals
    /// at a head disagree.
    pub ip: Option<Location<L>>,
    pub opcode: Option<Location<L>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record<L> {
    pub eip: u32,
    /// Instructions charged to the countdown and not retired at `eip`: the
    /// trace's `owed` at a head, its `unused` after an instruction.
    pub refund: u32,
    pub regs: [Option<Location<L>>; 8],
    pub eflags: Option<Location<L>>,
    /// Exact arrival recipes for a resident region.
    pub flag_groups: Vec<FlagGroup<L>>,
    /// `None` when the form runs no x87 instruction: the state's own x87
    /// registers stand.
    pub x87: Option<X87<L>>,
    pub xmm: [Option<Location<L>>; 8],
    pub mmx: [Option<Location<L>>; 8],
    /// MMX alias metadata for units with MMX/EMMS: bits 0–7 mark written MMX
    /// registers whose x87 exponent is `0xffff`; bits 8–9 encode tag state
    /// (0 untouched, 1 zero, 2 `0xffff`); bit 10 clears x87 TOP when set.
    pub mmx_meta: Option<Location<L>>,
    /// Frame bytes whose values differ from memory: offset from entry ESP,
    /// width in bytes and value.
    pub frame: Vec<(i32, u8, Location<L>)>,
}

impl<L: Copy> Record<L> {
    /// Every location the record reads.
    pub(crate) fn uses(&self) -> impl Iterator<Item = L> + '_ {
        let x87 = self
            .x87
            .iter()
            .flat_map(|x| x.stack.iter().chain([&x.c1, &x.ip, &x.opcode]));
        let held = self
            .regs
            .iter()
            .chain([&self.eflags, &self.mmx_meta])
            .chain(x87);
        held.chain(&self.xmm)
            .chain(&self.mmx)
            .flatten()
            .chain(self.frame.iter().map(|(_, _, location)| location))
            .flat_map(|location| match *location {
                Location::Constant(_) => [None, None],
                Location::At(at) | Location::Recipe(Recipe::Eflags(at)) => [Some(at), None],
                Location::Recipe(Recipe::C1 { a, b, .. }) => [Some(a), Some(b)],
            })
            .flatten()
            .chain(self.flag_groups.iter().flat_map(FlagGroup::uses))
    }

    /// Whether every component is present but the registers in `kept`
    /// (bits 0–7, XMM at 8–15, MMX at 16–23), as exits require (A64-OPT-6).
    pub(crate) fn complete(&self, kept: u32) -> bool {
        let held = |bit: usize, loc: &Option<Location<L>>| loc.is_some() || kept >> bit & 1 != 0;
        self.regs.iter().enumerate().all(|(r, loc)| held(r, loc))
            && self.xmm.iter().enumerate().all(|(r, loc)| held(8 + r, loc))
            && self
                .mmx
                .iter()
                .enumerate()
                .all(|(r, loc)| held(16 + r, loc))
            && self.eflags.is_some()
            && self.x87.as_ref().is_none_or(|x| {
                x.c1.is_some()
                    && x.ip.is_some()
                    && x.opcode.is_some()
                    && (0..8).all(|i| x.valid >> i & 1 == 0 || x.stack[i].is_some())
            })
    }
}

/// A record over IR values, as the front end builds it.
pub(crate) type IrRecord = Record<Value>;

/// A stable location in an optimized form. Spill cells are 16 bytes each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    Register(crate::encode::Reg),
    Vector(crate::encode::VReg),
    Spill(u32, super::ir::Type),
    Constant(u128),
    Flags(u32),
}

/// EFLAGS recomputed only when a record is materialized.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FlagRecipe {
    pub kind: super::ir::FlagKind,
    pub width: u8,
    pub a: Host,
    pub b: Host,
    pub prior: Option<Host>,
    pub immediate: Option<u64>,
}

/// The leaf that supplies flags an arithmetic recipe never changes.
pub(crate) fn flag_base(mut flags: Host, recipes: &[Option<FlagRecipe>]) -> Host {
    while let Host::Flags(i) = flags {
        flags = recipes[i as usize]
            .unwrap()
            .prior
            .unwrap_or(Host::Constant(0));
    }
    flags
}

pub(crate) type Dump = crate::runtime::RegisterDump;

struct Locations<'a> {
    dump: &'a Dump,
    spills: &'a [u128],
    flags: &'a [Option<FlagRecipe>],
}

impl Locations<'_> {
    fn get(&self, host: Host) -> u128 {
        match host {
            Host::Register(n) => u128::from(self.dump.x[n.number() as usize]),
            Host::Vector(n) => self.dump.v[n.number() as usize],
            Host::Spill(n, ty) => match ty {
                super::ir::Type::V128 => self.spills[n as usize],
                super::ir::Type::I32 | super::ir::Type::Flags => {
                    self.spills[n as usize] as u32 as u128
                }
                _ => self.spills[n as usize] as u64 as u128,
            },
            Host::Flags(n) => u128::from(self.flags(n)),
            Host::Constant(bits) => bits,
        }
    }

    fn flags(&self, index: u32) -> u32 {
        let r = self.flags[index as usize].unwrap();
        let a = self.get(r.a);
        let b = if r.kind.reads_b() { self.get(r.b) } else { 0 };
        let prior = r.prior.map_or(0, |p| self.get(p) as u32);
        compute_flags(r.kind, r.width, a, b, prior)
    }

    fn locate(&self, location: Location<Host>) -> u128 {
        match location {
            Location::Constant(bits) => u128::from(bits),
            Location::At(host) | Location::Recipe(Recipe::Eflags(host)) => self.get(host),
            Location::Recipe(Recipe::C1 { op, a, b }) => super::eval::locate(
                &Location::Recipe(Recipe::C1 {
                    op,
                    a: Value(0),
                    b: Value(1),
                }),
                &[self.get(a), self.get(b)],
            ),
        }
    }
}

fn compute_flags(kind: super::ir::FlagKind, width: u8, a: u128, b: u128, prior: u32) -> u32 {
    use adv_cpu_core::integer::{AF, CF, OF, PF, SF, ZF};
    if kind.floating() {
        // Recovery already knows the comparison completed.
        let (a, b) = (f64::from_bits(a as u64), f64::from_bits(b as u64));
        let set = if a.is_nan() || b.is_nan() {
            ZF | PF | CF
        } else if a < b {
            CF
        } else if a == b {
            ZF
        } else {
            0
        };
        prior & !(ZF | PF | CF | OF | SF | AF) | set
    } else {
        super::eval::flags(kind, width, a, b, prior).unwrap()
    }
}

/// Rebuilds the entry baseline before materializing a cold inline record.
pub(crate) fn restore_inline_entry(
    entry: &crate::runtime::OptEntryState,
    cpu: &mut adv_cpu_core::CpuState,
) {
    cpu.regs = entry.regs;
    let nzcv = (entry.nzcv >> 24) as u32;
    let parity = u32::from((entry.pf as u8).count_ones().is_multiple_of(2));
    let mapped = (nzcv & 0xc0)
        | ((nzcv >> 5 ^ 1) & 1)
        | ((nzcv & 0x10) << 7)
        | (parity << 2)
        | (entry.af as u32 & 0x10)
        | (entry.df as u32) << 10;
    cpu.eflags = (cpu.eflags & !crate::runtime::MAPPED_FLAGS) | mapped;
}

/// Restores the record's observable state and frame bytes. The returned
/// refund is added to the captured countdown to obtain the retired count.
/// `spills` contains the form's cells read from `dump.sp`.
// spec: A64-OPT-6
pub(crate) fn materialize(
    record: &Record<Host>,
    dump: &Dump,
    spills: &[u128],
    flags: &[Option<FlagRecipe>],
    cpu: &mut adv_cpu_core::CpuState,
    mut frame: impl FnMut(i32, &[u8]),
) -> u32 {
    let values = Locations {
        dump,
        spills,
        flags,
    };
    for (reg, location) in cpu.regs.iter_mut().zip(record.regs) {
        if let Some(location) = location {
            *reg = values.locate(location) as u32;
        }
    }
    if let Some(location) = record.eflags {
        cpu.eflags = values.locate(location) as u32;
        for group in &record.flag_groups {
            cpu.eflags = cpu.eflags & !group.mask | group.bits(|v| values.get(v));
        }
    }
    cpu.eip = record.eip;
    if let Some(x87) = &record.x87 {
        materialize_x87(x87, &values, cpu);
    }
    for (reg, location) in cpu.xmm.iter_mut().zip(record.xmm) {
        if let Some(location) = location {
            *reg = values.locate(location).to_le_bytes();
        }
    }
    let meta = record.mmx_meta.map_or(0, |l| values.locate(l) as u32);
    for (n, location) in record.mmx.iter().enumerate() {
        if let Some(location) = location {
            let value = values.locate(*location) as u64;
            cpu.mmx[n] = value;
            if meta >> n & 1 != 0 {
                cpu.x87.regs[n] = adv_cpu_core::state::X87Register::from_parts(value, 0xffff);
            }
        }
    }
    if meta >> 10 & 1 != 0 {
        cpu.x87.set_top(0);
    }
    match meta >> 8 & 3 {
        1 => cpu.x87.tag = 0,
        2 => cpu.x87.tag = 0xffff,
        _ => {}
    }
    for &(offset, width, location) in &record.frame {
        frame(
            offset,
            &values.locate(location).to_le_bytes()[..usize::from(width)],
        );
    }
    record.refund
}

fn materialize_x87(x87: &X87<Host>, values: &Locations, cpu: &mut adv_cpu_core::CpuState) {
    for (i, location) in x87.stack.iter().enumerate() {
        if x87.valid >> i & 1 != 0
            && let Some(location) = location
        {
            let value = binary80(values.locate(*location) as u64);
            cpu.x87.regs[i] = value;
            cpu.mmx[i] = value.significand();
            cpu.x87.tag = cpu.x87.tag & !(3 << (2 * i)) | value.tag() << (2 * i);
        }
    }
    cpu.x87.control = x87.control;
    cpu.x87.status = x87.status & !((7 << 11) | (1 << 9)) | u16::from(x87.top) << 11;
    if let Some(location) = x87.c1 {
        cpu.x87.status |= (values.locate(location) as u16 & 1) << 9;
    }
    if let Some(location) = x87.ip {
        cpu.x87.ip = values.locate(location) as u32;
    }
    if let Some(location) = x87.opcode {
        cpu.x87.opcode = values.locate(location) as u16;
    }
}

fn binary80(bits: u64) -> adv_cpu_core::state::X87Register {
    let sign = ((bits >> 48) as u16) & 0x8000;
    let exponent = (bits >> 52) & 0x7ff;
    let fraction = bits & ((1 << 52) - 1);
    let (significand, exponent) = match exponent {
        0 if fraction == 0 => (0, 0),
        0 => {
            let shift = fraction.leading_zeros();
            (fraction << shift, (16383 - 1074 + 63 - shift) as u16)
        }
        0x7ff => ((1 << 63) | fraction << 11, 0x7fff),
        _ => ((1 << 63) | fraction << 11, (exponent + 16383 - 1023) as u16),
    };
    adv_cpu_core::state::X87Register::from_parts(significand, sign | exponent)
}

pub(crate) fn lower_record(record: &IrRecord, locations: &[Host]) -> Record<Host> {
    let loc = |location: Location<Value>| match location {
        Location::Constant(bits) => Location::Constant(bits),
        Location::At(v) => Location::At(locations[v.0 as usize]),
        Location::Recipe(Recipe::Eflags(v)) => {
            Location::Recipe(Recipe::Eflags(locations[v.0 as usize]))
        }
        Location::Recipe(Recipe::C1 { op, a, b }) => Location::Recipe(Recipe::C1 {
            op,
            a: locations[a.0 as usize],
            b: locations[b.0 as usize],
        }),
    };
    Record {
        eip: record.eip,
        refund: record.refund,
        regs: record.regs.map(|l| l.map(loc)),
        eflags: record.eflags.map(loc),
        flag_groups: record
            .flag_groups
            .iter()
            .map(|group| group.map(&mut |v| locations[v.0 as usize]))
            .collect(),
        x87: record.x87.as_ref().map(|x| X87 {
            top: x.top,
            valid: x.valid,
            control: x.control,
            stack: x.stack.map(|l| l.map(loc)),
            status: x.status,
            c1: x.c1.map(loc),
            ip: x.ip.map(loc),
            opcode: x.opcode.map(loc),
        }),
        xmm: record.xmm.map(|l| l.map(loc)),
        mmx: record.mmx.map(|l| l.map(loc)),
        mmx_meta: record.mmx_meta.map(loc),
        frame: record
            .frame
            .iter()
            .map(|&(o, w, l)| (o, w, loc(l)))
            .collect(),
    }
}

pub(crate) fn lower_flags(
    defs: &[Option<super::ir::Op>],
    locations: &[Host],
) -> Vec<Option<FlagRecipe>> {
    use super::ir::Op;
    defs.iter()
        .map(|op| match *op {
            Some(Op::Flags {
                kind,
                width,
                a,
                b,
                prior,
            }) => Some(FlagRecipe {
                kind,
                width,
                a: locations[a.0 as usize],
                b: locations[b.0 as usize],
                prior: prior.map(|v| locations[v.0 as usize]),
                immediate: match defs[b.0 as usize] {
                    Some(Op::Constant(_, bits)) => Some(bits as u64),
                    _ => None,
                },
            }),
            _ => None,
        })
        .collect()
}

/// Source operands a lazy flag recipe keeps live, including its prior recipe.
pub(crate) fn leaves_of(
    v: Value,
    defs: &[Option<super::ir::Op>],
    imm: bool,
    out: &mut Vec<(Value, bool)>,
) {
    use super::ir::Op;
    if let Some(Op::Flags {
        kind,
        width,
        a,
        b,
        prior,
    }) = defs[v.0 as usize]
    {
        leaves_of(a, defs, false, out);
        let encodable = matches!(defs[b.0 as usize], Some(Op::Constant(_, bits)) if bits <= 4095 || bits & 4095 == 0 && bits >> 12 <= 4095);
        if kind.reads_b() {
            leaves_of(
                b,
                defs,
                width == 4
                    && matches!(kind, super::ir::FlagKind::Add | super::ir::FlagKind::Sub)
                    && encodable,
                out,
            );
        }
        if let Some(prior) = prior {
            leaves_of(prior, defs, false, out);
        }
    } else {
        out.push((v, imm));
    }
}
