//! MMX uses the low 64 bits of host vectors and preserves its x87 alias at boundaries.
use super::*;

/// A guest block's ordered MMX effects, composed while emitting instructions.
/// Only a control/host/fault boundary needs the corresponding runtime value.
pub(super) struct Effects {
    base: Value,
    and_mask: u32,
    or_mask: u32,
}

pub(super) fn register(reg: R) -> Option<usize> {
    Some(match reg {
        R::MM0 => 0,
        R::MM1 => 1,
        R::MM2 => 2,
        R::MM3 => 3,
        R::MM4 => 4,
        R::MM5 => 5,
        R::MM6 => 6,
        R::MM7 => 7,
        _ => return None,
    })
}
pub(super) fn supported(i: &Instruction) -> bool {
    if i.mnemonic() == M::Emms {
        return i.op_count() == 0;
    }
    if !matches!(
        i.mnemonic(),
        M::Movq
            | M::Movd
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
            | M::Punpcklwd
            | M::Punpckhwd
            | M::Punpckldq
            | M::Punpckhdq
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
    if i.mnemonic() == M::Movd {
        let mm = |n| i.op_kind(n) == OpKind::Register && register(i.op_register(n)).is_some();
        let scalar = |n| match i.op_kind(n) {
            OpKind::Register => reg_index(i.op_register(n)).is_some(),
            OpKind::Memory => {
                i.memory_size().size() == 4
                    && [i.memory_base(), i.memory_index()]
                        .iter()
                        .all(|&r| r == R::None || reg_index(r).is_some())
            }
            _ => false,
        };
        return (mm(0) && scalar(1)) || (scalar(0) && mm(1));
    }
    let shift = matches!(i.mnemonic(), M::Psrlw | M::Psrld | M::Psllw | M::Pslld);
    let mut has_mmx = false;
    for n in 0..2 {
        match i.op_kind(n) {
            OpKind::Register if register(i.op_register(n)).is_some() => has_mmx = true,
            OpKind::Memory
                if i.memory_size().size() == 8
                    && [i.memory_base(), i.memory_index()]
                        .iter()
                        .all(|&r| r == R::None || reg_index(r).is_some())
                    && (n == 1 || i.mnemonic() == M::Movq)
                    && !shift => {}
            OpKind::Immediate8 if n == 1 && shift => {}
            _ => return false,
        }
    }
    has_mmx && (!shift || i.op1_kind() == OpKind::Immediate8)
}
impl Emitter<'_> {
    pub(super) fn begin_mmx_block(&mut self) {
        self.mmx_effects = self.mmx_mode.map(|var| Effects {
            base: self.b.use_var(var),
            and_mask: u32::MAX,
            or_mask: 0,
        });
    }

    pub(super) fn flush_mmx_effects(&mut self) {
        let Some(effects) = &self.mmx_effects else {
            return;
        };
        let mut value = effects.base;
        if effects.and_mask != u32::MAX {
            value = self.b.ins().band_imm(value, i64::from(effects.and_mask));
        }
        if effects.or_mask != 0 {
            value = self.b.ins().bor_imm(value, i64::from(effects.or_mask));
        }
        self.b.def_var(self.mmx_mode.unwrap(), value);
        // Do not consume the recipe: a failed guard and both conditional arms
        // need the same prefix, while the successful path can keep composing.
    }

    fn mmx_cast(&mut self, value: Value, ty: ir::Type) -> Value {
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
    fn mmx_read(&mut self, i: &Instruction, n: u32) -> Value {
        if i.op_kind(n) == OpKind::Memory {
            let addr = self.address(i);
            let bits = self.direct_load(addr, types::I64);
            let value = self.b.ins().scalar_to_vector(types::I64X2, bits);
            self.mmx_cast(value, types::I8X16)
        } else {
            self.b
                .use_var(self.mmx[register(i.op_register(n)).unwrap()].unwrap())
        }
    }
    fn mmx_shuffle(&mut self, a: Value, b: Value, mut mask: Vec<u8>) -> Value {
        mask.resize(16, 0);
        let imm = self
            .b
            .func
            .dfg
            .immediates
            .push(ir::ConstantData::from(mask));
        self.b.ins().shuffle(a, b, imm)
    }
    pub(super) fn mmx_instruction(&mut self, i: &Instruction) {
        if i.mnemonic() == M::Emms {
            self.mmx_effects.as_mut().unwrap().or_mask |= 2;
            return;
        }
        if i.mnemonic() == M::Movd {
            if register(i.op0_register()).is_some() {
                let bits = self.read(i, 1);
                let bits = self.b.ins().uextend(types::I64, bits);
                let value = self.b.ins().scalar_to_vector(types::I64X2, bits);
                let value = self.mmx_cast(value, types::I8X16);
                self.b.def_var(
                    self.mmx[register(i.op0_register()).unwrap()].unwrap(),
                    value,
                );
            } else {
                let value = self.mmx_read(i, 1);
                let value = self.mmx_cast(value, types::I32X4);
                let bits = self.b.ins().extractlane(value, 0);
                let ptr = self.destination(i);
                self.write(i, ptr, bits);
            }
        } else if i.mnemonic() == M::Movq {
            let value = self.mmx_read(i, 1);
            if i.op0_kind() == OpKind::Memory {
                let addr = self.address(i);
                let value = self.mmx_cast(value, types::I64X2);
                let bits = self.b.ins().extractlane(value, 0);
                self.direct_store(addr, bits);
            } else {
                self.b.def_var(
                    self.mmx[register(i.op0_register()).unwrap()].unwrap(),
                    value,
                );
            }
        } else {
            let a = self.mmx_read(i, 0);
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
            let a = self.mmx_cast(a, ty);
            let result = if matches!(i.mnemonic(), M::Psrlw | M::Psrld | M::Psllw | M::Pslld) {
                let count = u32::from(i.immediate8());
                if count >= ty.lane_bits() {
                    self.b.ins().bxor(a, a)
                } else {
                    let count = self.constant(count);
                    if matches!(i.mnemonic(), M::Psrlw | M::Psrld) {
                        self.b.ins().ushr(a, count)
                    } else {
                        self.b.ins().ishl(a, count)
                    }
                }
            } else {
                let b = self.mmx_read(i, 1);
                let b = self.mmx_cast(b, ty);
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
                        let al = self.b.ins().uwiden_low(a);
                        let ah = self.b.ins().uwiden_high(a);
                        let bl = self.b.ins().uwiden_low(b);
                        let bh = self.b.ins().uwiden_high(b);
                        let lo = self.b.ins().imul(al, bl);
                        let hi = self.b.ins().imul(ah, bh);
                        let lo = self.b.ins().ushr_imm(lo, 16);
                        let hi = self.b.ins().ushr_imm(hi, 16);
                        self.b.ins().unarrow(lo, hi)
                    }
                    M::Packuswb | M::Packsswb => {
                        let value = if i.mnemonic() == M::Packuswb {
                            self.b.ins().unarrow(a, b)
                        } else {
                            self.b.ins().snarrow(a, b)
                        };
                        self.mmx_shuffle(value, value, vec![0, 1, 2, 3, 8, 9, 10, 11])
                    }
                    M::Punpcklbw
                    | M::Punpckhbw
                    | M::Punpcklwd
                    | M::Punpckhwd
                    | M::Punpckldq
                    | M::Punpckhdq => {
                        let width = match i.mnemonic() {
                            M::Punpcklwd | M::Punpckhwd => 2,
                            M::Punpckldq | M::Punpckhdq => 4,
                            _ => 1,
                        };
                        let base =
                            if matches!(i.mnemonic(), M::Punpckhbw | M::Punpckhwd | M::Punpckhdq) {
                                4
                            } else {
                                0
                            };
                        let mask = (0..4 / width)
                            .flat_map(|lane| {
                                (0..width)
                                    .map(move |byte| base + lane * width + byte)
                                    .chain(
                                        (0..width).map(move |byte| 16 + base + lane * width + byte),
                                    )
                            })
                            .collect();
                        self.mmx_shuffle(a, b, mask)
                    }
                    _ => unreachable!(),
                }
            };
            let result = self.mmx_cast(result, types::I8X16);
            self.b.def_var(
                self.mmx[register(i.op0_register()).unwrap()].unwrap(),
                result,
            );
        }
        // Compose the effect at compile time. Emitting AND/OR for every packed
        // instruction produces a long dependency chain in pixel loops because
        // Cranelift does not combine the alternating operations.
        let effects = self.mmx_effects.as_mut().unwrap();
        effects.and_mask &= !2;
        effects.or_mask &= !2;
        let written = register(i.op0_register()).map_or(0, |slot| 1 << (8 + slot));
        effects.or_mask |= 5 | written;
    }
}
