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
    if i.mnemonic() == M::Pinsrw {
        return i.op_count() == 3
            && register(i.op0_register()).is_some()
            && i.op2_kind() == OpKind::Immediate8
            && (i.op1_kind() == OpKind::Register && reg_index(i.op1_register()).is_some()
                || i.op1_kind() == OpKind::Memory
                    && i.memory_size().size() == 2
                    && [i.memory_base(), i.memory_index()]
                        .iter()
                        .all(|&r| r == R::None || reg_index(r).is_some()));
    }
    if i.mnemonic() == M::Maskmovdqu {
        return i.op_count() == 3
            && i.op0_kind() == OpKind::MemorySegEDI
            && register(i.op1_register()).is_some()
            && register(i.op2_register()).is_some();
    }
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
            | M::Punpcklwd
            | M::Punpckhwd
            | M::Packuswb
            | M::Packsswb
            | M::Packssdw
            | M::Psrlw
            | M::Psrld
            | M::Psllw
            | M::Pslld
            | M::Psraw
            | M::Psrad
    ) || i.op_count() != 2
    {
        return false;
    }
    let shift = matches!(
        i.mnemonic(),
        M::Psrlw | M::Psrld | M::Psllw | M::Pslld | M::Psraw | M::Psrad
    );
    for n in 0..2 {
        match i.op_kind(n) {
            OpKind::Register if register(i.op_register(n)).is_some() => {}
            OpKind::Memory
                if i.memory_size().size() == 16
                    && [i.memory_base(), i.memory_index()]
                        .iter()
                        .all(|&r| r == R::None || reg_index(r).is_some())
                    && (n == 1 || matches!(i.mnemonic(), M::Movdqa | M::Movdqu))
                    && (!shift || matches!(i.mnemonic(), M::Psraw | M::Psrad)) => {}
            OpKind::Immediate8 if n == 1 && shift => {}
            _ => return false,
        }
    }
    !shift || i.op1_kind() == OpKind::Immediate8 || matches!(i.mnemonic(), M::Psraw | M::Psrad)
}

impl Emitter<'_> {
    fn vector_address(&mut self, i: &Instruction) -> Value {
        let addr = self.address(i);
        // Legacy SSE2 arithmetic memory operands and MOVDQA require alignment.
        // Let the interpreter produce its architectural exception before changing state.
        if i.mnemonic() != M::Movdqu {
            let misaligned = self.b.ins().band_imm(addr, 15);
            let bad = self.b.ins().icmp_imm(IntCC::NotEqual, misaligned, 0);
            self.guard(bad, Exit::Fallback, None);
        }
        addr
    }

    fn vector_read(&mut self, i: &Instruction, operand: u32) -> Value {
        if i.op_kind(operand) == OpKind::Memory {
            let addr = self.vector_address(i);
            self.direct_load(addr, types::I8X16)
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
        if i.mnemonic() == M::Maskmovdqu {
            self.simd_masked_store(i);
            return;
        }
        if i.mnemonic() == M::Pinsrw {
            let previous = self.vector_read(i, 0);
            let previous = self.vector_cast(previous, types::I16X8);
            // The memory form reads only one unaligned word.
            let word = self.read(i, 1);
            let word = self.b.ins().ireduce(types::I16, word);
            let value = self.b.ins().insertlane(previous, word, i.immediate8() & 7);
            let value = self.vector_cast(value, types::I8X16);
            self.b.def_var(
                self.xmm[register(i.op0_register()).unwrap()].unwrap(),
                value,
            );
            return;
        }
        if matches!(i.mnemonic(), M::Movdqa | M::Movdqu) {
            let value = self.vector_read(i, 1);
            if i.op0_kind() == OpKind::Memory {
                let addr = self.vector_address(i);
                self.direct_store(addr, value);
            } else {
                self.b.def_var(
                    self.xmm[register(i.op0_register()).unwrap()].unwrap(),
                    value,
                );
            }
            return;
        }
        let a = self.vector_read(i, 0);
        let shift = matches!(
            i.mnemonic(),
            M::Psrlw | M::Psrld | M::Psllw | M::Pslld | M::Psraw | M::Psrad
        );
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
            | M::Psraw
            | M::Psllw => types::I16X8,
            M::Paddd | M::Psubd | M::Psrld | M::Pslld | M::Psrad | M::Packssdw => types::I32X4,
            _ => types::I8X16,
        };
        let a = self.vector_cast(a, ty);
        let result = if matches!(i.mnemonic(), M::Psraw | M::Psrad) {
            let maximum = ty.lane_bits() - 1;
            let count = if i.op1_kind() == OpKind::Immediate8 {
                self.constant(u32::from(i.immediate8()).min(maximum))
            } else {
                // SSE2 takes the entire low quadword, not a masked lane count.
                let source = self.vector_read(i, 1);
                let source = self.vector_cast(source, types::I64X2);
                let count = self.b.ins().extractlane(source, 0);
                let limit = self.b.ins().iconst(types::I64, i64::from(maximum));
                let count = self.b.ins().umin(count, limit);
                self.b.ins().ireduce(types::I32, count)
            };
            self.b.ins().sshr(a, count)
        } else if shift {
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
                M::Packsswb | M::Packssdw => self.b.ins().snarrow(a, b),
                M::Punpcklwd | M::Punpckhwd => {
                    let first = if i.mnemonic() == M::Punpckhwd { 4 } else { 0 };
                    let mask: Vec<u8> = (first..first + 4u8)
                        .flat_map(|lane| [lane * 2, lane * 2 + 1, 16 + lane * 2, 17 + lane * 2])
                        .collect();
                    let imm = self
                        .b
                        .func
                        .dfg
                        .immediates
                        .push(ir::ConstantData::from(mask));
                    self.b.ins().shuffle(a, b, imm)
                }
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

    /// Prove the whole possible destination before emitting any selected byte.
    /// Nonordinary or partially inaccessible spans retry the transactional
    /// interpreter, which validates only selected bytes and handles SMC/PCM.
    fn simd_masked_store(&mut self, i: &Instruction) {
        let bytes = self.vector_read(i, 1);
        let mask = self.vector_read(i, 2);
        let bits = self.b.ins().vhigh_bits(types::I32, mask);
        let active = self.b.create_block();
        let done = self.b.create_block();
        self.b.ins().brif(bits, active, &[], done, &[]);
        self.b.switch_to_block(active);
        let mut addr = self.b.use_var(self.regs[7]);
        if i.segment_prefix() == R::FS {
            let fs = self.b.ins().load(
                types::I32,
                MemFlags::trusted(),
                self.state,
                offset_of!(CpuState, fs_base) as i32,
            );
            addr = self.b.ins().iadd(addr, fs);
        }
        let offset = self.b.ins().iadd_imm(addr, -i64::from(self.base));
        let outside =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, offset, (self.len - 16) as i64);
        self.guard(outside, Exit::Fallback, None);
        let end = self.b.ins().iadd_imm(offset, 15);
        let first = self.permission(offset);
        let last = self.permission(end);
        let denied = self.denied(&[first, last], WRITE, crate::memory::SHARED | EXEC);
        self.guard(denied, Exit::Fallback, None);
        let address = self.window_address(addr);
        let all = self.b.ins().icmp_imm(IntCC::Equal, bits, 0xffff);
        let full = self.b.create_block();
        let partial = self.b.create_block();
        self.b.ins().brif(all, full, &[], partial, &[]);
        self.b.switch_to_block(full);
        self.b.ins().store(MemFlags::new(), bytes, address, 0);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(partial);
        for lane in 0..16u8 {
            let selected = self.b.ins().band_imm(bits, 1i64 << lane);
            let store = self.b.create_block();
            let next = self.b.create_block();
            self.b.ins().brif(selected, store, &[], next, &[]);
            self.b.switch_to_block(store);
            let byte = self.b.ins().extractlane(bytes, lane);
            self.b
                .ins()
                .store(MemFlags::new(), byte, address, i32::from(lane));
            self.b.ins().jump(next, &[]);
            self.b.switch_to_block(next);
        }
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
    }
}
