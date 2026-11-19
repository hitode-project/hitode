use super::*;
use crate::encode::{Float, Logic};
use crate::translate::simd::{Kind, Loc, Simd};

impl Front<'_> {
    pub(super) fn simd(&mut self, s: Simd) -> Result<(), Reject> {
        if self.assumed.x87 && s.mmx() {
            return Err(Reject::Mmx(self.index));
        }
        if s.kind == Kind::Emms {
            self.update_mmx_meta(true, None);
            return Ok(());
        }
        // Frame cells are at most eight bytes, and an aligned access must
        // not fault where the guest's would: its address is a constant.
        let constant = !s.mem.fs
            && s.mem.base.is_none()
            && s.mem.index.is_none()
            && s.mem.disp.is_multiple_of(16);
        if s.memory()
            && (s.aligned && !constant || s.width == 16 && self.frame_offset(s.mem).is_some())
        {
            return Err(Reject::Unsupported(self.index));
        }
        if s.kind == Kind::StoreControl {
            let value = self.imm(self.assumed.mxcsr);
            return self.write(s.mem, 4, value);
        }
        let full = matches!(s.dst, Loc::Xmm(_));
        let src = match s.src {
            Loc::Xmm(r) => self.state.xmm[r as usize],
            Loc::Mmx(r) => self.state.mmx[r as usize],
            Loc::Gpr(r) => self.state.regs[r as usize],
            Loc::Mem => self.read(
                s.mem,
                s.width,
                if matches!(s.kind, Kind::InsertWord | Kind::ScalarFromInt(_)) {
                    Type::I32
                } else {
                    Type::V128
                },
            )?,
            Loc::Imm => self.constant(Type::V128, u128::from(s.imm)),
        };
        let dst = match s.dst {
            Loc::Xmm(r) => self.state.xmm[r as usize],
            Loc::Mmx(r) => self.state.mmx[r as usize],
            _ => src,
        };
        let value = match s.kind {
            Kind::Move => {
                if s.width == 8 && matches!(s.dst, Loc::Mmx(_) | Loc::Xmm(_)) {
                    let src = self.bits(Type::V128, src);
                    self.value(Op::VecUnary(VecUnary::Low64, src))
                } else if s.width < 16 && matches!(s.dst, Loc::Mmx(_) | Loc::Xmm(_)) {
                    let mask = self.constant(Type::V128, (1u128 << (s.width * 8)) - 1);
                    let src = self.bits(Type::V128, src);
                    self.value(Op::Vec(VecOp::And(full), src, mask))
                } else {
                    src
                }
            }
            Kind::MoveLow(2) => self.value(Op::Vec(VecOp::MoveLow32, dst, src)),
            Kind::MoveLow(3) => self.value(Op::Vec(VecOp::Insert64(false, false), dst, src)),
            Kind::MoveHalf(high) => self.value(Op::Vec(VecOp::Insert64(high, !high), dst, src)),
            Kind::Half(high) if s.dst == Loc::Mem => {
                if high {
                    self.value(Op::VecUnary(VecUnary::High64, src))
                } else {
                    src
                }
            }
            Kind::Half(high) => self.value(Op::Vec(VecOp::Insert64(high, false), dst, src)),
            Kind::Shuffle(log2) => {
                self.value(Op::VecUnary(VecUnary::Shuffle(log2, s.imm, full), src))
            }
            Kind::ShuffleWords(high) => {
                self.value(Op::VecUnary(VecUnary::ShuffleWords(high, s.imm), src))
            }
            Kind::Shufps => self.value(Op::Vec(VecOp::Shufps(s.imm), dst, src)),
            Kind::Shufpd => self.value(Op::Vec(VecOp::Shufpd(s.imm), dst, src)),
            Kind::MulDwords => self.value(Op::Vec(VecOp::MulDwords(full), dst, src)),
            Kind::SadBytes => self.value(Op::Vec(VecOp::SadBytes(full), dst, src)),
            Kind::ShiftBytes(left) => {
                self.value(Op::VecUnary(VecUnary::ShiftBytes(left, s.imm), dst))
            }
            Kind::ExtractWord => {
                let lane = s.imm & if matches!(s.src, Loc::Xmm(_)) { 7 } else { 3 };
                self.value(Op::VecToInt(VecToInt::ExtractWord(lane), src))
            }
            Kind::InsertWord => {
                let lane = s.imm & if full { 7 } else { 3 };
                let word = self.bits(Type::V128, src);
                self.value(Op::Vec(VecOp::InsertWord(lane, full), dst, word))
            }
            Kind::ByteMask => {
                let wide = matches!(s.src, Loc::Xmm(_));
                self.value(Op::VecToInt(VecToInt::ByteMask(wide), src))
            }
            Kind::SignMask(double) => self.value(Op::VecToInt(VecToInt::SignMask(double), src)),
            Kind::Logic(op) => {
                let op = match op {
                    Logic::And => VecOp::And(full),
                    Logic::Orr => VecOp::Or(full),
                    Logic::Eor => VecOp::Xor(full),
                    _ => return Err(Reject::Unsupported(self.index)),
                };
                self.value(Op::Vec(op, dst, src))
            }
            Kind::AndNot => self.value(Op::Vec(VecOp::AndNot(full), dst, src)),
            Kind::Same(op, log2) => self.value(Op::Vec(VecOp::Same(op, log2, full), dst, src)),
            Kind::Unpack(op, log2) => self.value(Op::Vec(VecOp::Unpack(op, log2, full), dst, src)),
            Kind::Pack(op, log2) => self.value(Op::Vec(VecOp::Pack(op, log2, full), dst, src)),
            Kind::MulHigh(signed) => self.value(Op::Vec(VecOp::MulHigh(signed, full), dst, src)),
            Kind::MulAdd => self.value(Op::Vec(VecOp::MulAdd(full), dst, src)),
            Kind::ShiftImm(op, log2) => {
                self.value(Op::VecUnary(VecUnary::ShiftImm(op, log2, s.imm, full), dst))
            }
            Kind::ShiftBy(op, log2) => self.value(Op::Vec(VecOp::Shift(op, log2, full), dst, src)),
            Kind::Compare(false, true) if s.imm == 1 => {
                self.sse_input(dst, Some(src), true, false);
                self.value(Op::Vec(VecOp::LessF64, dst, src))
            }
            Kind::Double(op, scalar) | Kind::Float(op, scalar) if scalar || self.func.resident => {
                let op = match op {
                    Float::Fadd => FpOp::Add,
                    Float::Fsub => FpOp::Sub,
                    Float::Fmul => FpOp::Mul,
                    Float::Fdiv => FpOp::Div,
                    _ => return Err(Reject::Unsupported(self.index)),
                };
                let single = matches!(s.kind, Kind::Float(..));
                self.sse_input(dst, Some(src), !scalar, single);
                if !scalar {
                    self.value(Op::Vec(VecOp::PackedFp(op, !single), dst, src))
                } else if single {
                    let result = self.value(Op::Fp32(op, dst, src));
                    self.value(Op::Vec(VecOp::MoveLow32, dst, result))
                } else {
                    let a = self.bits(Type::F64, dst);
                    let b = self.bits(Type::F64, src);
                    let result = self.value(Op::SseFp64(op, a, b));
                    self.low(dst, result)
                }
            }
            Kind::ScalarFromInt(double) => {
                let src = self.bits(Type::I32, src);
                let result = self.value(Op::FromInt(src));
                if double {
                    self.low(dst, result)
                } else {
                    let result = self.bits(Type::V128, result);
                    let result = self.value(Op::VecUnary(VecUnary::NarrowScalar, result));
                    self.value(Op::Vec(VecOp::MoveLow32, dst, result))
                }
            }
            Kind::ScalarToInt(true, true) => {
                self.sse_input(src, None, false, false);
                let value = self.bits(Type::F64, src);
                self.value(Op::ToInt {
                    value,
                    wide: false,
                    sse: true,
                })
            }
            Kind::FromInt(double) if self.func.resident && !s.mmx() => self.value(Op::VecUnary(
                VecUnary::Convert(PackedConvert::FromDwords { double }),
                src,
            )),
            Kind::Widen(false) if self.func.resident => {
                if self.assumed.mxcsr & 2 == 0 {
                    let low = self.value(Op::VecUnary(VecUnary::Low64, src));
                    self.sse_input(low, None, true, true);
                }
                self.value(Op::VecUnary(VecUnary::Convert(PackedConvert::Widen), src))
            }
            Kind::Narrow(false) if self.func.resident => {
                self.sse_input(src, None, true, false);
                self.value(Op::VecUnary(VecUnary::Convert(PackedConvert::Narrow), src))
            }
            Kind::PackedToInt(truncate, double) if self.func.resident && !s.mmx() => {
                self.sse_input(src, None, true, !double);
                self.value(Op::VecUnary(
                    VecUnary::Convert(PackedConvert::ToDwords { double, truncate }),
                    src,
                ))
            }
            Kind::FlagCompare(true, ordered) => {
                self.sse_input(dst, Some(src), false, false);
                let a = self.bits(Type::F64, dst);
                let b = self.bits(Type::F64, src);
                self.state.flags = self.value(Op::Flags {
                    kind: if ordered {
                        FlagKind::OrderedCompare
                    } else {
                        FlagKind::UnorderedCompare
                    },
                    width: 8,
                    a,
                    b,
                    prior: Some(self.state.flags),
                });
                return Ok(());
            }
            _ => {
                return Err(if s.mmx() {
                    Reject::Mmx(self.index)
                } else {
                    Reject::Unsupported(self.index)
                });
            }
        };
        match s.dst {
            Loc::Xmm(r) => self.state.xmm[r as usize] = self.bits(Type::V128, value),
            Loc::Mmx(r) => self.state.mmx[r as usize] = self.bits(Type::V128, value),
            Loc::Gpr(r) => self.state.regs[r as usize] = self.bits(Type::I32, value),
            Loc::Mem => self.write(s.mem, s.width, value)?,
            _ => return Err(Reject::Unsupported(self.index)),
        }
        if s.mmx() {
            let dst_mmx = match s.dst {
                Loc::Mmx(r) => Some(r),
                _ => None,
            };
            self.update_mmx_meta(false, dst_mmx);
        }
        Ok(())
    }
    fn update_mmx_meta(&mut self, emms: bool, dst_mmx: Option<u8>) {
        if emms {
            self.state.mmx_meta_bits.0 = (self.state.mmx_meta_bits.0 & !0x300) | 0x200;
            self.state.mmx_meta_bits.1 |= 0x300;
            if self.state.mmx_meta_bits.1 == u32::MAX {
                self.state.mmx_meta = self.imm(self.state.mmx_meta_bits.0);
            } else {
                let clear = self.imm(!0x300);
                let set = self.imm(0x200);
                let masked = self.int(IntOp::And, self.state.mmx_meta, clear);
                self.state.mmx_meta = self.int(IntOp::Or, masked, set);
            }
        } else {
            let write_bit = dst_mmx.map_or(0, |n| 1u32 << n);
            self.state.mmx_meta_bits.0 = (self.state.mmx_meta_bits.0 & !0x300) | 0x500 | write_bit;
            self.state.mmx_meta_bits.1 |= 0x700 | write_bit;
            if self.state.mmx_meta_bits.1 == u32::MAX {
                self.state.mmx_meta = self.imm(self.state.mmx_meta_bits.0);
            } else {
                let clear = self.imm(!0x300);
                let set = self.imm(0x500 | write_bit);
                let masked = self.int(IntOp::And, self.state.mmx_meta, clear);
                self.state.mmx_meta = self.int(IntOp::Or, masked, set);
            }
        }
    }
    fn sse_input(&mut self, a: Value, b: Option<Value>, packed: bool, single: bool) {
        if self.assumed.mxcsr & 2 == 0 {
            let pc = self.func.blocks[self.block.0 as usize].pcs[0];
            self.func.push(
                self.block,
                Op::SseInput {
                    a,
                    b,
                    packed,
                    single,
                    pc,
                },
            );
        }
    }

    fn low(&mut self, dst: Value, src: Value) -> Value {
        let lo = self.bits(Type::V128, src);
        self.value(Op::Vec(VecOp::Insert64(false, false), dst, lo))
    }
}
