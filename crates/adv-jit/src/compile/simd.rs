//! Integer SSE2 lowering for software image composition. All values stay in host
//! vectors; no per-pixel helpers or guest-to-host image copies are involved.
use super::*;

pub(super) fn register(reg: R) -> Option<usize> {
    Some(match reg {
        R::XMM0 => 0,
        R::XMM1 => 1,
        R::XMM2 => 2,
        R::XMM3 => 3,
        R::XMM4 => 4,
        R::XMM5 => 5,
        R::XMM6 => 6,
        R::XMM7 => 7,
        _ => return None,
    })
}

pub(super) fn supported(i: &Instruction) -> bool {
    if !matches!(
        i.mnemonic(),
        M::Movdqu
            | M::Movdqa
            | M::Pavgb
            | M::Pavgw
            | M::Pxor
            | M::Pand
            | M::Pandn
            | M::Por
            | M::Paddb
            | M::Paddw
            | M::Paddd
            | M::Psubb
            | M::Psubw
            | M::Psubd
            | M::Paddusb
            | M::Paddusw
            | M::Psubusb
            | M::Psubusw
            | M::Pmullw
            | M::Pmulhuw
            | M::Punpcklbw
            | M::Punpckhbw
            | M::Packuswb
            | M::Packsswb
            | M::Psrlw
            | M::Psrld
            | M::Psllw
            | M::Pslld
    ) || i.op_count() != 2
    {
        return false;
    }
    let shift = matches!(i.mnemonic(), M::Psrlw | M::Psrld | M::Psllw | M::Pslld);
    for n in 0..2 {
        match i.op_kind(n) {
            OpKind::Register if register(i.op_register(n)).is_some() => {}
            OpKind::Memory
                if i.memory_size().size() == 16
                    && [i.memory_base(), i.memory_index()]
                        .iter()
                        .all(|&r| r == R::None || reg_index(r).is_some())
                    && (n == 1 || matches!(i.mnemonic(), M::Movdqa | M::Movdqu))
                    && !shift => {}
            OpKind::Immediate8 if n == 1 && shift => {}
            _ => return false,
        }
    }
    !shift || i.op1_kind() == OpKind::Immediate8
}

impl Emitter<'_> {
    fn vector_pointer(&mut self, i: &Instruction, write: bool) -> Value {
        let addr = self.address(i);
        // Legacy SSE2 arithmetic memory operands and MOVDQA require alignment.
        // Let Unicorn produce its architectural exception before changing state.
        if i.mnemonic() != M::Movdqu {
            let misaligned = self.b.ins().band_imm(addr, 15);
            let bad = self.b.ins().icmp_imm(IntCC::NotEqual, misaligned, 0);
            self.guard(bad, Exit::Fallback, None);
        }
        self.pointer(addr, write, 16)
    }

    fn vector_read(&mut self, i: &Instruction, operand: u32) -> Value {
        if i.op_kind(operand) == OpKind::Memory {
            let ptr = self.vector_pointer(i, false);
            self.b.ins().load(types::I8X16, MemFlags::new(), ptr, 0)
        } else {
            self.b
                .use_var(self.xmm[register(i.op_register(operand)).unwrap()].unwrap())
        }
    }

    fn vector_cast(&mut self, value: Value, ty: ir::Type) -> Value {
        if self.b.func.dfg.value_type(value) == ty {
            value
        } else {
            self.b.ins().bitcast(
                ty,
                MemFlags::new().with_endianness(ir::Endianness::Little),
                value,
            )
        }
    }

    pub(super) fn simd_instruction(&mut self, i: &Instruction) {
        if matches!(i.mnemonic(), M::Movdqa | M::Movdqu) {
            let value = self.vector_read(i, 1);
            if i.op0_kind() == OpKind::Memory {
                let ptr = self.vector_pointer(i, true);
                self.b.ins().store(MemFlags::new(), value, ptr, 0);
            } else {
                self.b.def_var(
                    self.xmm[register(i.op0_register()).unwrap()].unwrap(),
                    value,
                );
            }
            return;
        }
        let a = self.vector_read(i, 0);
        let shift = matches!(i.mnemonic(), M::Psrlw | M::Psrld | M::Psllw | M::Pslld);
        let ty = match i.mnemonic() {
            M::Pavgw
            | M::Paddw
            | M::Psubw
            | M::Paddusw
            | M::Psubusw
            | M::Pmullw
            | M::Pmulhuw
            | M::Packuswb
            | M::Packsswb
            | M::Psrlw
            | M::Psllw => types::I16X8,
            M::Paddd | M::Psubd | M::Psrld | M::Pslld => types::I32X4,
            _ => types::I8X16,
        };
        let a = self.vector_cast(a, ty);
        let result = if shift {
            let count = i.immediate8();
            if u32::from(count) >= ty.lane_bits() {
                self.b.ins().bxor(a, a)
            } else {
                let count = self.constant(u32::from(count));
                if matches!(i.mnemonic(), M::Psrlw | M::Psrld) {
                    self.b.ins().ushr(a, count)
                } else {
                    self.b.ins().ishl(a, count)
                }
            }
        } else {
            let b = self.vector_read(i, 1);
            let b = self.vector_cast(b, ty);
            match i.mnemonic() {
                M::Pavgb | M::Pavgw => self.b.ins().avg_round(a, b),
                M::Pxor => self.b.ins().bxor(a, b),
                M::Pand => self.b.ins().band(a, b),
                M::Por => self.b.ins().bor(a, b),
                M::Pandn => {
                    let inverted = self.b.ins().bnot(a);
                    self.b.ins().band(inverted, b)
                }
                M::Paddb | M::Paddw | M::Paddd => self.b.ins().iadd(a, b),
                M::Psubb | M::Psubw | M::Psubd => self.b.ins().isub(a, b),
                M::Paddusb | M::Paddusw => self.b.ins().uadd_sat(a, b),
                M::Psubusb | M::Psubusw => self.b.ins().usub_sat(a, b),
                M::Pmullw => self.b.ins().imul(a, b),
                M::Pmulhuw => {
                    // Cranelift 0.125 does not lower vector UMULHI on every backend.
                    // Widen first so each 16x16 product is exact on both x64 and ARM64.
                    let al = self.b.ins().uwiden_low(a);
                    let ah = self.b.ins().uwiden_high(a);
                    let bl = self.b.ins().uwiden_low(b);
                    let bh = self.b.ins().uwiden_high(b);
                    let lo = self.b.ins().imul(al, bl);
                    let hi = self.b.ins().imul(ah, bh);
                    let lo = self.b.ins().ushr_imm(lo, 16);
                    let hi = self.b.ins().ushr_imm(hi, 16);
                    // Shifted values are nonnegative signed i32 and fit in u16.
                    self.b.ins().unarrow(lo, hi)
                }
                M::Packuswb => self.b.ins().unarrow(a, b),
                M::Packsswb => self.b.ins().snarrow(a, b),
                M::Punpcklbw | M::Punpckhbw => {
                    let base = if i.mnemonic() == M::Punpckhbw { 8u8 } else { 0 };
                    let mask: Vec<u8> = (0..8u8)
                        .flat_map(|lane| [base + lane, 16 + base + lane])
                        .collect();
                    let imm = self
                        .b
                        .func
                        .dfg
                        .immediates
                        .push(ir::ConstantData::from(mask));
                    self.b.ins().shuffle(a, b, imm)
                }
                _ => unreachable!(),
            }
        };
        let result = self.vector_cast(result, types::I8X16);
        self.b.def_var(
            self.xmm[register(i.op0_register()).unwrap()].unwrap(),
            result,
        );
    }
}
