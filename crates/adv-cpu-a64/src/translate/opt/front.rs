//! Guest state as SSA values, with frame bytes shared across access widths.

use std::collections::BTreeMap;

use adv_cpu_core::CpuState;
use iced_x86::{ConditionCode, Mnemonic as M};

use super::ir::*;
use super::recovery::{IrRecord, Location, Recipe, X87};
use crate::translate::Unit;
use crate::translate::computation::Proof;
use crate::translate::decode::{self, Cc, EBP, ESP, Mem, Part, Src};
use crate::translate::trace::{Edge, Mode, Next};

mod cells;
mod integer;
mod live;
mod model;
mod passes;
mod simd;
#[cfg(test)]
mod tests;
mod x87;

pub(crate) struct Built {
    pub func: Func,
    pub assumed: Assumed,
    /// Appended-path instructions that leave instead of running.
    pub cuts: usize,
}

/// Entry facts checked by admission before selecting this form.
#[derive(Debug)]
pub(crate) struct Assumed {
    pub control: u16,
    pub status: u16,
    pub mxcsr: u32,
    pub top: u8,
    pub tags: u16,
    /// ESP modulo 16, when the unit aligns ESP.
    pub esp_low: Option<u8>,
    pub denormal: bool,
    /// Whether the form runs x87 instructions, so that the x87 facts hold.
    pub x87: bool,
    /// The MXCSR bits the form assumes: all of them when it runs SSE
    /// floating-point or control instructions, else the control bits.
    pub mxcsr_mask: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reject {
    Empty,
    Interpret(usize),
    Helper(usize),
    Mmx(usize),
    Unsupported(usize),
    Mode(usize),
    Stack(usize),
    Escape(usize),
    Dynamic(usize),
    StaticX87(usize),
    /// A head where observed flags differ between arrivals (A64-FLAG-3).
    Flags(usize),
}

impl Reject {
    /// The trace position of the instruction refused, when there is one.
    pub(crate) fn index(&self) -> Option<usize> {
        match *self {
            Reject::Empty => None,
            Reject::Interpret(i)
            | Reject::Helper(i)
            | Reject::Mmx(i)
            | Reject::Unsupported(i)
            | Reject::Mode(i)
            | Reject::Stack(i)
            | Reject::Escape(i)
            | Reject::Dynamic(i)
            | Reject::StaticX87(i)
            | Reject::Flags(i) => Some(i),
        }
    }
}

#[derive(Clone)]
struct State {
    regs: [Value; 8],
    flags: Value,
    xmm: [Value; 8],
    x87: [Option<Value>; 8],
    top: u8,
    control: u16,
    c1: Option<Location<Value>>,
    ip: Location<Value>,
    opcode: Location<Value>,
    floats: BTreeMap<i32, Value>,
    dirty: std::collections::BTreeSet<i32>,
    truncations: BTreeMap<(i32, u8), Value>,
    /// Live frame cells by offset, zero-extended to I32 up to four bytes
    /// and to I64 above.
    frame: BTreeMap<i32, Value>,
}

type Offsets = [Option<i32>; 8];

/// Where a record's region starts, which decides the components it holds.
#[derive(Clone, Copy)]
enum Start {
    /// On entry to a trace instruction.
    Entry(usize),
    /// After a trace instruction.
    After(usize),
    Exit,
}

struct Front<'a> {
    unit: &'a Unit,
    proof: &'a Proof,
    at: &'a CpuState,
    assumed: Assumed,
    func: Func,
    block: Block,
    index: usize,
    offsets: Vec<Offsets>,
    models: Vec<model::Model>,
    /// Offsets of the frame bytes the unit accesses, ascending.
    frame: Vec<i32>,
    /// The frame cells, by offset and width: every access covers whole
    /// cells, so a slot accessed at one width is one cell.
    cells: BTreeMap<i32, u8>,
    live: live::Live,
    /// The cells each instruction's block holds as values.
    held: Vec<std::collections::BTreeSet<i32>>,
    /// At a loop head that lacks registers, the cells its arrivals keep as
    /// values; they commit the others.
    carried: Vec<Option<std::collections::BTreeSet<i32>>>,
    /// The host address of entry ESP plus `bias`.
    base: Value,
    bias: i32,
    state: State,
    loads: BTreeMap<Value, usize>,
    stores: BTreeMap<RecordId, usize>,
    pending_store: Option<(u8, Value, Value)>,
}

/// Builds the optimized form of `unit`. An instruction on an appended path
/// (a branch target the compiler laid out away from the fall-through) that
/// the form refuses leaves at its EIP instead, so that a cold path does not
/// keep its hot loop in the ordinary form.
pub(crate) fn build(unit: &Unit, proof: &Proof, at: &CpuState) -> Result<Built, Reject> {
    let mut cut = vec![false; unit.trace.len()];
    let mut view = None::<Unit>;
    loop {
        let reject = match build_trace(view.as_ref().unwrap_or(unit), proof, at) {
            Ok((func, assumed)) => {
                let cuts = cut.iter().filter(|&&c| c).count();
                return Ok(Built {
                    func,
                    assumed,
                    cuts,
                });
            }
            Err(reject) => reject,
        };
        match reject.index() {
            Some(i) if i >= unit.trace.primary && cut.get(i) == Some(&false) => cut[i] = true,
            _ => return Err(reject),
        }
        view = Some(Unit {
            trace: unit.trace.cut(&cut),
            ..Unit::default()
        });
    }
}

fn build_trace(unit: &Unit, proof: &Proof, at: &CpuState) -> Result<(Func, Assumed), Reject> {
    if unit.trace.insts.is_empty() {
        return Err(Reject::Empty);
    }
    let insts = &unit.trace.insts;
    let x87 = insts
        .iter()
        .any(|i| matches!(i.op, decode::Op::X87(_) | decode::Op::Fild64(..)));
    let simd = |pick: fn(&crate::translate::simd::Simd) -> bool| {
        insts
            .iter()
            .any(|i| matches!(i.op, decode::Op::Simd(s) if pick(&s)))
    };
    let floating = simd(|s| s.floating());
    let control = simd(|s| {
        matches!(
            s.kind,
            crate::translate::simd::Kind::StoreControl | crate::translate::simd::Kind::LoadControl
        )
    });
    // Precision exceptions are sticky and the form's arithmetic may raise
    // them, so a form that computes assumes they are already raised.
    let mut forced = at.clone();
    forced.x87.status |= if x87 { 0x20 } else { 0 };
    forced.mxcsr |= if floating { 0x20 } else { 0 };
    let at = &forced;
    let assumed = Assumed {
        control: at.x87.control,
        status: at.x87.status & !0x3800,
        mxcsr: at.mxcsr,
        top: at.x87.top() as u8,
        tags: at.x87.tag,
        esp_low: insts
            .iter()
            .any(|i| matches!(i.op, decode::Op::Alu(decode::Alu::And, Src::Reg(ESP), _)))
            .then_some((at.regs[ESP as usize] & 15) as u8),
        denormal: at.x87.status & 2 != 0,
        x87,
        mxcsr_mask: if floating || control {
            u32::MAX
        } else {
            0xffc0
        },
    };
    let (offsets, frame, cells) = layout(unit, assumed.esp_low.unwrap_or(0))?;
    let mut arrivals = vec![Vec::new(); unit.trace.len()];
    for (from, ins) in unit.trace.insts.iter().enumerate() {
        let fall = match ins.next {
            Next::Edge(e) => Some(e),
            Next::Fall => Some(Edge::Internal(from + 1)),
            Next::Dynamic => None,
        };
        for e in [fall, ins.taken].into_iter().flatten() {
            if let Edge::Internal(to) = e {
                arrivals[to].push(from);
            }
        }
    }
    let models = model::analyze(unit, at, &offsets, &[])?;
    let live = live::analyze(unit, proof, &offsets, &frame, &models);
    let plan = cells::Cells {
        cells: &cells,
        frame: &frame,
    };
    let carried = plan.carried(unit, &arrivals, &live);
    let models = model::analyze(unit, at, &offsets, &plan.kept(&carried))?;
    let held = plan.held(unit, &live, &carried, &models);
    let mut func = Func {
        kept: live.kept,
        ..Func::default()
    };
    let entry_block = func.block(None);
    // Dead registers hold placeholders that no operation or record reads.
    let unused = [
        func.push(entry_block, Op::Constant(Type::I32, 0)).unwrap(),
        func.push(entry_block, Op::Constant(Type::V128, 0)).unwrap(),
    ];
    let fixed: Vec<Source> = (0..8)
        .map(Source::Reg)
        .chain([Source::Eflags])
        .chain((0..8).map(Source::Xmm))
        .collect();
    // Registers and EFLAGS, live ones as parameters of `block`.
    let registers = |func: &mut Func, block: Block, live: &live::Set| -> Vec<Value> {
        fixed
            .iter()
            .map(|s| match *s {
                Source::Reg(r) if !live.reg(usize::from(r)) => unused[0],
                Source::Xmm(r) if !live.reg(8 + usize::from(r)) => unused[1],
                _ => func.param(block, s.ty()),
            })
            .collect()
    };
    let params = registers(&mut func, entry_block, &live.entry[0]);
    let mut entry_sources: Vec<Source> = fixed
        .iter()
        .copied()
        .filter(|s| match *s {
            Source::Reg(r) => live.entry[0].reg(usize::from(r)),
            Source::Xmm(r) => live.entry[0].reg(8 + usize::from(r)),
            _ => true,
        })
        .collect();
    let mut x87 = [None; 8];
    for i in (0..8u8).filter(|i| assumed.x87 && assumed.tags >> (2 * i) & 3 != 3) {
        let source = Source::X87(i);
        x87[usize::from(i)] = Some(func.param(entry_block, source.ty()));
        entry_sources.push(source);
    }
    // The last instruction pointer and opcode vary from job to job.
    let metadata = assumed.x87.then(|| {
        entry_sources.extend([Source::X87Ip, Source::X87Opcode]);
        [
            func.param(entry_block, Type::I32),
            func.param(entry_block, Type::I32),
        ]
    });
    // Frame loads and commits address entry ESP plus the lowest cell
    // offset, so that their immediate offsets are small and non-negative.
    let bias = cells.keys().next().copied().unwrap_or(0);
    let low = func
        .push(
            entry_block,
            Op::Constant(Type::I32, u128::from(bias as u32)),
        )
        .unwrap();
    let low = func
        .push(entry_block, Op::Int(IntOp::Add, params[ESP as usize], low))
        .unwrap();
    let base = func.push(entry_block, Op::Host(low)).unwrap();
    let entry_cells: Vec<Source> = cells
        .iter()
        .filter(|&(offset, _)| held[0].contains(offset))
        .map(|(&offset, &width)| Source::Frame { offset, width })
        .collect();
    let entry_frame: BTreeMap<i32, Value> = entry_cells
        .iter()
        .map(|s| {
            let Source::Frame { offset, .. } = *s else {
                unreachable!()
            };
            (offset, func.param(entry_block, s.ty()))
        })
        .collect();
    let entry_state = State {
        regs: params[..8].try_into().unwrap(),
        flags: params[8],
        xmm: params[9..17].try_into().unwrap(),
        x87,
        top: assumed.top,
        control: assumed.control,
        c1: Some(Location::Constant(u64::from(at.x87.status & 0x200 != 0))),
        ip: metadata.map_or(Location::Constant(u64::from(at.x87.ip)), |[ip, _]| {
            Location::At(ip)
        }),
        opcode: metadata.map_or(Location::Constant(u64::from(at.x87.opcode)), |[_, op]| {
            Location::At(op)
        }),
        floats: BTreeMap::new(),
        dirty: Default::default(),
        truncations: BTreeMap::new(),
        frame: entry_frame,
    };
    let states: Vec<State> = models
        .iter()
        .enumerate()
        .map(|(i, model)| {
            let block = func.block(None);
            func.blocks[block.0 as usize].eip = Some(unit.trace.insts[i].pc);
            let params = registers(&mut func, block, &live.entry[i]);
            let x87 = std::array::from_fn(|s| {
                (assumed.x87 && model.valid >> s & 1 != 0).then(|| func.param(block, Type::F64))
            });
            // A cell holding a binary64 the x87 stored on some arrival is
            // held as F64; where the x87 stored it on every arrival, the
            // value also serves float reads without a check.
            let float = |offset: i32| cells[&offset] == 8 && model.stored.contains(&offset);
            let frame: BTreeMap<i32, Value> = cells
                .iter()
                .filter(|&(offset, _)| held[i].contains(offset))
                .map(|(&offset, &width)| {
                    let ty = if float(offset) {
                        Type::F64
                    } else {
                        Source::Frame { offset, width }.ty()
                    };
                    (offset, func.param(block, ty))
                })
                .collect();
            let truncations = model
                .truncations
                .iter()
                .map(|&key| (key, func.param(block, Type::F64)))
                .collect();
            let floats = model
                .floats
                .iter()
                .map(|&p| match frame.get(&p) {
                    Some(&cell) if float(p) => (p, cell),
                    _ => (p, func.param(block, Type::F64)),
                })
                .collect();
            let ip = Location::At(func.param(block, Type::I32));
            let opcode = Location::At(func.param(block, Type::I32));
            let c1 = match model.c1 {
                model::C1::Constant(c) => Some(Location::Constant(u64::from(c))),
                model::C1::Round(op) => Some(Location::Recipe(Recipe::C1 {
                    op,
                    a: func.param(block, Type::F64),
                    b: func.param(block, Type::F64),
                })),
                model::C1::Unknown => None,
            };
            Ok(State {
                regs: params[..8].try_into().unwrap(),
                flags: params[8],
                xmm: params[9..17].try_into().unwrap(),
                x87,
                frame,
                floats,
                dirty: model.dirty.clone(),
                truncations,
                top: model.top,
                control: model.control.ok_or(Reject::Mode(i))?,
                c1,
                ip,
                opcode,
            })
        })
        .collect::<Result<_, Reject>>()?;
    func.head_exit = unit.trace.returns_at_head().then_some(Block(1));
    func.entry = entry_sources.into_iter().chain(entry_cells).collect();
    let mut f = Front {
        unit,
        proof,
        at,
        assumed,
        func,
        block: entry_block,
        index: 0,
        offsets,
        models,
        frame: frame.clone(),
        cells: cells.clone(),
        live,
        held,
        carried,
        base,
        bias,
        state: entry_state,
        loads: BTreeMap::new(),
        stores: BTreeMap::new(),
        pending_store: None,
    };
    let entry = f.record(unit.trace.insts[0].pc, unit.trace.owed(0), Start::Entry(0));
    f.func.blocks[0].head = Some(Head { record: entry });
    let args = f.arguments(0)?;
    f.func.end(
        entry_block,
        End::Jump(Target {
            block: Block(1),
            args,
            charge: 0,
        }),
    );
    // The region start in effect when each translated instruction ends.
    let mut regions: Vec<Option<RecordId>> = vec![None; unit.trace.len()];
    for (i, state) in states.into_iter().enumerate() {
        f.index = i;
        f.block = Block(i as u32 + 1);
        f.state = state;
        let inst = unit.trace.insts[i];
        // A head by the IR's definition: arrivals from later instructions
        // (loops), charged arrivals, or arrivals from different regions.
        // Other merges continue the region their arrivals share.
        let from = &arrivals[i];
        let mut region = from.first().and_then(|&p| regions[p]);
        if unit.trace.head(i)
            || from.iter().any(|&p| unit.trace.net(p, i) > 0)
            || from
                .iter()
                .any(|&p| regions[p].is_none() || regions[p] != region)
        {
            if f.state.c1.is_none() {
                return Err(Reject::StaticX87(i));
            }
            let record = f.record(inst.pc, unit.trace.owed(i), Start::Entry(i));
            f.func.blocks[i + 1].head = Some(Head { record });
            region = Some(record);
        }
        f.instruction()?;
        if let Some((width, addr, value)) = f.pending_store.take() {
            let after = f.record(inst.next_pc, unit.trace.unused(i), Start::After(i));
            let r = &mut f.func.records[after.0 as usize];
            if unit.trace.observe_out[i] == crate::translate::flags::FlagSet::NONE {
                r.eflags = None;
            }
            if !metadata_live(unit, i + 1) {
                if let Some(x87) = &mut r.x87 {
                    x87.c1 = None;
                    x87.ip = None;
                    x87.opcode = None;
                }
            } else if f.state.c1.is_none() {
                return Err(Reject::StaticX87(i));
            }
            f.stores.insert(after, i);
            region = Some(after);
            f.func.push(
                f.block,
                Op::Store {
                    width,
                    addr,
                    value,
                    after,
                },
            );
        }
        let fall = match inst.next {
            Next::Fall => Edge::Internal(i + 1),
            Next::Edge(edge) => edge,
            Next::Dynamic => return Err(Reject::Dynamic(i)),
        };
        let end = if let decode::Op::Jcc(Cc::Flags(cond), _) = inst.op {
            let cond = f.value(Op::Test {
                flags: f.state.flags,
                cond,
            });
            let taken = f.target(inst.taken.expect("trace resolves branches"))?;
            let fall = f.target(fall)?;
            End::Branch { cond, taken, fall }
        } else {
            End::Jump(f.target(fall)?)
        };
        f.func.end(f.block, end);
        regions[i] = region;
    }
    passes::run(&mut f.func, proof, &f.loads, &f.stores);
    if let Some(b) = (1..f.func.blocks.len()).find(|&b| {
        f.func.blocks[b]
            .params
            .iter()
            .any(|&p| f.func.ty(p) == Type::Flags)
    }) {
        return Err(Reject::Flags(b - 1));
    }
    debug_assert_eq!(super::verify::verify(&f.func), Ok(()));
    Ok((f.func, f.assumed))
}

impl Front<'_> {
    fn value(&mut self, op: Op) -> Value {
        self.func.push(self.block, op).unwrap()
    }
    fn constant(&mut self, ty: Type, bits: u128) -> Value {
        self.value(Op::Constant(ty, bits))
    }
    fn imm(&mut self, bits: u32) -> Value {
        self.constant(Type::I32, u128::from(bits))
    }
    fn int(&mut self, op: IntOp, a: Value, b: Value) -> Value {
        self.value(Op::Int(op, a, b))
    }
    fn bits(&mut self, to: Type, value: Value) -> Value {
        if self.func.ty(value) == to {
            value
        } else {
            self.value(Op::Bits { to, value })
        }
    }
    fn mask(&mut self, value: Value, width: u8) -> Value {
        if width == 4 {
            value
        } else {
            let mask = self.imm((1u32 << (width * 8)) - 1);
            self.int(IntOp::And, value, mask)
        }
    }
    fn address(&mut self, mem: Mem) -> Result<Value, Reject> {
        if mem.fs || mem.address_bits != 32 {
            return Err(Reject::Unsupported(self.index));
        }
        if mem
            .base
            .is_some_and(|r| self.offsets[self.index][r as usize].is_some())
            && self.frame_offset(mem).is_none()
        {
            return Err(Reject::Stack(self.index));
        }
        let mut addr = self.imm(mem.disp);
        if let Some(base) = mem.base {
            addr = self.int(IntOp::Add, addr, self.state.regs[base as usize]);
        }
        if let Some(index) = mem.index {
            let scale = self.imm(mem.scale);
            let index = self.int(IntOp::Mul, self.state.regs[index as usize], scale);
            addr = self.int(IntOp::Add, addr, index);
        }
        Ok(addr)
    }
    fn frame_offset(&self, mem: Mem) -> Option<i32> {
        frame_offset(mem, &self.offsets[self.index])
    }
    /// Whether any byte of the cell at `offset` is in `set`.
    fn cell_live(&self, set: &live::Set, offset: i32, width: u8) -> bool {
        (offset..offset + i32::from(width))
            .any(|p| self.frame.binary_search(&p).is_ok_and(|j| set.byte(j)))
    }
    /// The cells `[offset, offset + width)` covers, by offset and width.
    fn covered(&self, offset: i32, width: u8) -> Vec<(i32, u8)> {
        self.cells
            .range(offset..offset + i32::from(width))
            .map(|(&start, &w)| (start, w))
            .collect()
    }
    fn read(&mut self, mem: Mem, width: u8, ty: Type) -> Result<Value, Reject> {
        if let Some(offset) = self.frame_offset(mem) {
            if let Some(bits) = self.models[self.index].load(mem, width, &self.offsets[self.index])
            {
                return Ok(self.constant(ty, u128::from(bits)));
            }
            let integer = if width > 4 { Type::I64 } else { Type::I32 };
            let cells = self.covered(offset, width);
            if let [(start, w)] = cells[..]
                && (start, w) == (offset, width)
            {
                let value = self.cell(start);
                return Ok(self.bits(ty, value));
            }
            let mut out = self.constant(integer, 0);
            for (start, _) in cells {
                let part = self.cell(start);
                let part = self.bits(integer, part);
                let shift = self.constant(integer, (start - offset) as u128 * 8);
                let part = self.int(IntOp::Shl, part, shift);
                out = self.int(IntOp::Or, out, part);
            }
            Ok(self.bits(ty, out))
        } else {
            let addr = self.address(mem)?;
            let value = self.value(Op::Load { ty, width, addr });
            self.loads.insert(value, self.index);
            Ok(value)
        }
    }
    /// Whether block `i` holds the cell at `start` as F64: an x87 binary64
    /// store wrote it on some arrival.
    pub(super) fn float_cell(&self, i: usize, start: i32) -> bool {
        self.cells[&start] == 8
            && self.held[i].contains(&start)
            && self.models[i].stored.contains(&start)
    }
    /// A frame cell's value: the one held, or else memory's, which holds
    /// the cell's value whenever no value is held.
    fn cell(&mut self, start: i32) -> Value {
        if let Some(&value) = self.state.frame.get(&start) {
            return value;
        }
        let ty = Source::Frame {
            offset: start,
            width: self.cells[&start],
        }
        .ty();
        let value = self.cell_value(start, ty);
        self.state.frame.insert(start, value);
        value
    }
    /// As `cell`, as `ty` and without keeping a loaded value: an edge's
    /// arguments load on that edge only.
    pub(super) fn cell_value(&mut self, start: i32, ty: Type) -> Value {
        match self.state.frame.get(&start) {
            Some(&value) => self.bits(ty, value),
            None => self.value(Op::FrameLoad {
                ty,
                base: self.base,
                offset: start - self.bias,
            }),
        }
    }
    fn write(&mut self, mem: Mem, width: u8, value: Value) -> Result<(), Reject> {
        if let Some(offset) = self.frame_offset(mem) {
            let integer = if width > 4 { Type::I64 } else { Type::I32 };
            self.state
                .floats
                .retain(|&start, _| start + 8 <= offset || offset + i32::from(width) <= start);
            self.state.truncations.retain(|&(start, w), _| {
                start + i32::from(w) <= offset || offset + i32::from(width) <= start
            });
            let cells = self.covered(offset, width);
            if cells[..] == [(offset, 8)] && self.func.ty(value) == Type::F64 {
                self.state.frame.insert(offset, value);
                self.state.dirty.extend(offset..offset + 8);
                return Ok(());
            }
            let value = self.bits(integer, value);
            for (start, w) in cells {
                let shift = self.constant(integer, (start - offset) as u128 * 8);
                let part = self.int(IntOp::Shr, value, shift);
                let cell = if w > 4 { Type::I64 } else { Type::I32 };
                let part = self.bits(cell, part);
                let part = match w {
                    4 | 8 => part,
                    _ => {
                        let mask = self.constant(cell, (1u128 << (w * 8)) - 1);
                        self.int(IntOp::And, part, mask)
                    }
                };
                self.state.frame.insert(start, part);
                self.state.dirty.extend(start..start + i32::from(w));
            }
        } else {
            let addr = self.address(mem)?;
            self.pending_store = Some((width, addr, value));
        }
        Ok(())
    }
    /// The record of a region start, holding the components `at` keeps live
    /// that the form may have changed.
    fn record(&mut self, eip: u32, refund: u32, at: Start) -> RecordId {
        let live = match at {
            Start::Entry(i) => &self.live.entry[i],
            Start::After(i) => &self.live.after[i],
            Start::Exit => &self.live.all,
        };
        let held = |bit: usize, v: Value| {
            (live.reg(bit) && self.live.kept >> bit & 1 == 0).then_some(Location::At(v))
        };
        let record = IrRecord {
            eip,
            refund,
            regs: std::array::from_fn(|r| held(r, self.state.regs[r])),
            eflags: Some(Location::Recipe(Recipe::Eflags(self.state.flags))),
            xmm: std::array::from_fn(|r| held(8 + r, self.state.xmm[r])),
            x87: self.assumed.x87.then(|| X87 {
                top: self.state.top,
                valid: self
                    .state
                    .x87
                    .iter()
                    .enumerate()
                    .fold(0, |mask, (i, v)| mask | (u8::from(v.is_some()) << i)),
                control: self.state.control,
                stack: self.state.x87.map(|v| v.map(Location::At)),
                status: self.at.x87.status & !(0x3800 | 0x200),
                c1: self.state.c1,
                ip: Some(self.state.ip),
                opcode: Some(self.state.opcode),
            }),
            frame: self
                .state
                .frame
                .iter()
                .map(|(&offset, &value)| (offset, self.cells[&offset], value))
                .filter(|&(offset, width, _)| {
                    self.state.dirty.contains(&offset) && self.cell_live(live, offset, width)
                })
                .map(|(offset, width, value)| (offset, width, Location::At(value)))
                .collect(),
        };
        self.func.record(record)
    }
    /// The dirty cells outside `kept` that execution can read where `live`
    /// holds, with their values.
    fn commits(
        &self,
        kept: &std::collections::BTreeSet<i32>,
        live: &live::Set,
    ) -> Vec<(i32, Value)> {
        self.state
            .frame
            .iter()
            .filter(|&(c, _)| {
                let width = self.cells[c];
                !kept.contains(c)
                    && (*c..*c + i32::from(width)).any(|b| self.state.dirty.contains(&b))
                    && self.cell_live(live, *c, width)
            })
            .map(|(&c, &v)| (c, v))
            .collect()
    }
    fn commit(&mut self, commits: Vec<(i32, Value)>) {
        for (offset, value) in commits {
            self.func.push(
                self.block,
                Op::FrameStore {
                    base: self.base,
                    offset: offset - self.bias,
                    value,
                },
            );
        }
    }
    fn target(&mut self, edge: Edge) -> Result<Target, Reject> {
        match edge {
            Edge::Internal(i) => {
                let target = Target {
                    block: Block(i as u32 + 1),
                    args: Vec::new(),
                    charge: self.unit.trace.net(self.index, i) as i32,
                };
                // Dirty cells the head does not carry commit on this edge
                // alone, after validation (A64-OPT-3).
                let commits = match &self.carried[i] {
                    Some(kept) => self.commits(kept, &self.live.entry[i]),
                    None => Vec::new(),
                };
                if commits.is_empty() {
                    return Ok(Target {
                        args: self.arguments(i)?,
                        ..target
                    });
                }
                let from = std::mem::replace(&mut self.block, self.func.block(None));
                let args = self.arguments(i)?;
                self.commit(commits);
                let edge = std::mem::replace(&mut self.block, from);
                self.func.end(edge, End::Jump(Target { args, ..target }));
                Ok(Target {
                    block: edge,
                    args: Vec::new(),
                    charge: 0,
                })
            }
            Edge::Interpret(_) => Err(Reject::Interpret(self.index)),
            Edge::Exit(eip) => {
                if self.state.c1.is_none() {
                    return Err(Reject::StaticX87(self.index));
                }
                let block = self.func.block(None);
                // A cut instruction leaves at its own EIP without retiring.
                let inst = self.unit.trace.insts[self.index];
                let cut = inst.op == decode::Op::Nop && inst.pc == eip;
                let refund = self.unit.trace.unused(self.index) + u32::from(cut);
                let record = self.record(eip, refund, Start::Exit);
                self.func.end(block, End::Exit(record));
                Ok(Target {
                    block,
                    args: Vec::new(),
                    charge: 0,
                })
            }
        }
    }
}

fn frame_offset(mem: Mem, offsets: &[Option<i32>; 8]) -> Option<i32> {
    if mem.fs || mem.address_bits != 32 || mem.index.is_some() {
        return None;
    }
    Some(offsets[mem.base? as usize]?.wrapping_add(mem.disp as i32))
}

/// Register frame offsets at each instruction, the frame bytes accessed, and
/// the cells their accesses cut them into.
type Layout = (Vec<Offsets>, Vec<i32>, BTreeMap<i32, u8>);

fn layout(unit: &Unit, esp_low: u8) -> Result<Layout, Reject> {
    let trace = &unit.trace;
    let mut offsets = vec![None; trace.len()];
    let mut entry = [None; 8];
    entry[ESP as usize] = Some(0);
    offsets[0] = Some(entry);
    let mut slots = vec![BTreeMap::<i32, i32>::new(); trace.len()];
    let mut work = vec![0];
    let mut frame = std::collections::BTreeSet::new();
    let mut cuts = std::collections::BTreeSet::new();
    while let Some(i) = work.pop() {
        let mut regs = offsets[i].unwrap();
        let mut saved = slots[i].clone();
        let inst = trace.insts[i];
        let stack_mem = |offset| Mem {
            base: Some(ESP),
            index: None,
            scale: 1,
            address_bits: 32,
            disp: offset as u32,
            fs: false,
        };
        let mut access = |mem, width| {
            if let Some(start) = frame_offset(mem, &regs) {
                frame.extend(start..start + i32::from(width));
                cuts.extend([start, start + i32::from(width)]);
            }
        };
        match inst.op {
            decode::Op::Mov(dst, src) | decode::Op::Alu(_, dst, src) => {
                for s in [dst, src] {
                    if let Src::Mem(mem) = s {
                        access(mem, 4);
                    }
                }
            }
            decode::Op::Narrow(_, width, dst, src) => {
                for p in [dst, src] {
                    if let Part::Mem(mem) = p {
                        access(mem, width);
                    }
                }
            }
            decode::Op::Push(src) => {
                if let Src::Mem(mem) = src {
                    access(mem, 4);
                }
                access(stack_mem(-4), 4);
            }
            decode::Op::Call(_) => access(stack_mem(-4), 4),
            decode::Op::Pop(_) | decode::Op::Ret(_) => access(stack_mem(0), 4),
            decode::Op::Leave => access(
                Mem {
                    base: Some(EBP),
                    ..stack_mem(0)
                },
                4,
            ),
            decode::Op::X87(x) => {
                if let Some(mem) = x.mem {
                    use adv_cpu_core::floating::fast::{Fast, Source};
                    let width = match x.fast {
                        Fast::Load(Source::F64)
                        | Fast::Arith {
                            source: Source::F64,
                            ..
                        } => 8,
                        Fast::Store { width, .. } => width,
                        Fast::LoadControl | Fast::StoreControl => 2,
                        _ => 4,
                    };
                    access(mem, width);
                }
            }
            decode::Op::Fild64(mem, ..) => access(mem, 8),
            decode::Op::Simd(s) if s.memory() => access(s.mem, s.width),
            decode::Op::Integer(crate::translate::integer::Integer::Divide(Src::Mem(mem), ..)) => {
                access(mem, 4)
            }
            decode::Op::Parts(p) => {
                if let Some(mem) = p.memory() {
                    access(mem, p.width);
                }
            }
            decode::Op::Extend(_, decode::Narrow8or16::Mem(mem, word), _) => {
                access(mem, if word { 2 } else { 1 })
            }
            decode::Op::Imul(_, Src::Mem(mem), _)
            | decode::Op::Carry(_, _, Src::Mem(mem))
            | decode::Op::Shift(_, Src::Mem(mem), _)
            | decode::Op::StepMemory(_, mem) => access(mem, 4),
            decode::Op::NarrowStep(_, width, Part::Mem(mem)) => access(mem, width),
            decode::Op::Lea(..)
            | decode::Op::Jcc(..)
            | decode::Op::Jmp(..)
            | decode::Op::Nop
            | decode::Op::Cdq => {}
            _ => {}
        }
        match inst.op {
            decode::Op::Mov(Src::Reg(dst), Src::Reg(src)) => {
                if regs[src as usize].is_some() && ![ESP, EBP].contains(&dst) {
                    return Err(Reject::Escape(i));
                }
                regs[dst as usize] = regs[src as usize];
            }
            decode::Op::Mov(Src::Reg(dst), Src::Mem(mem)) => {
                let value = frame_offset(mem, &regs).and_then(|p| saved.get(&p).copied());
                if value.is_some() && ![ESP, EBP].contains(&dst) {
                    return Err(Reject::Escape(i));
                }
                regs[dst as usize] = value;
            }
            decode::Op::Mov(Src::Reg(dst), _) => regs[dst as usize] = None,
            decode::Op::Mov(Src::Mem(mem), src) => {
                let offset = frame_offset(mem, &regs);
                let value = if let Src::Reg(r) = src {
                    regs[r as usize]
                } else {
                    None
                };
                if value.is_some() && offset.is_none() {
                    return Err(Reject::Escape(i));
                }
                if let Some(p) = offset {
                    saved.retain(|&s, _| s + 4 <= p || p + 4 <= s);
                    if let Some(v) = value {
                        saved.insert(p, v);
                    }
                }
            }
            decode::Op::Lea(dst, mem) => {
                let offset = frame_offset(mem, &regs);
                if offset.is_some() && ![ESP, EBP].contains(&dst) {
                    return Err(Reject::Escape(i));
                }
                regs[dst as usize] = offset;
            }
            decode::Op::Alu(op, Src::Reg(dst), Src::Imm(n)) if regs[dst as usize].is_some() => {
                use decode::Alu::*;
                let offset = regs[dst as usize].unwrap();
                regs[dst as usize] = match op {
                    Add => Some(offset.wrapping_add(n as i32)),
                    Sub => Some(offset.wrapping_sub(n as i32)),
                    And if dst == ESP
                        && n.wrapping_neg().is_power_of_two()
                        && n.wrapping_neg() <= 16 =>
                    {
                        let before = i32::from(esp_low).wrapping_add(offset);
                        Some((before & n as i32).wrapping_sub(i32::from(esp_low)))
                    }
                    Cmp | Test => Some(offset),
                    _ => return Err(Reject::Stack(i)),
                };
            }
            decode::Op::Push(src) => {
                let p = regs[ESP as usize].ok_or(Reject::Stack(i))? - 4;
                saved.retain(|&s, _| s + 4 <= p || p + 4 <= s);
                if let Src::Reg(r) = src
                    && let Some(v) = regs[r as usize]
                {
                    saved.insert(p, v);
                }
                regs[ESP as usize] = Some(p);
            }
            decode::Op::Call(_) => {
                let p = regs[ESP as usize].ok_or(Reject::Stack(i))? - 4;
                saved.remove(&p);
                regs[ESP as usize] = Some(p);
            }
            decode::Op::Pop(dst) => {
                let p = regs[ESP as usize].ok_or(Reject::Stack(i))?;
                let value = saved.get(&p).copied();
                if value.is_some() && ![ESP, EBP].contains(&dst) {
                    return Err(Reject::Escape(i));
                }
                regs[ESP as usize] = Some(p + 4);
                regs[dst as usize] = value;
            }
            decode::Op::Ret(n) => {
                regs[ESP as usize] =
                    Some(regs[ESP as usize].ok_or(Reject::Stack(i))? + 4 + i32::from(n))
            }
            decode::Op::Leave => {
                let p = regs[EBP as usize].ok_or(Reject::Stack(i))?;
                regs[ESP as usize] = Some(p + 4);
                regs[EBP as usize] = saved.get(&p).copied();
            }
            decode::Op::Alu(op, Src::Reg(dst), _)
                if !matches!(op, decode::Alu::Cmp | decode::Alu::Test) =>
            {
                regs[dst as usize] = None
            }
            decode::Op::Step(up, dst) => {
                regs[dst as usize] = regs[dst as usize].map(|p| if up { p + 1 } else { p - 1 });
            }
            decode::Op::Neg(dst)
            | decode::Op::Not(dst)
            | decode::Op::Imul(dst, ..)
            | decode::Op::Extend(dst, ..)
            | decode::Op::Carry(_, dst, _) => regs[dst as usize] = None,
            decode::Op::Shift(_, Src::Reg(dst), _) => regs[dst as usize] = None,
            decode::Op::Narrow(alu, width, dst, _)
                if !matches!(alu, Some(decode::Alu::Cmp | decode::Alu::Test)) =>
            {
                match dst {
                    Part::Reg(r) => {
                        let r = if width == 1 && r >= 4 { r - 4 } else { r };
                        regs[r as usize] = None;
                    }
                    Part::Mem(mem) => {
                        if let Some(p) = frame_offset(mem, &regs) {
                            saved.retain(|&s, _| s + 4 <= p || p + i32::from(width) <= s);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        let overwritten = match inst.op {
            decode::Op::X87(x) => {
                if let adv_cpu_core::floating::fast::Fast::Store { width, .. } = x.fast {
                    x.mem.map(|m| (m, width))
                } else {
                    None
                }
            }
            decode::Op::Simd(s) if s.dst == crate::translate::simd::Loc::Mem => {
                Some((s.mem, s.width))
            }
            decode::Op::Alu(op, Src::Mem(mem), _)
                if !matches!(op, decode::Alu::Cmp | decode::Alu::Test) =>
            {
                Some((mem, 4))
            }
            decode::Op::StepMemory(_, mem) => Some((mem, 4)),
            decode::Op::NarrowStep(_, width, Part::Mem(mem)) => Some((mem, width)),
            _ => None,
        };
        if let Some((mem, width)) = overwritten
            && let Some(p) = frame_offset(mem, &offsets[i].unwrap())
        {
            saved.retain(|&s, _| s + 4 <= p || p + i32::from(width) <= s);
        }
        if let decode::Op::Simd(s) = inst.op
            && let crate::translate::simd::Loc::Gpr(r) = s.dst
        {
            regs[r as usize] = None;
        }
        if regs[ESP as usize].is_none() {
            return Err(Reject::Stack(i));
        }
        let fall = match inst.next {
            Next::Fall => Some(Edge::Internal(i + 1)),
            Next::Edge(e) => Some(e),
            Next::Dynamic => None,
        };
        for edge in [fall, inst.taken].into_iter().flatten() {
            let Edge::Internal(to) = edge else {
                continue;
            };
            if let Some(old) = offsets[to] {
                if old != regs {
                    return Err(Reject::Stack(to));
                }
                let before = slots[to].clone();
                slots[to].retain(|p, v| saved.get(p) == Some(v));
                if slots[to] != before {
                    work.push(to);
                }
            } else {
                offsets[to] = Some(regs);
                slots[to] = saved.clone();
                work.push(to);
            }
        }
    }
    // A cell runs from a cut or a gap to the next one.
    let mut cells = BTreeMap::new();
    let mut open: Option<i32> = None;
    for &p in &frame {
        if let Some(start) = open
            && (cuts.contains(&p) || !frame.contains(&(p - 1)))
        {
            cells.insert(start, (p - start) as u8);
            open = None;
        }
        open.get_or_insert(p);
        if !frame.contains(&(p + 1)) {
            let start = open.take().unwrap();
            cells.insert(start, (p + 1 - start) as u8);
        }
    }
    Ok((
        offsets.into_iter().map(|r| r.unwrap_or(entry)).collect(),
        frame.into_iter().collect(),
        cells,
    ))
}

fn metadata_live(unit: &Unit, start: usize) -> bool {
    let mut seen = vec![false; unit.trace.len()];
    let mut work = vec![start];
    while let Some(i) = work.pop() {
        if i >= seen.len() {
            return true;
        }
        if std::mem::replace(&mut seen[i], true) {
            continue;
        }
        if unit.trace.head(i) || unit.trace.merge[i] {
            return true;
        }
        let inst = unit.trace.insts[i];
        match inst.op {
            decode::Op::Fild64(..) => return true,
            decode::Op::X87(x) => {
                use adv_cpu_core::floating::fast::Fast;
                if x.mem.is_some() {
                    return true;
                }
                if !matches!(x.fast, Fast::LoadControl | Fast::StoreControl) {
                    continue;
                }
            }
            _ => {}
        }
        let fall = match inst.next {
            Next::Fall => Some(Edge::Internal(i + 1)),
            Next::Edge(e) => Some(e),
            Next::Dynamic => return true,
        };
        for e in [fall, inst.taken].into_iter().flatten() {
            match e {
                Edge::Internal(t) => work.push(t),
                _ => return true,
            }
        }
    }
    false
}
