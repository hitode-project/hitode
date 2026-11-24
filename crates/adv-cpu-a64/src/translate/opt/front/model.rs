//! Constants proven on every arrival, independently of SSA identities.
use super::{
    BTreeMap, Cc, CpuState, EBP, ESP, Edge, FpOp, M, Mem, Next, Part, Reject, Src, Unit, decode,
    frame_offset,
};
use adv_cpu_core::floating::fast::{Fast, Op as Arithmetic};
use std::collections::BTreeSet;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum C1 {
    Constant(bool),
    Round(FpOp, bool),
    Value,
}
/// The frame sets are shared between the models of consecutive
/// instructions and copied only where one changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Model {
    pub top: u8,
    pub valid: u8,
    pub control: Option<u16>,
    pub c1: C1,
    pub floats: Rc<BTreeSet<i32>>,
    /// Frame positions where an x87 binary64 store on some path wrote the
    /// bytes last: the cell is best held as F64 there.
    pub stored: Rc<BTreeSet<i32>>,
    pub dirty: Rc<BTreeSet<i32>>,
    regs: [(u32, u32); 8],
    mxcsr: u32,
    /// Known-bit lattice `(bits, known)` for MMX alias metadata (bits 0–7
    /// written mask, 8–9 tag state, 10 TOP=0).
    pub mmx_meta: (u32, u32),
    /// Known-bit lattice `(bits, known)` per frame byte.
    bytes: Rc<BTreeMap<i32, (u8, u8)>>,
    flags: Option<u32>,
}

fn retain<T: Ord + Clone>(set: &mut Rc<BTreeSet<T>>, keep: impl Fn(&T) -> bool) {
    if !set.iter().all(&keep) {
        Rc::make_mut(set).retain(keep);
    }
}

fn extend<T: Ord + Clone>(set: &mut Rc<BTreeSet<T>>, from: &Rc<BTreeSet<T>>) {
    if !Rc::ptr_eq(set, from) && !from.is_subset(set) {
        Rc::make_mut(set).extend(from.iter().cloned());
    }
}

/// Clears the frame facts a write of `width` bytes at `p` overlaps.
fn overwrite(m: &mut Model, p: i32, width: u8) {
    let apart = |start: i32, w: u8| start + i32::from(w) <= p || p + i32::from(width) <= start;
    retain(&mut m.floats, |&start| apart(start, 8));
    retain(&mut m.stored, |&start| apart(start, 8));
    if !(p..p + i32::from(width)).all(|b| m.dirty.contains(&b)) {
        Rc::make_mut(&mut m.dirty).extend(p..p + i32::from(width));
    }
}

pub(super) struct Analysis {
    pub models: Vec<Model>,
    pub reachable: Vec<bool>,
    pub branch: Vec<Option<bool>>,
}

/// `kept` gives, for a loop head whose arrivals commit frame cells, the
/// frame bytes that may stay dirty on arrival (A64-OPT-3).
pub(super) fn analyze(
    unit: &Unit,
    at: &CpuState,
    offsets: &[[Option<i32>; 8]],
    kept: &[Option<BTreeSet<i32>>],
    write_through: bool,
) -> Result<Analysis, Reject> {
    let entry = Model {
        top: at.x87.top() as u8,
        valid: (0..8).fold(0, |v, i| v | (u8::from(at.x87.physical_tag(i) != 3) << i)),
        control: Some(at.x87.control),
        c1: C1::Constant(at.x87.status & 0x200 != 0),
        floats: Default::default(),
        stored: Default::default(),
        dirty: Default::default(),
        regs: [(0, 0); 8],
        mxcsr: at.mxcsr,
        mmx_meta: (0, u32::MAX),
        bytes: Default::default(),
        flags: None,
    };
    let mut states = vec![None; unit.trace.len()];
    let mut branches = vec![None; unit.trace.len()];
    states[0] = Some(entry.clone());
    let mut work = vec![0];
    while let Some(i) = work.pop() {
        let mut m = states[i].clone().unwrap();
        let inst = unit.trace.insts[i];
        if inst.effects.writes != crate::translate::front::flags::FlagSet::NONE {
            m.flags = None;
        }
        m.step(inst.op, &offsets[i], i)?;
        if matches!(inst.op, decode::Op::Call(_)) {
            m.write(
                Part::Mem(Mem {
                    base: Some(ESP),
                    index: None,
                    scale: 1,
                    address_bits: 32,
                    disp: (-4i32) as u32,
                    fs: false,
                }),
                4,
                Some(inst.next_pc),
                &offsets[i],
            );
        }
        if write_through {
            m.dirty = Default::default();
        }
        let branch = match inst.op {
            decode::Op::Jcc(Cc::Flags(cc), _) => m
                .flags
                .map(|flags| adv_cpu_core::integer::condition_true(cc, flags)),
            _ => None,
        };
        branches[i] = branch;
        let fall = match inst.next {
            Next::Fall => Some(Edge::Internal(i + 1)),
            Next::Edge(e) => Some(e),
            _ => None,
        };
        for (edge, taken) in [(fall, false), (inst.taken, true)] {
            if branch.is_some_and(|b| b != taken) {
                continue;
            }
            let Some(Edge::Internal(to)) = edge else {
                continue;
            };
            let mut committed;
            let m = match kept.get(to) {
                Some(Some(kept)) => {
                    committed = m.clone();
                    retain(&mut committed.dirty, |b| kept.contains(b));
                    &committed
                }
                _ => &m,
            };
            let changed = if let Some(old) = &mut states[to] {
                if old.top != m.top || old.valid != m.valid {
                    return Err(Reject::StaticX87(to));
                }
                let before = old.clone();
                if old.control != m.control {
                    old.control = None;
                }
                for (a, b) in old.regs.iter_mut().zip(m.regs) {
                    a.1 &= b.1 & !(a.0 ^ b.0);
                    a.0 &= a.1;
                }
                old.mmx_meta.1 &= m.mmx_meta.1 & !(old.mmx_meta.0 ^ m.mmx_meta.0);
                old.mmx_meta.0 &= old.mmx_meta.1;
                retain(&mut old.floats, |p| m.floats.contains(p));
                extend(&mut old.stored, &m.stored);
                extend(&mut old.dirty, &m.dirty);
                if !old.bytes.iter().all(|(p, v)| m.bytes.get(p) == Some(v)) {
                    let bytes = Rc::make_mut(&mut old.bytes);
                    bytes.retain(|p, (v1, k1)| {
                        let Some(&(v2, k2)) = m.bytes.get(p) else {
                            return false;
                        };
                        let k = *k1 & k2 & !(*v1 ^ v2);
                        *v1 &= k;
                        *k1 = k;
                        k != 0
                    });
                }
                if old.flags != m.flags {
                    old.flags = None;
                }
                old.c1 = merge_c1(old.c1, m.c1, to)?;
                *old != before
            } else {
                states[to] = Some(m.clone());
                true
            };
            if changed {
                work.push(to);
            }
        }
    }
    let reachable = states.iter().map(Option::is_some).collect();
    let models = states
        .into_iter()
        .map(|m| m.unwrap_or_else(|| entry.clone()))
        .collect();
    Ok(Analysis {
        models,
        reachable,
        branch: branches,
    })
}

fn merge_c1(a: C1, b: C1, i: usize) -> Result<C1, Reject> {
    Ok(match (a, b) {
        (a, b) if a == b => a,
        (C1::Round(..), C1::Round(..)) => return Err(Reject::StaticX87(i)),
        (round @ C1::Round(..), _) | (_, round @ C1::Round(..)) => round,
        _ => C1::Value,
    })
}

impl Model {
    pub(super) fn load(&self, mem: Mem, width: u8, offsets: &[Option<i32>; 8]) -> Option<u64> {
        let offset = frame_offset(mem, offsets)?;
        (0..i32::from(width)).try_fold(0, |bits, i| {
            let &(v, k) = self.bytes.get(&(offset + i))?;
            (k == u8::MAX).then_some(bits | (u64::from(v) << (i * 8)))
        })
    }
    fn read_bits(&self, src: Part, width: u8, offsets: &[Option<i32>; 8]) -> (u32, u32) {
        let width_mask = adv_cpu_core::integer::mask(width);
        let (bits, known) = match src {
            Part::Imm(n) => (n, u32::MAX),
            Part::Reg(r) => {
                let (r, shift) = if width == 1 && r >= 4 {
                    (r - 4, 8)
                } else {
                    (r, 0)
                };
                let (bits, known) = self.regs[r as usize];
                (bits >> shift, known >> shift)
            }
            Part::Mem(mem) => {
                let Some(offset) = frame_offset(mem, offsets) else {
                    return (0, 0);
                };
                let mut bits = 0u32;
                let mut known = 0u32;
                for i in 0..i32::from(width) {
                    if let Some(&(v, k)) = self.bytes.get(&(offset + i)) {
                        bits |= u32::from(v & k) << (i * 8);
                        known |= u32::from(k) << (i * 8);
                    }
                }
                (bits, known)
            }
        };
        (bits & known & width_mask, known & width_mask)
    }
    pub(super) fn read(&self, src: Part, width: u8, offsets: &[Option<i32>; 8]) -> Option<u32> {
        let mask = adv_cpu_core::integer::mask(width);
        let (bits, known) = self.read_bits(src, width, offsets);
        (known & mask == mask).then_some(bits & mask)
    }
    fn write_bits(
        &mut self,
        dst: Part,
        width: u8,
        bits: u32,
        known: u32,
        offsets: &[Option<i32>; 8],
    ) {
        match dst {
            Part::Reg(r) => {
                let (r, shift) = if width == 1 && r >= 4 {
                    (r - 4, 8)
                } else {
                    (r, 0)
                };
                let mask = adv_cpu_core::integer::mask(width) << shift;
                let (rb, rk) = &mut self.regs[r as usize];
                *rb = *rb & !mask | ((bits & known) << shift) & mask;
                *rk = *rk & !mask | (known << shift) & mask;
            }
            Part::Mem(mem) => {
                if let Some(p) = frame_offset(mem, offsets) {
                    overwrite(self, p, width);
                    let byte = |b: i32| {
                        if b >= 4 {
                            return None;
                        }
                        let k = (known >> (b * 8)) as u8;
                        (k != 0).then_some(((bits >> (b * 8)) as u8 & k, k))
                    };
                    if (0..i32::from(width)).any(|b| self.bytes.get(&(p + b)).copied() != byte(b)) {
                        let bytes = Rc::make_mut(&mut self.bytes);
                        for b in 0..i32::from(width) {
                            match byte(b) {
                                Some(x) => bytes.insert(p + b, x),
                                None => bytes.remove(&(p + b)),
                            };
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn write(&mut self, dst: Part, width: u8, value: Option<u32>, offsets: &[Option<i32>; 8]) {
        let (bits, known) = match value {
            Some(v) => (v, u32::MAX),
            None => (0, 0),
        };
        self.write_bits(dst, width, bits, known, offsets);
    }
    fn step(&mut self, op: decode::Op, offsets: &[Option<i32>; 8], i: usize) -> Result<(), Reject> {
        use crate::translate::front::forms::Integer;
        use decode::Op as D;
        let part = |s| match s {
            Src::Reg(r) => Part::Reg(r),
            Src::Imm(n) => Part::Imm(n),
            Src::Mem(m) => Part::Mem(m),
        };
        match op {
            D::Mov(dst, src) => {
                let (bits, known) = self.read_bits(part(src), 4, offsets);
                self.write_bits(part(dst), 4, bits, known, offsets);
            }
            D::Narrow(None, width, dst, src) => {
                let (bits, known) = self.read_bits(src, width, offsets);
                self.write_bits(dst, width, bits, known, offsets);
            }
            D::Alu(alu, dst, src) => self.alu(alu, part(dst), part(src), 4, offsets),
            D::Narrow(Some(alu), width, dst, src) => self.alu(alu, dst, src, width, offsets),
            D::X87(x) => {
                let slot = |index: u8| (self.top + index) & 7;
                let need = |s: u8| {
                    if self.valid & (1 << s) == 0 {
                        Err(Reject::StaticX87(i))
                    } else {
                        Ok(())
                    }
                };
                match x.fast {
                    Fast::Load(_) | Fast::Constant(_) => {
                        let top = self.top.wrapping_sub(1) & 7;
                        if self.valid & (1 << top) != 0 {
                            return Err(Reject::StaticX87(i));
                        }
                        self.top = top;
                        self.valid |= 1 << top;
                        self.c1 = C1::Constant(false);
                    }
                    Fast::Store {
                        pop,
                        integer,
                        width,
                    } => {
                        need(self.top)?;
                        if pop {
                            self.valid &= !(1 << self.top);
                            self.top = (self.top + 1) & 7;
                        }
                        // Chopping never rounds the magnitude up.
                        let chop = self.control.is_some_and(|c| c & 0x0c00 == 0x0c00);
                        self.c1 = if integer && !chop || !integer && width == 4 {
                            C1::Value
                        } else {
                            C1::Constant(false)
                        };
                        if let Some(mem) = x.mem
                            && let Some(p) = frame_offset(mem, offsets)
                        {
                            overwrite(self, p, width);
                            if self.bytes.range(p..p + i32::from(width)).next().is_some() {
                                let bytes = Rc::make_mut(&mut self.bytes);
                                for b in 0..i32::from(width) {
                                    bytes.remove(&(p + b));
                                }
                            }
                            if !integer && width == 8 {
                                Rc::make_mut(&mut self.floats).insert(p);
                                Rc::make_mut(&mut self.stored).insert(p);
                            }
                        }
                    }
                    Fast::StoreSt { index, pop } => {
                        need(self.top)?;
                        self.valid |= 1 << slot(index);
                        if pop {
                            self.valid &= !(1 << self.top);
                            self.top = (self.top + 1) & 7;
                        }
                        self.c1 = C1::Constant(false);
                    }
                    Fast::Arith {
                        op,
                        dest,
                        source,
                        pop,
                        ..
                    } => {
                        need(slot(dest))?;
                        if let adv_cpu_core::floating::fast::Source::St(s) = source {
                            need(slot(s))?;
                        }
                        self.c1 = C1::Round(fp(op), self.control.is_some_and(|c| c & 0x300 == 0));
                        if pop {
                            self.valid &= !(1 << self.top);
                            self.top = (self.top + 1) & 7;
                        }
                    }
                    Fast::Exchange(index) => {
                        need(self.top)?;
                        need(slot(index))?;
                        self.c1 = C1::Constant(false);
                    }
                    Fast::Negate | Fast::Absolute => {
                        need(self.top)?;
                        self.c1 = C1::Constant(false);
                    }
                    Fast::StoreControl => {
                        let mem = x.mem.ok_or(Reject::Unsupported(i))?;
                        self.write(Part::Mem(mem), 2, self.control.map(u32::from), offsets);
                    }
                    Fast::LoadControl => {
                        let mem = x.mem.ok_or(Reject::Unsupported(i))?;
                        self.control = self.load(mem, 2, offsets).map(|n| n as u16);
                    }
                    _ => {}
                }
            }
            D::Fild64(..) => {
                let top = self.top.wrapping_sub(1) & 7;
                if self.valid & (1 << top) != 0 {
                    return Err(Reject::StaticX87(i));
                }
                self.top = top;
                self.valid |= 1 << top;
                self.c1 = C1::Constant(false);
            }
            D::Simd(s) if s.kind == crate::translate::front::forms::SimdKind::StoreControl => {
                // Control bits (0xffc0) are invariant; sticky bits (0x3f) are dynamic.
                self.write_bits(Part::Mem(s.mem), 4, self.mxcsr & !0x3f, !0x3f, offsets);
            }
            D::Simd(s) => {
                if s.kind == crate::translate::front::forms::SimdKind::Emms {
                    self.mmx_meta.0 = (self.mmx_meta.0 & !0x300) | 0x200;
                    self.mmx_meta.1 |= 0x300;
                } else if s.mmx() {
                    let write_bit = if let crate::translate::front::forms::Loc::Mmx(n) = s.dst {
                        1u32 << n
                    } else {
                        0
                    };
                    self.mmx_meta.0 = (self.mmx_meta.0 & !0x300) | 0x500 | write_bit;
                    self.mmx_meta.1 |= 0x700 | write_bit;
                }
                if s.dst == crate::translate::front::forms::Loc::Mem {
                    self.write(Part::Mem(s.mem), s.width, None, offsets);
                }
                if let crate::translate::front::forms::Loc::Gpr(r) = s.dst {
                    self.regs[r as usize] = (0, 0);
                }
            }
            D::StepMemory(up, mem) => {
                let v = self.read(Part::Mem(mem), 4, offsets).map(|v| {
                    if up {
                        v.wrapping_add(1)
                    } else {
                        v.wrapping_sub(1)
                    }
                });
                self.write(Part::Mem(mem), 4, v, offsets);
            }
            D::NarrowStep(up, width, dst) => {
                let v = self.read(dst, width, offsets).map(|v| {
                    if up {
                        v.wrapping_add(1)
                    } else {
                        v.wrapping_sub(1)
                    }
                });
                self.write(dst, width, v, offsets);
            }
            D::Shift(_, dst, _) => {
                self.write(part(dst), 4, None, offsets);
                self.flags = None;
            }
            D::Cmov(_, dst, _) | D::Integer(Integer::Bswap(dst)) => {
                self.write(Part::Reg(dst), 4, None, offsets);
            }
            D::Setcc(_, dst) => self.write(Part::Reg(dst), 1, None, offsets),
            D::Integer(Integer::Set(_, dst)) => self.write(dst, 1, None, offsets),
            D::Integer(Integer::Cmov(_, dst, _, width) | Integer::Extend(dst, _, _, width, _)) => {
                self.write(dst, width, None, offsets)
            }
            D::Integer(Integer::NarrowShift(_, dst, width, count)) if count != 0 => {
                self.write(Part::Reg(dst), width, None, offsets);
                self.flags = None;
            }
            D::Integer(Integer::TestBit(..)) => self.flags = None,
            D::Parts(p) if matches!(p.kind, M::Adc | M::Sbb) => {
                self.write(p.dst, p.width, None, offsets);
                self.flags = None;
            }
            D::Pop(r) => {
                let v = self.read(
                    Part::Mem(Mem {
                        base: Some(ESP),
                        index: None,
                        scale: 1,
                        address_bits: 32,
                        disp: 0,
                        fs: false,
                    }),
                    4,
                    offsets,
                );
                self.regs[ESP as usize] = (0, 0);
                self.write(Part::Reg(r), 4, v, offsets);
            }
            D::Leave => {
                self.regs[ESP as usize] = (0, 0);
                self.regs[EBP as usize] = (0, 0);
            }
            D::Extend(dst, src, signed) => {
                let (src, width) = match src {
                    decode::Narrow8or16::Byte(r) => (Part::Reg(r), 1),
                    decode::Narrow8or16::Word(r) => (Part::Reg(r), 2),
                    decode::Narrow8or16::Mem(mem, word) => (Part::Mem(mem), 1 + u8::from(word)),
                };
                let v = self
                    .read(src, width, offsets)
                    .map(|v| match (signed, width) {
                        (false, _) => v,
                        (true, 1) => v as u8 as i8 as u32,
                        (true, _) => v as u16 as i16 as u32,
                    });
                self.write(Part::Reg(dst), 4, v, offsets);
            }
            D::Imul(dst, ..) | D::Neg(dst) | D::Not(dst) | D::Step(_, dst) | D::Lea(dst, _) => {
                self.regs[dst as usize] = (0, 0)
            }
            D::Carry(_, dst, _) => {
                self.regs[dst as usize] = (0, 0);
                self.flags = None;
            }
            D::Cdq => {
                self.regs[2] = if self.regs[0].1 & 0x80000000 != 0 {
                    (((self.regs[0].0 as i32) >> 31) as u32, u32::MAX)
                } else {
                    (0, 0)
                }
            }
            D::Integer(Integer::Multiply(..)) => {
                self.regs[0] = (0, 0);
                self.regs[2] = (0, 0);
                self.flags = None;
            }
            D::Integer(Integer::Divide(..)) => {
                self.regs[0] = (0, 0);
                self.regs[2] = (0, 0);
            }
            D::Push(src) => {
                let value = self.read(part(src), 4, offsets);
                self.write(
                    Part::Mem(Mem {
                        base: Some(ESP),
                        index: None,
                        scale: 1,
                        address_bits: 32,
                        disp: (-4i32) as u32,
                        fs: false,
                    }),
                    4,
                    value,
                    offsets,
                );
                self.regs[ESP as usize] = (0, 0);
            }
            D::Call(_) | D::Ret(_) => self.regs[ESP as usize] = (0, 0),
            _ => {}
        }
        Ok(())
    }
    fn alu(
        &mut self,
        alu: decode::Alu,
        dst: Part,
        src: Part,
        width: u8,
        offsets: &[Option<i32>; 8],
    ) {
        let mask = adv_cpu_core::integer::mask(width);
        let (ab, ak) = self.read_bits(dst, width, offsets);
        let (bb, bk) = self.read_bits(src, width, offsets);
        let a = (ak & mask == mask).then_some(ab & mask);
        let b = (bk & mask == mask).then_some(bb & mask);
        let (res_bits, res_known) = match alu {
            decode::Alu::And | decode::Alu::Test => {
                let k = ((ak & bk) | (ak & !ab) | (bk & !bb)) & mask;
                ((ab & bb) & k, k)
            }
            decode::Alu::Or => {
                let k = ((ak & bk) | (ak & ab) | (bk & bb)) & mask;
                ((ab | bb) & k, k)
            }
            decode::Alu::Xor => {
                let k = (ak & bk) & mask;
                ((ab ^ bb) & k, k)
            }
            decode::Alu::Add => match a.zip(b) {
                Some((a, b)) => (a.wrapping_add(b) & mask, mask),
                None => (0, 0),
            },
            decode::Alu::Sub | decode::Alu::Cmp => match a.zip(b) {
                Some((a, b)) => (a.wrapping_sub(b) & mask, mask),
                None => (0, 0),
            },
        };
        self.flags = match alu {
            decode::Alu::And | decode::Alu::Test | decode::Alu::Or | decode::Alu::Xor
                if res_known == mask =>
            {
                let mut flags = 0;
                // Logical flags (CF=OF=AF=0, SF/ZF/PF from result) depend solely on res_bits.
                adv_cpu_core::integer::binary(&mut flags, M::Or, res_bits, 0, width);
                Some(flags)
            }
            _ => a.zip(b).map(|(a, b)| {
                let mut flags = 0;
                let kind = match alu {
                    decode::Alu::Add => M::Add,
                    decode::Alu::Sub | decode::Alu::Cmp => M::Sub,
                    decode::Alu::And | decode::Alu::Test => M::And,
                    decode::Alu::Or => M::Or,
                    decode::Alu::Xor => M::Xor,
                };
                adv_cpu_core::integer::binary(&mut flags, kind, a, b, width);
                flags
            }),
        };
        if !matches!(alu, decode::Alu::Cmp | decode::Alu::Test) {
            self.write_bits(dst, width, res_bits, res_known, offsets);
        }
    }
}

pub(super) fn fp(op: Arithmetic) -> FpOp {
    match op {
        Arithmetic::Add => FpOp::Add,
        Arithmetic::Sub => FpOp::Sub,
        Arithmetic::Mul => FpOp::Mul,
        Arithmetic::Div => FpOp::Div,
    }
}
