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

mod integer;
mod model;
mod passes;
mod simd;
#[cfg(test)]
mod tests;
mod x87;

pub(crate) struct Built {
    pub func: Func,
    pub assumed: Assumed,
}

/// Entry facts checked by admission before selecting this form.
#[derive(Debug)]
pub(crate) struct Assumed {
    pub control: u16,
    pub status: u16,
    pub ip: u32,
    pub opcode: u16,
    pub mxcsr: u32,
    pub top: u8,
    pub tags: u16,
    pub esp_low: u8,
    pub denormal: bool,
    pub frame: Option<(i32, i32)>,
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
    frame: BTreeMap<i32, Value>,
}

impl State {
    fn values(&self) -> Vec<Value> {
        self.regs
            .iter()
            .copied()
            .chain([self.flags])
            .chain(self.xmm)
            .chain(self.x87.into_iter().flatten())
            .chain(self.frame.values().copied())
            .collect()
    }
}

type Offsets = [Option<i32>; 8];

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
    state: State,
    loads: BTreeMap<Value, usize>,
    stores: BTreeMap<RecordId, usize>,
    pending_store: Option<(u8, Value, Value)>,
}

pub(crate) fn build(unit: &Unit, proof: &Proof, at: &CpuState) -> Result<Built, Reject> {
    if unit.trace.insts.is_empty() {
        return Err(Reject::Empty);
    }
    let assumed = Assumed {
        control: at.x87.control,
        status: at.x87.status & !0x3800,
        ip: at.x87.ip,
        opcode: at.x87.opcode,
        mxcsr: at.mxcsr,
        top: at.x87.top() as u8,
        tags: at.x87.tag,
        esp_low: (at.regs[ESP as usize] & 15) as u8,
        denormal: at.x87.status & 2 != 0,
        frame: None,
    };
    let (offsets, frame) = layout(unit, assumed.esp_low)?;
    let models = model::analyze(unit, at, &offsets)?;
    let words: Vec<i32> = frame
        .iter()
        .map(|p| p & !3)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let common: Vec<Source> = (0..8)
        .map(Source::Reg)
        .chain([Source::Eflags])
        .chain((0..8).map(Source::Xmm))
        .chain(
            (0..8)
                .filter(|i| assumed.tags >> (2 * i) & 3 != 3)
                .map(Source::X87),
        )
        .collect();
    let mut func = Func::default();
    let entry_block = func.block(None);
    let params: Vec<Value> = common
        .iter()
        .map(|s| func.param(entry_block, s.ty()))
        .collect();
    let mut x87 = [None; 8];
    for (&source, &p) in common[17..].iter().zip(&params[17..]) {
        let Source::X87(i) = source else {
            unreachable!()
        };
        x87[i as usize] = Some(p);
    }
    let words: BTreeMap<i32, Value> = words
        .iter()
        .map(|&p| (p, func.param(entry_block, Type::I32)))
        .collect();
    let entry_state = State {
        regs: params[..8].try_into().unwrap(),
        flags: params[8],
        xmm: params[9..17].try_into().unwrap(),
        x87,
        top: assumed.top,
        control: assumed.control,
        c1: Some(Location::Constant(u64::from(at.x87.status & 0x200 != 0))),
        ip: Location::Constant(u64::from(at.x87.ip)),
        opcode: Location::Constant(u64::from(at.x87.opcode)),
        floats: BTreeMap::new(),
        dirty: Default::default(),
        truncations: BTreeMap::new(),
        frame: BTreeMap::new(),
    };
    let states: Vec<State> = models
        .iter()
        .enumerate()
        .map(|(i, model)| {
            let block = func.block(None);
            let params: Vec<Value> = common[..17]
                .iter()
                .map(|s| func.param(block, s.ty()))
                .collect();
            let x87 = std::array::from_fn(|s| {
                (model.valid >> s & 1 != 0).then(|| func.param(block, Type::F64))
            });
            let frame = frame
                .iter()
                .map(|&p| (p, func.param(block, Type::I32)))
                .collect();
            let truncations = model
                .truncations
                .iter()
                .map(|&key| (key, func.param(block, Type::F64)))
                .collect();
            let floats = model
                .floats
                .iter()
                .map(|&p| (p, func.param(block, Type::F64)))
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
    func.entry = common
        .into_iter()
        .chain(
            words
                .keys()
                .copied()
                .map(|offset| Source::Frame { offset, width: 4 }),
        )
        .collect();
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
        state: entry_state,
        loads: BTreeMap::new(),
        stores: BTreeMap::new(),
        pending_store: None,
    };
    if let (Some(&lo), Some(&hi)) = (frame.first(), frame.last()) {
        f.assumed.frame = Some((lo & !3, (hi & !3) + 4));
    }
    let entry = f.record(unit.trace.insts[0].pc, unit.trace.owed(0));
    f.func.blocks[0].head = Some(Head { record: entry });
    for &p in &frame {
        let shift = f.imm(((p & 3) * 8) as u32);
        let byte = f.int(IntOp::Shr, words[&(p & !3)], shift);
        let byte = f.mask(byte, 1);
        f.state.frame.insert(p, byte);
    }
    let args = f.arguments(0)?;
    f.func.end(
        entry_block,
        End::Jump(Target {
            block: Block(1),
            args,
            charge: 0,
        }),
    );
    for (i, state) in states.into_iter().enumerate() {
        f.index = i;
        f.block = Block(i as u32 + 1);
        f.state = state;
        let inst = unit.trace.insts[i];
        if unit.trace.head(i)
            || unit.trace.merge[i]
            || unit.trace.insts.iter().enumerate().any(|(from, ins)| {
                let fall = match ins.next {
                    Next::Edge(e) => Some(e),
                    Next::Fall => Some(Edge::Internal(from + 1)),
                    _ => None,
                };
                [fall, ins.taken].into_iter().flatten().any(
                    |e| matches!(e, Edge::Internal(to) if to == i && unit.trace.net(from, to) > 0),
                )
            })
        {
            if f.state.c1.is_none() {
                return Err(Reject::StaticX87(i));
            }
            let record = f.record(inst.pc, unit.trace.owed(i));
            f.func.blocks[i + 1].head = Some(Head { record });
        }
        f.instruction()?;
        if let Some((width, addr, value)) = f.pending_store.take() {
            let after = f.record(inst.next_pc, unit.trace.unused(i));
            if !metadata_live(unit, i + 1) {
                let r = &mut f.func.records[after.0 as usize];
                r.x87.c1 = None;
                r.x87.ip = None;
                r.x87.opcode = None;
            } else if f.state.c1.is_none() {
                return Err(Reject::StaticX87(i));
            }
            f.stores.insert(after, i);
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
    }
    passes::run(&mut f.func, proof, &f.loads, &f.stores);
    debug_assert_eq!(super::verify::verify(&f.func), Ok(()));
    Ok(Built {
        func: f.func,
        assumed: f.assumed,
    })
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
    fn read(&mut self, mem: Mem, width: u8, ty: Type) -> Result<Value, Reject> {
        if let Some(offset) = self.frame_offset(mem) {
            if let Some(bits) = self.models[self.index].load(mem, width, &self.offsets[self.index])
            {
                return Ok(self.constant(ty, u128::from(bits)));
            }
            let integer = if width > 4 { Type::I64 } else { Type::I32 };
            let mut out = self.constant(integer, 0);
            for byte in 0..i32::from(width) {
                let pos = offset + byte;
                let part = self.state.frame[&pos];
                let part = self.bits(integer, part);
                let shift = self.constant(integer, byte as u128 * 8);
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
    fn write(&mut self, mem: Mem, width: u8, value: Value) -> Result<(), Reject> {
        if let Some(offset) = self.frame_offset(mem) {
            let integer = if width > 4 { Type::I64 } else { Type::I32 };
            self.state
                .floats
                .retain(|&start, _| start + 8 <= offset || offset + i32::from(width) <= start);
            self.state.truncations.retain(|&(start, w), _| {
                start + i32::from(w) <= offset || offset + i32::from(width) <= start
            });
            let value = self.bits(integer, value);
            for byte in 0..i32::from(width) {
                let pos = offset + byte;
                let shift = self.constant(integer, byte as u128 * 8);
                let part = self.int(IntOp::Shr, value, shift);
                let part = self.bits(Type::I32, part);
                let part = self.mask(part, 1);
                self.state.frame.insert(pos, part);
                self.state.dirty.insert(pos);
            }
        } else {
            let addr = self.address(mem)?;
            self.pending_store = Some((width, addr, value));
        }
        Ok(())
    }
    fn record(&mut self, eip: u32, refund: u32) -> RecordId {
        self.func.record(IrRecord {
            eip,
            refund,
            regs: self.state.regs.map(|v| Some(Location::At(v))),
            eflags: Some(Location::Recipe(Recipe::Eflags(self.state.flags))),
            xmm: self.state.xmm.map(|v| Some(Location::At(v))),
            x87: X87 {
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
            },
            frame: self
                .state
                .frame
                .iter()
                .filter(|(offset, _)| self.state.dirty.contains(offset))
                .map(|(&offset, &value)| (offset, 1, Location::At(value)))
                .collect(),
        })
    }
    fn target(&mut self, edge: Edge) -> Result<Target, Reject> {
        match edge {
            Edge::Internal(i) => Ok(Target {
                block: Block(i as u32 + 1),
                args: self.arguments(i)?,
                charge: self.unit.trace.net(self.index, i) as i32,
            }),
            Edge::Interpret(_) => Err(Reject::Interpret(self.index)),
            Edge::Exit(eip) => {
                if self.state.c1.is_none() {
                    return Err(Reject::StaticX87(self.index));
                }
                let block = self.func.block(None);
                let record = self.record(eip, self.unit.trace.unused(self.index));
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

fn layout(unit: &Unit, esp_low: u8) -> Result<(Vec<Offsets>, Vec<i32>), Reject> {
    let trace = &unit.trace;
    let mut offsets = vec![None; trace.len()];
    let mut entry = [None; 8];
    entry[ESP as usize] = Some(0);
    offsets[0] = Some(entry);
    let mut slots = vec![BTreeMap::<i32, i32>::new(); trace.len()];
    let mut work = vec![0];
    let mut frame = std::collections::BTreeSet::new();
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
                for pos in start..start + i32::from(width) {
                    frame.insert(pos);
                }
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
    Ok((
        offsets.into_iter().map(|r| r.unwrap_or(entry)).collect(),
        frame.into_iter().collect(),
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
