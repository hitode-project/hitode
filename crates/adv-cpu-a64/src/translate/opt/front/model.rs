//! Constants proven on every arrival, independently of SSA identities.
use super::*;
use adv_cpu_core::floating::fast::{Fast, Op as Arithmetic};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum C1 {
    Constant(bool),
    Round(FpOp),
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Model {
    pub top: u8,
    pub valid: u8,
    pub control: Option<u16>,
    pub c1: C1,
    pub floats: std::collections::BTreeSet<i32>,
    pub dirty: std::collections::BTreeSet<i32>,
    pub truncations: std::collections::BTreeSet<(i32, u8)>,
    regs: [(u32, u32); 8],
    mxcsr: u32,
    bytes: BTreeMap<i32, u8>,
    flags: Option<u32>,
}

pub(super) fn analyze(
    unit: &Unit,
    at: &CpuState,
    offsets: &[[Option<i32>; 8]],
) -> Result<Vec<Model>, Reject> {
    let entry = Model {
        top: at.x87.top() as u8,
        valid: (0..8).fold(0, |v, i| v | (u8::from(at.x87.physical_tag(i) != 3) << i)),
        control: Some(at.x87.control),
        c1: C1::Constant(at.x87.status & 0x200 != 0),
        floats: Default::default(),
        dirty: Default::default(),
        truncations: Default::default(),
        regs: [(0, 0); 8],
        mxcsr: at.mxcsr,
        bytes: BTreeMap::new(),
        flags: None,
    };
    let mut states = vec![None; unit.trace.len()];
    states[0] = Some(entry.clone());
    let mut work = vec![0];
    while let Some(i) = work.pop() {
        let mut m = states[i].clone().unwrap();
        let inst = unit.trace.insts[i];
        if inst.effects.writes != crate::translate::flags::FlagSet::NONE {
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
        let branch = match inst.op {
            decode::Op::Jcc(Cc::Flags(cc), _) => m
                .flags
                .map(|flags| adv_cpu_core::integer::condition_true(cc, flags)),
            _ => None,
        };
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
                old.floats.retain(|p| m.floats.contains(p));
                old.dirty.extend(m.dirty.iter().copied());
                old.truncations.retain(|k| m.truncations.contains(k));
                old.bytes.retain(|p, v| m.bytes.get(p) == Some(v));
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
    Ok(states
        .into_iter()
        .map(|m| m.unwrap_or_else(|| entry.clone()))
        .collect())
}

fn merge_c1(a: C1, b: C1, i: usize) -> Result<C1, Reject> {
    Ok(match (a, b) {
        (a, b) if a == b => a,
        (C1::Unknown, _) | (_, C1::Unknown) => C1::Unknown,
        (C1::Round(a), C1::Round(b)) if a != b => return Err(Reject::StaticX87(i)),
        (C1::Round(op), _) | (_, C1::Round(op)) => C1::Round(op),
        _ => C1::Round(FpOp::Add),
    })
}

impl Model {
    pub(super) fn load(&self, mem: Mem, width: u8, offsets: &[Option<i32>; 8]) -> Option<u64> {
        let offset = frame_offset(mem, offsets)?;
        (0..i32::from(width)).try_fold(0, |bits, i| {
            Some(bits | (u64::from(*self.bytes.get(&(offset + i))?) << (i * 8)))
        })
    }
    pub(super) fn read(&self, src: Part, width: u8, offsets: &[Option<i32>; 8]) -> Option<u32> {
        let x = match src {
            Part::Imm(n) => n,
            Part::Reg(r) => {
                let (r, shift) = if width == 1 && r >= 4 {
                    (r - 4, 8)
                } else {
                    (r, 0)
                };
                let (bits, known) = self.regs[r as usize];
                let mask = adv_cpu_core::integer::mask(width) << shift;
                if known & mask != mask {
                    return None;
                }
                bits >> shift
            }
            Part::Mem(mem) => self.load(mem, width, offsets)? as u32,
        };
        Some(x & adv_cpu_core::integer::mask(width))
    }
    fn write(&mut self, dst: Part, width: u8, value: Option<u32>, offsets: &[Option<i32>; 8]) {
        match dst {
            Part::Reg(r) => {
                let (r, shift) = if width == 1 && r >= 4 {
                    (r - 4, 8)
                } else {
                    (r, 0)
                };
                let mask = adv_cpu_core::integer::mask(width) << shift;
                let (bits, known) = &mut self.regs[r as usize];
                *bits = *bits & !mask | (value.unwrap_or(0) << shift) & mask;
                *known = *known & !mask | if value.is_some() { mask } else { 0 };
            }
            Part::Mem(mem) => {
                if let Some(p) = frame_offset(mem, offsets) {
                    self.floats
                        .retain(|&start| start + 8 <= p || p + i32::from(width) <= start);
                    self.truncations.retain(|&(start, w)| {
                        start + i32::from(w) <= p || p + i32::from(width) <= start
                    });
                    for b in 0..i32::from(width) {
                        self.dirty.insert(p + b);
                        if let Some(x) = value {
                            self.bytes.insert(p + b, (x >> (b * 8)) as u8);
                        } else {
                            self.bytes.remove(&(p + b));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    fn step(&mut self, op: decode::Op, offsets: &[Option<i32>; 8], i: usize) -> Result<(), Reject> {
        use decode::Op as D;
        let part = |s| match s {
            Src::Reg(r) => Part::Reg(r),
            Src::Imm(n) => Part::Imm(n),
            Src::Mem(m) => Part::Mem(m),
        };
        match op {
            D::Mov(dst, src) => {
                let v = self.read(part(src), 4, offsets);
                self.write(part(dst), 4, v, offsets);
            }
            D::Narrow(None, width, dst, src) => {
                let v = self.read(src, width, offsets);
                self.write(dst, width, v, offsets);
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
                    Fast::Store { pop, integer, .. } => {
                        need(self.top)?;
                        if pop {
                            self.valid &= !(1 << self.top);
                            self.top = (self.top + 1) & 7;
                        }
                        self.c1 = if integer {
                            C1::Unknown
                        } else {
                            C1::Constant(false)
                        };
                        if let Some(mem) = x.mem {
                            let width = if let Fast::Store { width, .. } = x.fast {
                                width
                            } else {
                                unreachable!()
                            };
                            if let Some(p) = frame_offset(mem, offsets) {
                                self.floats.retain(|&start| {
                                    start + 8 <= p || p + i32::from(width) <= start
                                });
                                self.truncations.retain(|&(start, w)| {
                                    start + i32::from(w) <= p || p + i32::from(width) <= start
                                });
                                for b in 0..i32::from(width) {
                                    self.dirty.insert(p + b);
                                    self.bytes.remove(&(p + b));
                                }
                                if integer {
                                    self.truncations.insert((p, width));
                                } else if width == 8 {
                                    self.floats.insert(p);
                                }
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
                        self.c1 = C1::Round(fp(op));
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
            D::Simd(s) if s.kind == crate::translate::simd::Kind::StoreControl => {
                self.write(Part::Mem(s.mem), 4, Some(self.mxcsr), offsets);
            }
            D::Simd(s) => {
                if s.dst == crate::translate::simd::Loc::Mem {
                    self.write(Part::Mem(s.mem), s.width, None, offsets);
                }
                if let crate::translate::simd::Loc::Gpr(r) = s.dst {
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
            D::Shift(_, dst, _) => self.write(part(dst), 4, None, offsets),
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
                self.write(Part::Reg(r), 4, v, offsets);
            }
            D::Leave => self.regs[EBP as usize] = (0, 0),
            D::Imul(dst, ..)
            | D::Extend(dst, ..)
            | D::Neg(dst)
            | D::Not(dst)
            | D::Step(_, dst)
            | D::Lea(dst, _) => self.regs[dst as usize] = (0, 0),
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
            D::Integer(crate::translate::integer::Integer::Divide(..)) => {
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
            }
            D::Call(_) => {}
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
        let a = self.read(dst, width, offsets);
        let b = self.read(src, width, offsets);
        let value = a.zip(b).map(|(a, b)| {
            use decode::Alu::*;
            match alu {
                Add => a.wrapping_add(b),
                Sub | Cmp => a.wrapping_sub(b),
                And | Test => a & b,
                Or => a | b,
                Xor => a ^ b,
            }
        });
        self.flags = a.zip(b).map(|(a, b)| {
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
        });
        if !matches!(alu, decode::Alu::Cmp | decode::Alu::Test) {
            self.write(dst, width, value, offsets);
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
