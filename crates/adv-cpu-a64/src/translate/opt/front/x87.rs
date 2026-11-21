use super::{FpOp, FpUnary, Front, Location, Mem, Op, Recipe, Reject, Source, Type, Value, model};
use crate::translate::front::forms::X87 as Instruction;
use adv_cpu_core::floating::fast::{Fast, Source as Operand};

impl Front<'_> {
    pub(super) fn arguments(&mut self, to: usize) -> Result<Vec<Value>, Reject> {
        let live = &self.live.entry[to];
        let mut args: Vec<Value> = (0..8)
            .filter(|&r| live.reg(r))
            .map(|r| self.state.regs[r])
            .chain([self.state.flags])
            .chain(
                (0..8)
                    .filter(|&r| live.reg(8 + r))
                    .map(|r| self.state.xmm[r]),
            )
            .chain(
                (0..8)
                    .filter(|&r| live.reg(16 + r))
                    .map(|r| self.state.mmx[r]),
            )
            .chain(self.state.x87.into_iter().flatten())
            .collect();
        for start in self.held[to].clone() {
            let ty = if self.float_cell(to, start) {
                Type::F64
            } else {
                Source::Frame {
                    offset: start,
                    width: self.cells[&start],
                }
                .ty()
            };
            let value = self.cell_value(start, ty);
            args.push(value);
        }
        for &k in self.models[to].floats.clone().iter() {
            if !self.float_cell(to, k) {
                args.push(self.state.floats[&k]);
            }
        }
        let ip = self.location(self.state.ip);
        let opcode = self.location(self.state.opcode);
        args.extend([ip, opcode]);
        if let model::C1::Round(op, single) = self.models[to].c1 {
            let pair = match self.state.c1 {
                Some(Location::Recipe(Recipe::C1 {
                    op: from,
                    single: narrow,
                    a,
                    b,
                })) if from == op && single == narrow => [a, b],
                Some(Location::Constant(c)) => {
                    let (a, b) = if c == 0 {
                        (1.0, 1.0)
                    } else {
                        let eps = if single {
                            f64::from(f32::EPSILON)
                        } else {
                            f64::EPSILON
                        };
                        match op {
                            FpOp::Add => (1.0, 0.75 * eps),
                            FpOp::Sub => (1.0, -0.75 * eps),
                            FpOp::Mul => (1.5, 1.0 + eps),
                            FpOp::Div => (1.0, 10.0),
                        }
                    };
                    debug_assert_eq!(super::super::eval::rounded_up(op, a, b, single), c != 0);
                    [
                        self.constant(Type::F64, u128::from(a.to_bits())),
                        self.constant(Type::F64, u128::from(b.to_bits())),
                    ]
                }
                _ => return Err(Reject::StaticX87(to)),
            };
            args.extend(pair);
        }
        if self.models[to].c1 == model::C1::Value {
            args.push(self.location(self.state.c1.ok_or(Reject::StaticX87(to))?));
        }
        if self.assumed.mmx && self.models[to].mmx_meta.1 != u32::MAX {
            args.push(self.state.mmx_meta);
        }
        Ok(args)
    }
    fn location(&mut self, loc: Location<Value>) -> Value {
        match loc {
            Location::Constant(n) => self.imm(n as u32),
            Location::At(v) => v,
            _ => unreachable!(),
        }
    }
    pub(super) fn x87(&mut self, x: Instruction) -> Result<(), Reject> {
        let mut metadata = true;
        match x.fast {
            Fast::Load(src @ (Operand::F64 | Operand::F32 | Operand::I32 | Operand::St(_))) => {
                let value = self.x87_source(src, x.mem)?;
                self.x87_push(value)?;
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Constant(one) => {
                let value = self.constant(
                    Type::F64,
                    u128::from(if one { 1.0f64.to_bits() } else { 0 }),
                );
                self.x87_push(value)?;
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Store {
                width: 8,
                integer: false,
                pop,
            } => {
                let value = self.st(0)?;
                let mem = x.mem.ok_or(Reject::Unsupported(self.index))?;
                self.write(mem, 8, value)?;
                if let Some(offset) = self.frame_offset(mem) {
                    self.state.floats.insert(offset, value);
                }
                if pop {
                    self.x87_pop();
                }
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Store {
                width,
                integer,
                pop,
            } => {
                let rounding = (self.state.control >> 10 & 3) as u8;
                if self.state.control & 0x3f != 0x3f || !integer && rounding != 0 {
                    return Err(Reject::Mode(self.index));
                }
                let value = self.st(0)?;
                let stored = self.value(Op::X87Store {
                    value,
                    width,
                    rounding,
                    integer,
                });
                let mem = x.mem.ok_or(Reject::Unsupported(self.index))?;
                self.write(mem, width, stored)?;
                if pop {
                    self.x87_pop();
                }
                self.state.c1 = Some(if integer && rounding == 3 {
                    Location::Constant(0)
                } else {
                    Location::At(self.value(Op::X87StoreC1 {
                        value,
                        stored,
                        integer,
                    }))
                });
            }
            Fast::StoreSt { index, pop } => {
                let value = self.st(0)?;
                let slot = (self.state.top + index) & 7;
                self.state.x87[slot as usize] = Some(value);
                if pop {
                    self.x87_pop();
                }
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Exchange(index) => {
                self.st(0)?;
                self.st(index)?;
                let slot = (self.state.top + index) & 7;
                self.state.x87.swap(self.state.top as usize, slot as usize);
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Negate | Fast::Absolute => {
                let value = self.st(0)?;
                let result = self.value(Op::FpUnary(
                    if x.fast == Fast::Negate {
                        FpUnary::Neg
                    } else {
                        FpUnary::Abs
                    },
                    value,
                ));
                self.state.x87[self.state.top as usize] = Some(result);
                self.state.c1 = Some(Location::Constant(0));
            }
            Fast::Arith {
                op,
                dest,
                source: src @ (Operand::F64 | Operand::F32 | Operand::St(_)),
                reverse,
                pop,
            } => {
                let single = match self.state.control & 0x0f3f {
                    0x003f => true,
                    0x023f => false,
                    _ => return Err(Reject::Mode(self.index)),
                };
                let a = self.st(dest)?;
                let b = self.x87_source(src, x.mem)?;
                let (a, b) = if reverse { (b, a) } else { (a, b) };
                let op = model::fp(op);
                let result = self.value(if single {
                    Op::X87Single(op, a, b)
                } else {
                    Op::Fp(op, a, b)
                });
                let slot = (self.state.top + dest) & 7;
                self.state.x87[slot as usize] = Some(result);
                self.state.c1 = Some(Location::Recipe(Recipe::C1 { op, a, b, single }));
                if pop {
                    self.x87_pop();
                }
            }
            Fast::StoreControl => {
                metadata = false;
                let value = self.imm(u32::from(self.state.control));
                self.write(x.mem.ok_or(Reject::Unsupported(self.index))?, 2, value)?;
            }
            Fast::LoadControl => {
                metadata = false;
                let value = self.models[self.index]
                    .load(
                        x.mem.ok_or(Reject::Unsupported(self.index))?,
                        2,
                        &self.offsets[self.index],
                    )
                    .ok_or(Reject::Mode(self.index))? as u16;
                if value & 0x3f != 0x3f || self.at.x87.status & 0x8000 != 0 {
                    return Err(Reject::Mode(self.index));
                }
                self.state.control = value;
            }
            _ => return Err(Reject::Unsupported(self.index)),
        }
        if metadata {
            self.x87_metadata(x.opcode);
        }
        Ok(())
    }
    fn st(&self, index: u8) -> Result<Value, Reject> {
        self.state.x87[((self.state.top + index) & 7) as usize].ok_or(Reject::StaticX87(self.index))
    }
    fn x87_push(&mut self, value: Value) -> Result<(), Reject> {
        let top = self.state.top.wrapping_sub(1) & 7;
        if self.state.x87[top as usize].is_some() {
            return Err(Reject::StaticX87(self.index));
        }
        self.state.top = top;
        self.state.x87[top as usize] = Some(value);
        Ok(())
    }
    fn x87_pop(&mut self) {
        self.state.x87[self.state.top as usize] = None;
        self.state.top = (self.state.top + 1) & 7;
    }
    pub(super) fn fild64(&mut self, mem: Mem, opcode: Option<u16>) -> Result<(), Reject> {
        let value = self.integer_load(mem, 8)?;
        self.x87_push(value)?;
        self.state.c1 = Some(Location::Constant(0));
        self.x87_metadata(opcode);
        Ok(())
    }
    fn integer_load(&mut self, mem: Mem, width: u8) -> Result<Value, Reject> {
        let integer = self.read(mem, width, if width == 8 { Type::I64 } else { Type::I32 })?;
        Ok(self.value(Op::FromInt(integer)))
    }
    fn x87_source(&mut self, source: Operand, mem: Option<Mem>) -> Result<Value, Reject> {
        match source {
            Operand::St(index) => self.st(index),
            Operand::F64 => {
                let mem = mem.ok_or(Reject::Unsupported(self.index))?;
                if let Some(offset) = self.frame_offset(mem)
                    && let Some(&value) = self.state.floats.get(&offset)
                {
                    return Ok(value);
                }
                let bits = self.read(mem, 8, Type::I64)?;
                Ok(self.value(Op::X87Load {
                    bits,
                    denormal: !self.assumed.denormal,
                    single: false,
                }))
            }
            Operand::F32 => {
                let mem = mem.ok_or(Reject::Unsupported(self.index))?;
                let bits = self.read(mem, 4, Type::I32)?;
                Ok(self.value(Op::X87Load {
                    bits,
                    denormal: !self.assumed.denormal,
                    single: true,
                }))
            }
            Operand::I32 => self.integer_load(mem.ok_or(Reject::Unsupported(self.index))?, 4),
            _ => Err(Reject::Unsupported(self.index)),
        }
    }
    fn x87_metadata(&mut self, opcode: Option<u16>) {
        self.state.ip = Location::Constant(u64::from(self.unit.trace.insts[self.index].pc));
        if let Some(opcode) = opcode {
            self.state.opcode = Location::Constant(u64::from(opcode));
        }
    }
}
