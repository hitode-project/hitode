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
    let mm = |n: u32| i.op_kind(n) == OpKind::Register && register(i.op_register(n)).is_some();
    let gpr = |n: u32| {
        i.op_kind(n) == OpKind::Register
            && i.op_register(n).size() == 4
            && reg_index(i.op_register(n)).is_some()
    };
    let memory = |n: u32, size: usize| {
        i.op_kind(n) == OpKind::Memory
            && i.memory_size().size() == size
            && [i.memory_base(), i.memory_index()]
                .iter()
                .all(|&r| r == R::None || reg_index(r).is_some())
    };
    let source = |n: u32| mm(n) || memory(n, 8);
    let immediate = |n: u32| i.op_kind(n) == OpKind::Immediate8;
    let count = i.op_count();
    match i.mnemonic() {
        M::Emms => count == 0,
        M::Movd => {
            count == 2 && (mm(0) && (gpr(1) || memory(1, 4)) || (gpr(0) || memory(0, 4)) && mm(1))
        }
        M::Movq => count == 2 && (mm(0) && source(1) || memory(0, 8) && mm(1)),
        M::Movntq => count == 2 && memory(0, 8) && mm(1),
        M::Psrlw | M::Psrld | M::Psrlq | M::Psllw | M::Pslld | M::Psllq | M::Psraw | M::Psrad => {
            count == 2 && mm(0) && (source(1) || immediate(1))
        }
        M::Pshufw => count == 3 && mm(0) && source(1) && immediate(2),
        M::Pextrw => count == 3 && gpr(0) && mm(1) && immediate(2),
        M::Pinsrw => count == 3 && mm(0) && (gpr(1) || memory(1, 2)) && immediate(2),
        M::Pmovmskb => count == 2 && gpr(0) && mm(1),
        M::Pavgb
        | M::Pavgw
        | M::Pxor
        | M::Pand
        | M::Pandn
        | M::Por
        | M::Paddb
        | M::Paddw
        | M::Paddd
        | M::Paddq
        | M::Psubb
        | M::Psubw
        | M::Psubd
        | M::Psubq
        | M::Paddsb
        | M::Paddsw
        | M::Psubsb
        | M::Psubsw
        | M::Paddusb
        | M::Paddusw
        | M::Psubusb
        | M::Psubusw
        | M::Pcmpeqb
        | M::Pcmpeqw
        | M::Pcmpeqd
        | M::Pcmpgtb
        | M::Pcmpgtw
        | M::Pcmpgtd
        | M::Pmullw
        | M::Pmulhw
        | M::Pmulhuw
        | M::Pmuludq
        | M::Pmaddwd
        | M::Pmaxsw
        | M::Pminsw
        | M::Pmaxub
        | M::Pminub
        | M::Psadbw
        | M::Punpcklbw
        | M::Punpckhbw
        | M::Punpcklwd
        | M::Punpckhwd
        | M::Punpckldq
        | M::Punpckhdq
        | M::Packuswb
        | M::Packsswb
        | M::Packssdw => count == 2 && mm(0) && source(1),
        _ => false,
    }
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
    fn set_mmx(&mut self, i: &Instruction, value: Value) {
        let value = self.mmx_cast(value, types::I8X16);
        self.b.def_var(
            self.mmx[register(i.op0_register()).unwrap()].unwrap(),
            value,
        );
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
        } else if matches!(i.mnemonic(), M::Movq | M::Movntq) {
            let value = self.mmx_read(i, 1);
            if i.op0_kind() == OpKind::Memory {
                let addr = self.address(i);
                let value = self.mmx_cast(value, types::I64X2);
                let bits = self.b.ins().extractlane(value, 0);
                self.direct_store(addr, bits);
            } else {
                self.set_mmx(i, value);
            }
        } else if i.mnemonic() == M::Pextrw {
            let value = self.mmx_read(i, 1);
            let value = self.mmx_cast(value, types::I16X8);
            let lane = self.b.ins().extractlane(value, i.immediate8() & 3);
            let value = self.b.ins().uextend(types::I32, lane);
            self.write(i, None, value);
        } else if i.mnemonic() == M::Pmovmskb {
            let value = self.mmx_read(i, 1);
            let bits = self.b.ins().vhigh_bits(types::I32, value);
            let bits = self.b.ins().band_imm(bits, 0xff);
            self.write(i, None, bits);
        } else if i.mnemonic() == M::Pinsrw {
            let a = self.mmx_read(i, 0);
            let a = self.mmx_cast(a, types::I16X8);
            let word = self.read(i, 1);
            let word = self.b.ins().ireduce(types::I16, word);
            let value = self.b.ins().insertlane(a, word, i.immediate8() & 3);
            self.set_mmx(i, value);
        } else if i.mnemonic() == M::Pshufw {
            let b = self.mmx_read(i, 1);
            let order = i.immediate8();
            let mask = (0..4)
                .flat_map(|lane| {
                    let word = (order >> (lane * 2)) & 3;
                    [word * 2, word * 2 + 1]
                })
                .collect();
            let value = self.mmx_shuffle(b, b, mask);
            self.set_mmx(i, value);
        } else if matches!(
            i.mnemonic(),
            M::Psrlw | M::Psrld | M::Psrlq | M::Psllw | M::Pslld | M::Psllq | M::Psraw | M::Psrad
        ) {
            let ty = match i.mnemonic() {
                M::Psrlw | M::Psllw | M::Psraw => types::I16X8,
                M::Psrld | M::Pslld | M::Psrad => types::I32X4,
                _ => types::I64X2,
            };
            let a = self.mmx_read(i, 0);
            let a = self.mmx_cast(a, ty);
            let bits = ty.lane_bits();
            let arithmetic = matches!(i.mnemonic(), M::Psraw | M::Psrad);
            let left = matches!(i.mnemonic(), M::Psllw | M::Pslld | M::Psllq);
            let value = if i.op1_kind() == OpKind::Immediate8 {
                let count = u32::from(i.immediate8());
                if count >= bits && !arithmetic {
                    self.b.ins().bxor(a, a)
                } else {
                    // Arithmetic shifts saturate the count to fill with the sign.
                    let count = self.constant(count.min(bits - 1));
                    if arithmetic {
                        self.b.ins().sshr(a, count)
                    } else if left {
                        self.b.ins().ishl(a, count)
                    } else {
                        self.b.ins().ushr(a, count)
                    }
                }
            } else {
                // The whole 64-bit operand is the count; host vector shifts mask it.
                let count = self.mmx_read(i, 1);
                let count = self.mmx_cast(count, types::I64X2);
                let count = self.b.ins().extractlane(count, 0);
                let large =
                    self.b
                        .ins()
                        .icmp_imm(IntCC::UnsignedGreaterThan, count, i64::from(bits - 1));
                let small = self.b.ins().ireduce(types::I32, count);
                let top = self.constant(bits - 1);
                let count = self.b.ins().select(large, top, small);
                if arithmetic {
                    self.b.ins().sshr(a, count)
                } else {
                    let shifted = if left {
                        self.b.ins().ishl(a, count)
                    } else {
                        self.b.ins().ushr(a, count)
                    };
                    let zero = self.b.ins().bxor(a, a);
                    self.b.ins().select(large, zero, shifted)
                }
            };
            self.set_mmx(i, value);
        } else {
            let a = self.mmx_read(i, 0);
            let b = self.mmx_read(i, 1);
            let ty = match i.mnemonic() {
                M::Pavgw
                | M::Paddw
                | M::Psubw
                | M::Paddsw
                | M::Psubsw
                | M::Paddusw
                | M::Psubusw
                | M::Pcmpeqw
                | M::Pcmpgtw
                | M::Pmullw
                | M::Pmulhw
                | M::Pmulhuw
                | M::Pmaddwd
                | M::Pmaxsw
                | M::Pminsw
                | M::Packuswb
                | M::Packsswb => types::I16X8,
                M::Paddd | M::Psubd | M::Pcmpeqd | M::Pcmpgtd | M::Pmuludq | M::Packssdw => {
                    types::I32X4
                }
                M::Paddq | M::Psubq => types::I64X2,
                _ => types::I8X16,
            };
            let a = self.mmx_cast(a, ty);
            let b = self.mmx_cast(b, ty);
            let result = match i.mnemonic() {
                M::Pavgb | M::Pavgw => self.b.ins().avg_round(a, b),
                M::Pxor => self.b.ins().bxor(a, b),
                M::Pand => self.b.ins().band(a, b),
                M::Por => self.b.ins().bor(a, b),
                M::Pandn => {
                    let inverted = self.b.ins().bnot(a);
                    self.b.ins().band(inverted, b)
                }
                M::Paddb | M::Paddw | M::Paddd | M::Paddq => self.b.ins().iadd(a, b),
                M::Psubb | M::Psubw | M::Psubd | M::Psubq => self.b.ins().isub(a, b),
                M::Paddsb | M::Paddsw => self.b.ins().sadd_sat(a, b),
                M::Psubsb | M::Psubsw => self.b.ins().ssub_sat(a, b),
                M::Paddusb | M::Paddusw => self.b.ins().uadd_sat(a, b),
                M::Psubusb | M::Psubusw => self.b.ins().usub_sat(a, b),
                M::Pcmpeqb | M::Pcmpeqw | M::Pcmpeqd => self.b.ins().icmp(IntCC::Equal, a, b),
                M::Pcmpgtb | M::Pcmpgtw | M::Pcmpgtd => {
                    self.b.ins().icmp(IntCC::SignedGreaterThan, a, b)
                }
                M::Pmaxsw => self.b.ins().smax(a, b),
                M::Pminsw => self.b.ins().smin(a, b),
                M::Pmaxub => self.b.ins().umax(a, b),
                M::Pminub => self.b.ins().umin(a, b),
                M::Pmullw => self.b.ins().imul(a, b),
                // Only the low four words are architectural, so one widening suffices.
                M::Pmulhw => {
                    let a = self.b.ins().swiden_low(a);
                    let b = self.b.ins().swiden_low(b);
                    let product = self.b.ins().imul(a, b);
                    let high = self.b.ins().sshr_imm(product, 16);
                    self.b.ins().snarrow(high, high)
                }
                M::Pmulhuw => {
                    let a = self.b.ins().uwiden_low(a);
                    let b = self.b.ins().uwiden_low(b);
                    let product = self.b.ins().imul(a, b);
                    let high = self.b.ins().ushr_imm(product, 16);
                    self.b.ins().unarrow(high, high)
                }
                M::Pmaddwd => {
                    let a = self.b.ins().swiden_low(a);
                    let b = self.b.ins().swiden_low(b);
                    let product = self.b.ins().imul(a, b);
                    self.b.ins().iadd_pairwise(product, product)
                }
                M::Pmuludq => {
                    let a = self.b.ins().extractlane(a, 0);
                    let b = self.b.ins().extractlane(b, 0);
                    let a = self.b.ins().uextend(types::I64, a);
                    let b = self.b.ins().uextend(types::I64, b);
                    let product = self.b.ins().imul(a, b);
                    self.b.ins().scalar_to_vector(types::I64X2, product)
                }
                M::Psadbw => {
                    let high = self.b.ins().umax(a, b);
                    let low = self.b.ins().umin(a, b);
                    let difference = self.b.ins().isub(high, low);
                    let words = self.b.ins().uwiden_low(difference);
                    let pairs = self.b.ins().iadd_pairwise(words, words);
                    let quads = self.b.ins().iadd_pairwise(pairs, pairs);
                    let sum = self.b.ins().iadd_pairwise(quads, quads);
                    let sum = self.b.ins().extractlane(sum, 0);
                    let sum = self.b.ins().uextend(types::I64, sum);
                    self.b.ins().scalar_to_vector(types::I64X2, sum)
                }
                M::Packuswb | M::Packsswb | M::Packssdw => {
                    let value = if i.mnemonic() == M::Packuswb {
                        self.b.ins().unarrow(a, b)
                    } else {
                        self.b.ins().snarrow(a, b)
                    };
                    let value = self.mmx_cast(value, types::I8X16);
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
                    let base = if matches!(i.mnemonic(), M::Punpckhbw | M::Punpckhwd | M::Punpckhdq)
                    {
                        4
                    } else {
                        0
                    };
                    let mask = (0..4 / width)
                        .flat_map(|lane| {
                            (0..width)
                                .map(move |byte| base + lane * width + byte)
                                .chain((0..width).map(move |byte| 16 + base + lane * width + byte))
                        })
                        .collect();
                    self.mmx_shuffle(a, b, mask)
                }
                _ => unreachable!(),
            };
            self.set_mmx(i, result);
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
