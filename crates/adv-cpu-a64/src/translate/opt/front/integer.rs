use super::{
    Cc, ConditionCode, EBP, ESP, FlagKind, Front, IntOp, IntUnary, M, Mem, Mode, Op, Part, Reject,
    Src, Type, Value, decode,
};
use crate::translate::decode::{Alu, Narrow8or16, ShiftKind};
use crate::translate::front::tags::Mnemonic;
use crate::translate::integer::Integer;

impl Front<'_> {
    pub(super) fn instruction(&mut self) -> Result<(), Reject> {
        use decode::Op as D;
        let inst = self.unit.trace.insts[self.index];
        if inst.call.is_some() || inst.locked.is_some() {
            return Err(Reject::Helper(self.index));
        }
        match inst.op {
            D::X87(x) => self.x87(x)?,
            D::Fild64(mem, _, opcode) => self.fild64(mem, opcode)?,
            D::Nop | D::Jmp(_) | D::Jcc(Cc::Flags(_), _) => {}
            D::Mov(dst, src) => {
                let value = self.src(src)?;
                self.put(dst, value)?;
            }
            D::Lea(dst, mem) if !mem.fs => {
                self.state.regs[dst as usize] = self.address(mem)?;
            }
            D::Alu(alu, dst, src) => self.alu(alu, part(dst), part(src), 4)?,
            D::Carry(sub, dst, src) => self.binary(
                if sub { M::Sbb } else { M::Adc },
                Part::Reg(dst),
                part(src),
                4,
            )?,
            D::Step(up, dst) => self.unary(if up { M::Inc } else { M::Dec }, Part::Reg(dst), 4)?,
            D::StepMemory(up, mem) => {
                self.unary(if up { M::Inc } else { M::Dec }, Part::Mem(mem), 4)?
            }
            D::Neg(dst) => self.unary(M::Neg, Part::Reg(dst), 4)?,
            D::Not(dst) => self.unary(M::Not, Part::Reg(dst), 4)?,
            D::Narrow(alu, width, dst, src) => {
                if let Some(alu) = alu {
                    self.alu(alu, dst, src, width)?;
                } else {
                    let value = self.part(src, width)?;
                    self.put_part(dst, width, value)?;
                }
            }
            D::NarrowStep(up, width, dst) => {
                self.unary(if up { M::Inc } else { M::Dec }, dst, width)?
            }
            D::Extend(dst, src, signed) => {
                let (src, width) = match src {
                    Narrow8or16::Byte(r) => (Part::Reg(r), 1),
                    Narrow8or16::Word(r) => (Part::Reg(r), 2),
                    Narrow8or16::Mem(mem, word) => (Part::Mem(mem), if word { 2 } else { 1 }),
                };
                let value = self.extend(src, width, signed)?;
                self.state.regs[dst as usize] = value;
            }
            D::Cdq => {
                let count = self.imm(31);
                self.state.regs[2] = self.int(IntOp::Sar, self.state.regs[0], count);
            }
            D::Imul(dst, src, imm) => {
                let a = match imm {
                    Some(n) => self.imm(n),
                    None => self.state.regs[dst as usize],
                };
                let b = self.src(src)?;
                let result = self.int(IntOp::Mul, a, b);
                self.flags(FlagKind::MulSigned, 4, a, b);
                self.state.regs[dst as usize] = result;
            }
            D::Shift(kind, dst, count) => {
                let count = match count {
                    Some(n) => self.imm(n),
                    None => self.state.regs[1],
                };
                self.shift(kind, part(dst), count, 4)?;
            }
            D::Cmov(cc, dst, src) => {
                self.cmov(cc, Part::Reg(dst), part(src), 4)?;
            }
            D::Setcc(cc, dst) => self.setcc(cc, Part::Reg(dst))?,
            D::Parts(p) if matches!(p.kind, Mnemonic::Adc | Mnemonic::Sbb) => self.binary(
                if p.kind == Mnemonic::Adc {
                    M::Adc
                } else {
                    M::Sbb
                },
                p.dst,
                p.source.unwrap().0,
                p.width,
            )?,
            D::Integer(Integer::Cmov(cc, dst, src, width)) => self.cmov(cc, dst, src, width)?,
            D::Integer(Integer::Set(cc, dst)) => self.setcc(cc, dst)?,
            D::Integer(Integer::Extend(dst, src, width, dst_width, signed)) => {
                let value = self.extend(src, width, signed)?;
                self.put_part(dst, dst_width, value)?;
            }
            D::Integer(Integer::TestBit(src, bit)) => {
                let value = self.src(src)?;
                let bit = self.imm(u32::from(bit));
                let value = self.int(IntOp::Shr, value, bit);
                self.flags(FlagKind::CarryBit, 4, value, value);
            }
            D::Integer(Integer::Bswap(dst)) => {
                self.state.regs[dst as usize] = self.value(Op::IntUnary(
                    IntUnary::ByteSwap,
                    self.state.regs[dst as usize],
                ));
            }
            D::Integer(Integer::Multiply(src, signed)) => {
                let a = self.state.regs[0];
                let b = self.src(src)?;
                let high = self.int(
                    if signed {
                        IntOp::MulHighSigned
                    } else {
                        IntOp::MulHighUnsigned
                    },
                    a,
                    b,
                );
                self.state.regs[0] = self.int(IntOp::Mul, a, b);
                self.state.regs[2] = high;
                if signed {
                    self.flags(FlagKind::MulSigned, 4, a, b);
                } else {
                    self.flags(FlagKind::MulUnsigned, 4, high, high);
                }
            }
            D::Integer(Integer::NarrowShift(kind, dst, width, count)) => {
                if count != 0 {
                    let count = self.imm(count);
                    self.shift(kind, Part::Reg(dst), count, width)?;
                }
            }
            D::Push(src) => {
                let value = self.src(src)?;
                self.push(value)?;
            }
            D::Pop(dst) => {
                let value = self.pop()?;
                self.state.regs[dst as usize] = value;
            }
            D::Call(_) if inst.mode != Mode::Real => {
                let ret = self.imm(inst.next_pc);
                self.push(ret)?;
            }
            D::Ret(release) if matches!(inst.mode, Mode::Inline(_)) => {
                let mem = Mem {
                    base: Some(ESP),
                    index: None,
                    scale: 1,
                    address_bits: 32,
                    disp: 0,
                    fs: false,
                };
                let Mode::Inline(expected) = inst.mode else {
                    unreachable!()
                };
                if self.models[self.index].load(mem, 4, &self.offsets[self.index])
                    != Some(u64::from(expected))
                {
                    return Err(Reject::Dynamic(self.index));
                }
                self.pop()?;
                if release != 0 {
                    let release = self.imm(u32::from(release));
                    self.state.regs[ESP as usize] =
                        self.int(IntOp::Add, self.state.regs[ESP as usize], release);
                }
            }
            D::Leave => {
                self.state.regs[ESP as usize] = self.state.regs[EBP as usize];
                let mem = Mem {
                    base: Some(EBP),
                    index: None,
                    scale: 1,
                    address_bits: 32,
                    disp: 0,
                    fs: false,
                };
                let value = self.read(mem, 4, Type::I32)?;
                let four = self.imm(4);
                self.state.regs[ESP as usize] =
                    self.int(IntOp::Add, self.state.regs[ESP as usize], four);
                self.state.regs[EBP as usize] = value;
            }
            D::Integer(Integer::Divide(src, signed, _)) => {
                let divisor = self.src(src)?;
                let lo = self.bits(Type::I64, self.state.regs[0]);
                let hi = self.bits(Type::I64, self.state.regs[2]);
                let shift = self.constant(Type::I64, 32);
                let hi = self.int(IntOp::Shl, hi, shift);
                let dividend = self.int(IntOp::Or, lo, hi);
                let proved = signed && self.division_safe[self.index];
                let quotient = self.value(Op::Div {
                    signed,
                    dividend,
                    divisor,
                    proved,
                });
                self.state.regs[0] = quotient;
                self.state.regs[2] = self.value(Op::Remainder {
                    quotient,
                    dividend,
                    divisor,
                });
            }
            D::Value(_) | D::Body(_) | D::Syscall => return Err(Reject::Helper(self.index)),
            D::Simd(s) => self.simd(s)?,
            _ => return Err(Reject::Unsupported(self.index)),
        }
        Ok(())
    }

    fn src(&mut self, src: Src) -> Result<Value, Reject> {
        self.part(part(src), 4)
    }
    fn put(&mut self, dst: Src, value: Value) -> Result<(), Reject> {
        self.put_part(part(dst), 4, value)
    }
    fn part(&mut self, src: Part, width: u8) -> Result<Value, Reject> {
        if let Some(bits) = self.models[self.index].read(src, width, &self.offsets[self.index]) {
            return Ok(self.imm(bits));
        }
        let value = match src {
            Part::Reg(r) => {
                let (r, shift) = register(r, width);
                let mut value = self.state.regs[r as usize];
                if shift != 0 {
                    let count = self.imm(shift);
                    value = self.int(IntOp::Shr, value, count);
                }
                value
            }
            Part::Imm(n) => self.imm(n),
            Part::Mem(mem) => self.read(mem, width, Type::I32)?,
        };
        Ok(self.mask(value, width))
    }
    fn put_part(&mut self, dst: Part, width: u8, value: Value) -> Result<(), Reject> {
        match dst {
            Part::Reg(r) => {
                let (r, shift) = register(r, width);
                let result = if width == 4 {
                    value
                } else {
                    let mask = self.imm(!(((1u32 << (width * 8)) - 1) << shift));
                    let old = self.int(IntOp::And, self.state.regs[r as usize], mask);
                    let value = self.mask(value, width);
                    let count = self.imm(shift);
                    let value = self.int(IntOp::Shl, value, count);
                    self.int(IntOp::Or, old, value)
                };
                self.state.regs[r as usize] = result;
            }
            Part::Mem(mem) => self.write(mem, width, value)?,
            Part::Imm(_) => unreachable!("decoded destination"),
        }
        Ok(())
    }
    fn flags(&mut self, kind: FlagKind, width: u8, a: Value, b: Value) {
        self.state.flags = self.value(Op::Flags {
            kind,
            width,
            a,
            b,
            prior: Some(self.state.flags),
        });
    }
    fn alu(&mut self, alu: Alu, dst: Part, src: Part, width: u8) -> Result<(), Reject> {
        self.binary(
            match alu {
                Alu::Add => M::Add,
                Alu::Sub => M::Sub,
                Alu::And => M::And,
                Alu::Or => M::Or,
                Alu::Xor => M::Xor,
                Alu::Cmp => M::Cmp,
                Alu::Test => M::Test,
            },
            dst,
            src,
            width,
        )
    }
    fn binary(&mut self, kind: M, dst: Part, src: Part, width: u8) -> Result<(), Reject> {
        let a = self.part(dst, width)?;
        let b = self.part(src, width)?;
        let op = match kind {
            M::Add | M::Adc => IntOp::Add,
            M::Sub | M::Sbb | M::Cmp => IntOp::Sub,
            M::And | M::Test => IntOp::And,
            M::Or => IntOp::Or,
            M::Xor => IntOp::Xor,
            _ => return Err(Reject::Unsupported(self.index)),
        };
        let mut result = self.int(op, a, b);
        let flag = match kind {
            M::Add => FlagKind::Add,
            M::Adc => FlagKind::AddCarry,
            M::Sub | M::Cmp => FlagKind::Sub,
            M::Sbb => FlagKind::SubBorrow,
            _ => FlagKind::Logic,
        };
        if matches!(kind, M::Adc | M::Sbb) {
            let carry = self.value(Op::Test {
                flags: self.state.flags,
                cond: ConditionCode::b,
            });
            result = self.int(op, result, carry);
        }
        self.flags(
            flag,
            width,
            if flag == FlagKind::Logic { result } else { a },
            b,
        );
        if !matches!(kind, M::Cmp | M::Test) {
            self.put_part(dst, width, result)?;
        }
        Ok(())
    }
    fn unary(&mut self, kind: M, dst: Part, width: u8) -> Result<(), Reject> {
        let a = self.part(dst, width)?;
        let one = self.imm(1);
        let result = match kind {
            M::Inc => self.int(IntOp::Add, a, one),
            M::Dec => self.int(IntOp::Sub, a, one),
            M::Not => self.value(Op::IntUnary(IntUnary::Not, a)),
            M::Neg => self.value(Op::IntUnary(IntUnary::Neg, a)),
            _ => return Err(Reject::Unsupported(self.index)),
        };
        if kind != M::Not {
            self.flags(
                match kind {
                    M::Inc => FlagKind::Increment,
                    M::Dec => FlagKind::Decrement,
                    _ => FlagKind::Negate,
                },
                width,
                a,
                one,
            );
        }
        self.put_part(dst, width, result)
    }
    fn extend(&mut self, src: Part, width: u8, signed: bool) -> Result<Value, Reject> {
        let value = self.part(src, width)?;
        let op = match (signed, width) {
            (true, 1) => IntUnary::SignExtend8,
            (true, 2) => IntUnary::SignExtend16,
            (false, 1) => IntUnary::ZeroExtend8,
            (false, 2) => IntUnary::ZeroExtend16,
            _ => return Ok(value),
        };
        Ok(self.value(Op::IntUnary(op, value)))
    }
    fn cmov(&mut self, cc: ConditionCode, dst: Part, src: Part, width: u8) -> Result<(), Reject> {
        // x86 reads the source even when the condition is false.
        let a = self.part(src, width)?;
        let b = self.part(dst, width)?;
        let cond = self.value(Op::Test {
            flags: self.state.flags,
            cond: cc,
        });
        let value = self.value(Op::Select { cond, a, b });
        self.put_part(dst, width, value)
    }

    fn setcc(&mut self, cc: ConditionCode, dst: Part) -> Result<(), Reject> {
        let value = self.value(Op::Test {
            flags: self.state.flags,
            cond: cc,
        });
        self.put_part(dst, 1, value)
    }

    fn shift(&mut self, kind: ShiftKind, dst: Part, count: Value, width: u8) -> Result<(), Reject> {
        let a = self.part(dst, width)?;
        let (op, flag) = match kind {
            ShiftKind::Shl => (IntOp::Shl, FlagKind::Shl),
            ShiftKind::Shr => (IntOp::Shr, FlagKind::Shr),
            ShiftKind::Sar => (IntOp::Sar, FlagKind::Sar),
            ShiftKind::Rol | ShiftKind::Ror => {
                let right = if kind == ShiftKind::Rol {
                    self.value(Op::IntUnary(IntUnary::Neg, count))
                } else {
                    count
                };
                let result = self.int(IntOp::Ror, a, right);
                let flag = if kind == ShiftKind::Rol {
                    FlagKind::Rol
                } else {
                    FlagKind::Ror
                };
                self.flags(flag, width, result, count);
                return self.put_part(dst, width, result);
            }
        };
        let input = if kind == ShiftKind::Sar && width < 4 {
            let extend = if width == 1 {
                IntUnary::SignExtend8
            } else {
                IntUnary::SignExtend16
            };
            self.value(Op::IntUnary(extend, a))
        } else {
            a
        };
        let result = self.int(op, input, count);
        self.flags(flag, width, a, count);
        self.put_part(dst, width, result)
    }
    fn push(&mut self, value: Value) -> Result<(), Reject> {
        let mem = Mem {
            base: Some(ESP),
            index: None,
            scale: 1,
            address_bits: 32,
            disp: (-4i32) as u32,
            fs: false,
        };
        self.write(mem, 4, value)?;
        let four = self.imm(4);
        self.state.regs[ESP as usize] = self.int(IntOp::Sub, self.state.regs[ESP as usize], four);
        Ok(())
    }
    fn pop(&mut self) -> Result<Value, Reject> {
        let mem = Mem {
            base: Some(ESP),
            index: None,
            scale: 1,
            address_bits: 32,
            disp: 0,
            fs: false,
        };
        let value = self.read(mem, 4, Type::I32)?;
        let four = self.imm(4);
        self.state.regs[ESP as usize] = self.int(IntOp::Add, self.state.regs[ESP as usize], four);
        Ok(value)
    }
}

pub(super) fn part(src: Src) -> Part {
    match src {
        Src::Reg(r) => Part::Reg(r),
        Src::Imm(n) => Part::Imm(n),
        Src::Mem(mem) => Part::Mem(mem),
    }
}
fn register(r: u8, width: u8) -> (u8, u32) {
    if width == 1 && r >= 4 {
        (r - 4, 8)
    } else {
        (r, 0)
    }
}
