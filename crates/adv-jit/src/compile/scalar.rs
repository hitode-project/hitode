//! Scalar arithmetic and flag consumers used by profiled guest loops.
use super::*;

fn source32(i: &Instruction, operand: u32) -> bool {
    match i.op_kind(operand) {
        OpKind::Register => reg_index(i.op_register(operand)).is_some(),
        OpKind::Memory => {
            i.memory_size().size() == 4
                && [i.memory_base(), i.memory_index()]
                    .iter()
                    .all(|&reg| reg == R::None || reg_index(reg).is_some())
        }
        _ => false,
    }
}

fn set_condition(mnemonic: M) -> Option<M> {
    Some(match mnemonic {
        M::Sete => M::Je,
        M::Setne => M::Jne,
        M::Setb => M::Jb,
        M::Setae => M::Jae,
        M::Setbe => M::Jbe,
        M::Seta => M::Ja,
        M::Setl => M::Jl,
        M::Setge => M::Jge,
        M::Setle => M::Jle,
        M::Setg => M::Jg,
        M::Seto => M::Jo,
        M::Setno => M::Jno,
        M::Sets => M::Js,
        M::Setns => M::Jns,
        M::Setp => M::Jp,
        M::Setnp => M::Jnp,
        _ => return None,
    })
}

pub(super) fn supported(i: &Instruction) -> bool {
    if set_condition(i.mnemonic()).is_some() {
        return i.op_count() == 1
            && match i.op0_kind() {
                OpKind::Register => i.op0_register().is_gpr8(),
                OpKind::Memory => {
                    i.memory_size().size() == 1
                        && [i.memory_base(), i.memory_index()]
                            .iter()
                            .all(|&reg| reg == R::None || reg_index(reg).is_some())
                }
                _ => false,
            };
    }
    match i.mnemonic() {
        M::Sbb => {
            i.op_count() == 2
                && source32(i, 0)
                && (source32(i, 1)
                    || matches!(i.op1_kind(), OpKind::Immediate32 | OpKind::Immediate8to32))
        }
        M::Mul | M::Div => i.op_count() == 1 && source32(i, 0),
        M::Imul => match i.op_count() {
            1 => source32(i, 0),
            2 => {
                i.op0_kind() == OpKind::Register
                    && reg_index(i.op0_register()).is_some()
                    && source32(i, 1)
            }
            3 => {
                i.op0_kind() == OpKind::Register
                    && reg_index(i.op0_register()).is_some()
                    && source32(i, 1)
                    && matches!(i.op2_kind(), OpKind::Immediate32 | OpKind::Immediate8to32)
            }
            _ => false,
        },
        M::Shl | M::Shr | M::Sar => {
            i.op_count() == 2
                && i.op0_kind() == OpKind::Register
                && reg_index(i.op0_register()).is_some()
                && (i.op1_kind() == OpKind::Immediate8
                    || (i.op1_kind() == OpKind::Register && i.op1_register() == R::CL))
        }
        _ => false,
    }
}

impl Emitter<'_> {
    pub(super) fn narrow_compare(&mut self, i: &Instruction, size: usize) {
        let a = self.read(i, 0);
        let b = self.read(i, 1);
        let subtract = i.mnemonic() == M::Cmp;
        let result = if subtract {
            self.b.ins().isub(a, b)
        } else {
            self.b.ins().band(a, b)
        };
        let result = self.b.ins().band_imm(result, (1i64 << (size * 8)) - 1);
        let carry = if subtract {
            let value = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
            self.b.ins().uextend(types::I32, value)
        } else {
            self.constant(0)
        };
        let sign = self.b.ins().ushr_imm(result, (size * 8 - 1) as i64);
        let zero = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
        let zero = self.b.ins().uextend(types::I32, zero);
        let byte = self.b.ins().band_imm(result, 255);
        let parity = self.b.ins().popcnt(byte);
        let parity = self.b.ins().band_imm(parity, 1);
        let parity = self.b.ins().bxor_imm(parity, 1);
        let (aux, overflow) = if subtract {
            let ab = self.b.ins().bxor(a, b);
            let ar = self.b.ins().bxor(a, result);
            let overflow = self.b.ins().band(ab, ar);
            let overflow = self.b.ins().ushr_imm(overflow, (size * 8 - 1) as i64);
            let overflow = self.b.ins().band_imm(overflow, 1);
            let aux = self.b.ins().bxor(ab, result);
            let aux = self.b.ins().ushr_imm(aux, 4);
            let aux = self.b.ins().band_imm(aux, 1);
            (aux, overflow)
        } else {
            (self.constant(0), self.constant(0))
        };
        let old = self.materialize_flags();
        let mut flags = self.b.ins().band_imm(old, !0x8d5_i64);
        for (value, bit) in [
            (carry, 0),
            (parity, 2),
            (aux, 4),
            (zero, 6),
            (sign, 7),
            (overflow, 11),
        ] {
            let value = self.b.ins().ishl_imm(value, bit);
            flags = self.b.ins().bor(flags, value);
        }
        self.set_raw_flags(flags);
    }

    fn set_raw_flags(&mut self, flags: Value) {
        self.b.def_var(self.flags, flags);
        let zero = self.constant(0);
        for var in self.lazy {
            self.b.def_var(var, zero);
        }
        let carry = self.b.ins().band_imm(flags, 1);
        self.b.def_var(self.lazy[4], carry);
    }

    pub(super) fn scalar_instruction(&mut self, i: &Instruction) {
        if let Some(condition) = set_condition(i.mnemonic()) {
            let ptr = self.destination(i);
            let value = self.condition(condition);
            let value = self.b.ins().uextend(types::I32, value);
            self.write(i, ptr, value);
            return;
        }
        if i.mnemonic() == M::Sbb {
            self.subtract_with_borrow(i);
            return;
        }
        if i.mnemonic() == M::Div {
            self.unsigned_divide(i);
            return;
        }
        if matches!(i.mnemonic(), M::Shl | M::Shr | M::Sar) {
            self.scalar_shift(i);
            return;
        }
        let signed = i.mnemonic() == M::Imul;
        let (a, b) = match i.op_count() {
            1 => (self.b.use_var(self.regs[0]), self.read(i, 0)),
            2 => (self.read(i, 0), self.read(i, 1)),
            3 => (self.read(i, 1), self.read(i, 2)),
            _ => unreachable!(),
        };
        let a = if signed {
            self.b.ins().sextend(types::I64, a)
        } else {
            self.b.ins().uextend(types::I64, a)
        };
        let b = if signed {
            self.b.ins().sextend(types::I64, b)
        } else {
            self.b.ins().uextend(types::I64, b)
        };
        let product = self.b.ins().imul(a, b);
        let low = self.b.ins().ireduce(types::I32, product);
        let high = self.b.ins().ushr_imm(product, 32);
        let overflow = if signed {
            let extended = self.b.ins().sextend(types::I64, low);
            self.b.ins().icmp(IntCC::NotEqual, product, extended)
        } else {
            self.b.ins().icmp_imm(IntCC::NotEqual, high, 0)
        };
        if i.op_count() == 1 {
            let high = self.b.ins().ireduce(types::I32, high);
            self.b.def_var(self.regs[0], low);
            self.b.def_var(self.regs[2], high);
        } else {
            self.write(i, None, low);
        }
        // MUL/IMUL only define CF and OF. Preserve the other flags rather than
        // inventing arithmetic flags for the low half of the product.
        let old = self.materialize_flags();
        let flags = self.b.ins().band_imm(old, !0x801_i64);
        let carry = self.b.ins().uextend(types::I32, overflow);
        let overflow = self.b.ins().ishl_imm(carry, 11);
        let bits = self.b.ins().bor(carry, overflow);
        let flags = self.b.ins().bor(flags, bits);
        self.set_raw_flags(flags);
    }

    fn subtract_with_borrow(&mut self, i: &Instruction) {
        let a = self.read(i, 0);
        let b = self.read(i, 1);
        // Validate a memory destination before changing any architectural state.
        let ptr = self.destination(i);
        let carry = self.flag(0);
        let difference = self.b.ins().isub(a, b);
        let result = self.b.ins().isub(difference, carry);
        // CF must account for a borrow even when b + carry wraps at UINT32_MAX.
        let less = self.b.ins().icmp(IntCC::UnsignedLessThan, a, b);
        let less = self.b.ins().uextend(types::I32, less);
        let equal = self.b.ins().icmp(IntCC::Equal, a, b);
        let equal = self.b.ins().uextend(types::I32, equal);
        let equal_and_carry = self.b.ins().band(equal, carry);
        let borrow = self.b.ins().bor(less, equal_and_carry);
        // The original operands and final result also give SBB's AF and OF.
        self.flags(a, b, result, true, false, false);
        let zero = self.constant(0);
        for (var, value) in self.lazy[4..].iter().zip([borrow, zero, zero]) {
            self.b.def_var(*var, value);
        }
        self.write(i, ptr, result);
    }

    fn unsigned_divide(&mut self, i: &Instruction) {
        // Read the source before checking #DE, and capture aliased EAX/EDX sources
        // before either destination register changes.
        let divisor = self.read(i, 0);
        let low = self.b.use_var(self.regs[0]);
        let high = self.b.use_var(self.regs[2]);
        // The quotient exceeds UINT32_MAX iff high >= divisor. This includes
        // divisor zero. Unicorn raises the guest exception at the original PC.
        let bad = self
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, high, divisor);
        self.guard(bad, Exit::Fallback, None);
        let narrow = self.b.create_block();
        let wide = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I32);
        self.b.append_block_param(done, types::I32);
        let zero_high = self.b.ins().icmp_imm(IntCC::Equal, high, 0);
        self.b.ins().brif(zero_high, narrow, &[], wide, &[]);

        self.b.switch_to_block(narrow);
        let quotient = self.b.ins().udiv(low, divisor);
        let product = self.b.ins().imul(quotient, divisor);
        let remainder = self.b.ins().isub(low, product);
        self.b
            .ins()
            .jump(done, &[quotient.into(), remainder.into()]);

        self.b.switch_to_block(wide);
        let low = self.b.ins().uextend(types::I64, low);
        let high = self.b.ins().uextend(types::I64, high);
        let high = self.b.ins().ishl_imm(high, 32);
        let dividend = self.b.ins().bor(high, low);
        let divisor = self.b.ins().uextend(types::I64, divisor);
        let quotient = self.b.ins().udiv(dividend, divisor);
        let product = self.b.ins().imul(quotient, divisor);
        let remainder = self.b.ins().isub(dividend, product);
        let quotient = self.b.ins().ireduce(types::I32, quotient);
        let remainder = self.b.ins().ireduce(types::I32, remainder);
        self.b
            .ins()
            .jump(done, &[quotient.into(), remainder.into()]);

        self.b.switch_to_block(done);
        let quotient = self.b.block_params(done)[0];
        let remainder = self.b.block_params(done)[1];
        self.b.def_var(self.regs[0], quotient);
        self.b.def_var(self.regs[2], remainder);
        // DIV leaves arithmetic flags undefined; keep the prior values.
    }

    fn scalar_shift(&mut self, i: &Instruction) {
        let input = self.read(i, 0);
        let count = self.read(i, 1);
        let count = self.b.ins().band_imm(count, 31);
        let result = match i.mnemonic() {
            M::Shl => self.b.ins().ishl(input, count),
            M::Shr => self.b.ins().ushr(input, count),
            M::Sar => self.b.ins().sshr(input, count),
            _ => unreachable!(),
        };
        let carry = if i.mnemonic() == M::Shl {
            let width = self.constant(32);
            let shift = self.b.ins().isub(width, count);
            self.b.ins().ushr(input, shift)
        } else {
            let shift = self.b.ins().iadd_imm(count, -1);
            self.b.ins().ushr(input, shift)
        };
        let carry = self.b.ins().band_imm(carry, 1);
        let sign = self.b.ins().ushr_imm(result, 31);
        let overflow = match i.mnemonic() {
            M::Shl => self.b.ins().bxor(sign, carry),
            M::Shr => self.b.ins().ushr_imm(input, 31),
            M::Sar => self.constant(0),
            _ => unreachable!(),
        };
        let old = self.materialize_flags();
        let old_overflow = self.b.ins().ushr_imm(old, 11);
        let old_overflow = self.b.ins().band_imm(old_overflow, 1);
        let one = self.b.ins().icmp_imm(IntCC::Equal, count, 1);
        let overflow = self.b.ins().select(one, overflow, old_overflow);
        let overflow = self.b.ins().ishl_imm(overflow, 11);
        let byte = self.b.ins().band_imm(result, 255);
        let parity = self.b.ins().popcnt(byte);
        let parity = self.b.ins().band_imm(parity, 1);
        let parity = self.b.ins().bxor_imm(parity, 1);
        let parity = self.b.ins().ishl_imm(parity, 2);
        let zero = self.b.ins().icmp_imm(IntCC::Equal, result, 0);
        let zero = self.b.ins().uextend(types::I32, zero);
        let zero = self.b.ins().ishl_imm(zero, 6);
        let sign = self.b.ins().ishl_imm(sign, 7);
        let mut flags = self.b.ins().band_imm(old, !0x8c5_i64);
        for bits in [carry, parity, zero, sign, overflow] {
            flags = self.b.ins().bor(flags, bits);
        }
        let no_shift = self.b.ins().icmp_imm(IntCC::Equal, count, 0);
        let flags = self.b.ins().select(no_shift, old, flags);
        self.write(i, None, result);
        self.set_raw_flags(flags);
    }
}
