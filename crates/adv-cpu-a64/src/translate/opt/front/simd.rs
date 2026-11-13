use super::*;
use crate::encode::{Float, Logic, ShiftImm};
use crate::translate::simd::{Kind, Loc, Simd};

impl Front<'_> {
    pub(super) fn simd(&mut self, s: Simd) -> Result<(), Reject> {
        // Frame cells are at most eight bytes, and an aligned access must
        // not fault where the guest's would: its address is a constant.
        let constant =
            s.mem.base.is_none() && s.mem.index.is_none() && s.mem.disp.is_multiple_of(16);
        if s.memory()
            && (s.aligned && !constant || s.width == 16 && self.frame_offset(s.mem).is_some())
        {
            return Err(Reject::Unsupported(self.index));
        }
        if s.kind == Kind::StoreControl {
            let value = self.imm(self.assumed.mxcsr);
            return self.write(s.mem, 4, value);
        }
        let src = match s.src {
            Loc::Xmm(r) => self.state.xmm[r as usize],
            Loc::Gpr(r) => self.state.regs[r as usize],
            Loc::Mem => self.read(s.mem, s.width, Type::V128)?,
            Loc::Imm => self.constant(Type::V128, u128::from(s.imm)),
            Loc::Mmx(_) => return Err(Reject::Mmx(self.index)),
        };
        let dst = match s.dst {
            Loc::Xmm(r) => self.state.xmm[r as usize],
            _ => src,
        };
        let value = match s.kind {
            Kind::Move => {
                if s.width < 16 {
                    let mask = self.constant(Type::V128, (1u128 << (s.width * 8)) - 1);
                    let src = self.bits(Type::V128, src);
                    self.value(Op::Vec(VecOp::And, src, mask))
                } else {
                    src
                }
            }
            Kind::MoveLow(3) => self.low(dst, src),
            Kind::Logic(op) => {
                let op = match op {
                    Logic::And => VecOp::And,
                    Logic::Orr => VecOp::Or,
                    Logic::Eor => VecOp::Xor,
                    _ => return Err(Reject::Unsupported(self.index)),
                };
                self.value(Op::Vec(op, dst, src))
            }
            Kind::AndNot => self.value(Op::Vec(VecOp::AndNot, dst, src)),
            Kind::Same(crate::encode::Same::Sub, 2) => self.value(Op::Vec(VecOp::SubI32, dst, src)),
            Kind::ShiftImm(op, 3) | Kind::ShiftBy(op, 3) => {
                let op = match op {
                    ShiftImm::Shl => VecOp::ShlI64,
                    ShiftImm::Ushr => VecOp::ShrI64,
                    _ => return Err(Reject::Unsupported(self.index)),
                };
                self.value(Op::Vec(op, dst, src))
            }
            Kind::Compare(false, true) if s.imm == 1 => {
                self.value(Op::Vec(VecOp::LessF64, dst, src))
            }
            Kind::Double(op, true) => {
                if self.assumed.mxcsr & 0xffc0 != 0x1f80 {
                    return Err(Reject::Mode(self.index));
                }
                let op = match op {
                    Float::Fadd => FpOp::Add,
                    Float::Fsub => FpOp::Sub,
                    Float::Fmul => FpOp::Mul,
                    Float::Fdiv => FpOp::Div,
                    _ => return Err(Reject::Unsupported(self.index)),
                };
                let a = self.bits(Type::F64, dst);
                let b = self.bits(Type::F64, src);
                let result = self.value(Op::Fp(op, a, b));
                self.low(dst, result)
            }
            Kind::ScalarFromInt(true) => {
                let src = self.bits(Type::I32, src);
                let result = self.value(Op::FromInt(src));
                self.low(dst, result)
            }
            Kind::ScalarToInt(true, true) => {
                let value = self.bits(Type::F64, src);
                self.value(Op::ToInt { value, wide: false })
            }
            Kind::FlagCompare(true) => {
                let a = self.bits(Type::F64, dst);
                let b = self.bits(Type::F64, src);
                self.state.flags = self.value(Op::Flags {
                    kind: FlagKind::UnorderedCompare,
                    width: 8,
                    a,
                    b,
                    prior: Some(self.state.flags),
                });
                return Ok(());
            }
            _ => return Err(Reject::Unsupported(self.index)),
        };
        match s.dst {
            Loc::Xmm(r) => self.state.xmm[r as usize] = self.bits(Type::V128, value),
            Loc::Gpr(r) => self.state.regs[r as usize] = self.bits(Type::I32, value),
            Loc::Mem => self.write(s.mem, s.width, value)?,
            _ => return Err(Reject::Unsupported(self.index)),
        }
        Ok(())
    }
    fn low(&mut self, dst: Value, src: Value) -> Value {
        let mask = self.constant(Type::V128, u128::MAX << 64);
        let hi = self.value(Op::Vec(VecOp::And, dst, mask));
        let lo = self.bits(Type::I64, src);
        let lo = self.bits(Type::V128, lo);
        self.value(Op::Vec(VecOp::Or, hi, lo))
    }
}
