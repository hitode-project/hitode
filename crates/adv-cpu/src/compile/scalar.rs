//! Scalar arithmetic and flag consumers used by profiled guest loops.
use super::*;
use iced_x86::Code;

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
        M::Cdq => i.code() == Code::Cdq,
        M::Leave => i.code() == Code::Leaved,
        M::Adc | M::Sbb => {
            i.op_count() == 2
                && source32(i, 0)
                && (source32(i, 1)
                    || matches!(i.op1_kind(), OpKind::Immediate32 | OpKind::Immediate8to32))
        }
        M::Mul | M::Div | M::Idiv => i.op_count() == 1 && source32(i, 0),
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
                && (reg_index(i.op0_register()).is_some()
                    || i.op0_register().is_gpr8()
                    || i.op0_register().is_gpr16())
                && (i.op1_kind() == OpKind::Immediate8
                    || (i.op1_kind() == OpKind::Register && i.op1_register() == R::CL))
        }
        M::Rol | M::Ror => {
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
    /// 8/16-bit ADD/SUB/AND/OR/XOR/CMP/TEST/INC/DEC. Lazy flags keep the
    /// operands shifted to bit 31 and record the shift in the kind's high byte,
    /// so ZF/SF/CF/OF need no width; PF and AF shift the result back.
    pub(super) fn narrow_alu(&mut self, i: &Instruction, size: usize) {
        let shift = (32 - size * 8) as i64;
        let counter = self.counting(i);
        // The whole counter register: the shifts below drop its high half.
        let a = match counter {
            Some(counter) => self.b.use_var(self.regs[counter]),
            None => self.read(i, 0),
        };
        let b = if i.op_count() == 2 {
            self.read(i, 1)
        } else {
            self.constant(1)
        };
        // Validate the destination before changing flags or registers.
        let ptr = if matches!(i.mnemonic(), M::Cmp | M::Test) {
            None
        } else {
            self.destination(i)
        };
        let result = match i.mnemonic() {
            M::Add | M::Inc => self.b.ins().iadd(a, b),
            M::Sub | M::Cmp | M::Dec => self.b.ins().isub(a, b),
            M::And | M::Test => self.b.ins().band(a, b),
            M::Or => self.b.ins().bor(a, b),
            M::Xor => self.b.ins().bxor(a, b),
            _ => unreachable!(),
        };
        let subtract = matches!(i.mnemonic(), M::Sub | M::Cmp | M::Dec);
        let logical = matches!(i.mnemonic(), M::And | M::Or | M::Xor | M::Test);
        let wide_a = self.b.ins().ishl_imm(a, shift);
        let wide_b = self.b.ins().ishl_imm(b, shift);
        let wide = self.b.ins().ishl_imm(result, shift);
        self.flags(
            wide_a,
            wide_b,
            wide,
            subtract,
            logical,
            matches!(i.mnemonic(), M::Inc | M::Dec),
        );
        let kind = if logical {
            3
        } else if subtract {
            2
        } else {
            1
        };
        let kind = self.constant(kind | (shift as u32) << 8);
        self.b.def_var(self.lazy[3], kind);
        if let Some(counter) = counter {
            self.b.def_var(self.regs[counter], result);
        } else if !matches!(i.mnemonic(), M::Cmp | M::Test) {
            self.write(i, ptr, result);
        }
    }

    /// Replace CF and OF while keeping the other arithmetic flags, without
    /// computing any of them: kind bit 4 puts OF in b and folds the old b into
    /// a, so AF's a ^ b ^ result is unchanged and PF/ZF/SF stay as they were.
    fn set_carry_overflow(&mut self, carry: Value, overflow: Value) {
        let a = self.b.use_var(self.lazy[0]);
        let b = self.b.use_var(self.lazy[1]);
        let a = self.b.ins().bxor(a, b);
        let kind = self.b.use_var(self.lazy[3]);
        let kind = self.b.ins().bor_imm(kind, 4);
        self.b.def_var(self.lazy[0], a);
        self.b.def_var(self.lazy[1], overflow);
        self.b.def_var(self.lazy[3], kind);
        let zero = self.constant(0);
        for (var, value) in self.lazy[4..].iter().zip([carry, zero, zero]) {
            self.b.def_var(*var, value);
        }
    }

    fn flags_state(&mut self) -> [Value; 8] {
        let mut state = [self.b.use_var(self.flags); 8];
        for (value, var) in state[1..].iter_mut().zip(self.lazy) {
            *value = self.b.use_var(var);
        }
        state
    }

    /// A zero masked count leaves every flag unchanged.
    fn keep_flags_if(&mut self, keep: Value, saved: [Value; 8]) {
        let mut vars = [self.flags; 8];
        vars[1..].copy_from_slice(&self.lazy);
        for (var, old) in vars.into_iter().zip(saved) {
            let new = self.b.use_var(var);
            let value = self.b.ins().select(keep, old, new);
            self.b.def_var(var, value);
        }
    }

    pub(super) fn scalar_instruction(&mut self, i: &Instruction) {
        if i.code() == Code::Cdq {
            let value = self.b.use_var(self.regs[0]);
            let sign = self.b.ins().sshr_imm(value, 31);
            self.b.def_var(self.regs[2], sign);
            return;
        }
        if i.code() == Code::Leaved {
            let frame = self.b.use_var(self.regs[5]);
            let saved_frame = self.direct_load(frame, types::I32);
            let stack = self.b.ins().iadd_imm(frame, 4);
            // A faulting frame read must retain both architectural stack registers.
            self.b.def_var(self.regs[5], saved_frame);
            self.b.def_var(self.regs[4], stack);
            return;
        }
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
        if i.mnemonic() == M::Adc {
            self.add_with_carry(i);
            return;
        }
        if i.mnemonic() == M::Div {
            self.unsigned_divide(i);
            return;
        }
        if i.mnemonic() == M::Idiv {
            self.signed_divide(i);
            return;
        }
        if matches!(i.mnemonic(), M::Shl | M::Shr | M::Sar) {
            self.scalar_shift(i);
            return;
        }
        if matches!(i.mnemonic(), M::Rol | M::Ror) {
            self.scalar_rotate(i);
            return;
        }
        let signed = i.mnemonic() == M::Imul;
        let (a, b) = match i.op_count() {
            1 => (self.b.use_var(self.regs[0]), self.read(i, 0)),
            2 => (self.read(i, 0), self.read(i, 1)),
            3 => (self.read(i, 1), self.read(i, 2)),
            _ => unreachable!(),
        };
        // Two 32-bit halves rather than one 64-bit product: AArch64 has a
        // multiply-high, and an unused half costs nothing.
        let low = self.b.ins().imul(a, b);
        let high = if signed {
            self.b.ins().smulhi(a, b)
        } else {
            self.b.ins().umulhi(a, b)
        };
        // The product fits when the high half only extends the low half.
        let extension = if signed {
            self.b.ins().sshr_imm(low, 31)
        } else {
            self.constant(0)
        };
        let overflow = self.b.ins().icmp(IntCC::NotEqual, high, extension);
        if i.op_count() == 1 {
            self.b.def_var(self.regs[0], low);
            self.b.def_var(self.regs[2], high);
        } else {
            self.write(i, None, low);
        }
        // MUL/IMUL only define CF and OF. Preserve the other flags rather than
        // inventing arithmetic flags for the low half of the product.
        let carry = self.b.ins().uextend(types::I32, overflow);
        self.set_carry_overflow(carry, carry);
    }

    fn add_with_carry(&mut self, i: &Instruction) {
        let a = self.read(i, 0);
        let b = self.read(i, 1);
        let ptr = self.destination(i);
        let carry = self.flag(0);
        let sum = self.b.ins().iadd(a, b);
        let result = self.b.ins().iadd(sum, carry);
        // Carry is result < a, or result == a with an incoming carry.
        // Use the original b for AF/OF, including b == UINT32_MAX and CF=1.
        let less = self.b.ins().icmp(IntCC::UnsignedLessThan, result, a);
        let less = self.b.ins().uextend(types::I32, less);
        let equal = self.b.ins().icmp(IntCC::Equal, result, a);
        let equal = self.b.ins().uextend(types::I32, equal);
        let equal_and_carry = self.b.ins().band(equal, carry);
        let carry_out = self.b.ins().bor(less, equal_and_carry);
        self.flags(a, b, result, false, false, true);
        let zero = self.constant(0);
        for (var, value) in self.lazy[4..].iter().zip([carry_out, zero, zero]) {
            self.b.def_var(*var, value);
        }
        self.write(i, ptr, result);
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

    fn signed_divide(&mut self, i: &Instruction) {
        let divisor = self.read(i, 0);
        let low = self.b.use_var(self.regs[0]);
        let high = self.b.use_var(self.regs[2]);
        let zero = self.b.ins().icmp_imm(IntCC::Equal, divisor, 0);
        self.guard(zero, Exit::Fallback, None);
        let sign = self.b.ins().sshr_imm(low, 31);
        let is_narrow = self.b.ins().icmp(IntCC::Equal, high, sign);
        let narrow = self.b.create_block();
        let wide = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, types::I32);
        self.b.append_block_param(done, types::I32);
        self.b.ins().brif(is_narrow, narrow, &[], wide, &[]);

        self.b.switch_to_block(narrow);
        let minimum = self
            .b
            .ins()
            .icmp_imm(IntCC::Equal, low, i64::from(i32::MIN));
        let negative_one = self.b.ins().icmp_imm(IntCC::Equal, divisor, -1);
        let overflow = self.b.ins().band(minimum, negative_one);
        self.guard(overflow, Exit::Fallback, None);
        let quotient = self.b.ins().sdiv(low, divisor);
        let product = self.b.ins().imul(quotient, divisor);
        let remainder = self.b.ins().isub(low, product);
        self.b
            .ins()
            .jump(done, &[quotient.into(), remainder.into()]);

        self.b.switch_to_block(wide);
        let wide_low = self.b.ins().uextend(types::I64, low);
        let wide_high = self.b.ins().uextend(types::I64, high);
        let wide_high = self.b.ins().ishl_imm(wide_high, 32);
        let dividend = self.b.ins().bor(wide_high, wide_low);
        let wide_divisor = self.b.ins().sextend(types::I64, divisor);
        // Guard the host's own exceptional divide before computing a quotient.
        let minimum = self.b.ins().icmp_imm(IntCC::Equal, dividend, i64::MIN);
        let negative_one = self.b.ins().icmp_imm(IntCC::Equal, divisor, -1);
        let overflow = self.b.ins().band(minimum, negative_one);
        self.guard(overflow, Exit::Fallback, None);
        let quotient = self.b.ins().sdiv(dividend, wide_divisor);
        let low_quotient = self.b.ins().ireduce(types::I32, quotient);
        let extended = self.b.ins().sextend(types::I64, low_quotient);
        let overflow = self.b.ins().icmp(IntCC::NotEqual, extended, quotient);
        self.guard(overflow, Exit::Fallback, None);
        let product = self.b.ins().imul(quotient, wide_divisor);
        let remainder = self.b.ins().isub(dividend, product);
        let remainder = self.b.ins().ireduce(types::I32, remainder);
        self.b
            .ins()
            .jump(done, &[low_quotient.into(), remainder.into()]);

        self.b.switch_to_block(done);

        self.b.seal_block(done);
        let quotient = self.b.block_params(done)[0];
        let remainder = self.b.block_params(done)[1];
        self.b.def_var(self.regs[0], quotient);
        self.b.def_var(self.regs[2], remainder);
        // All flags are undefined after IDIV; retain the existing values.
    }

    fn unsigned_divide(&mut self, i: &Instruction) {
        // Read the source before checking #DE, and capture aliased EAX/EDX sources
        // before either destination register changes.
        let divisor = self.read(i, 0);
        let low = self.b.use_var(self.regs[0]);
        let high = self.b.use_var(self.regs[2]);
        // The quotient exceeds UINT32_MAX iff high >= divisor. This includes
        // divisor zero. the interpreter raises the guest exception at the original PC.
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

        self.b.seal_block(done);
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
        let size = i.op0_register().size();
        let bits = (size * 8) as i64;
        // Keep the operation in I32: x86 masks the count to five bits even
        // for a byte/word, so counts >= the operand width must not wrap.
        let signed_input = if i.mnemonic() == M::Sar && size < 4 {
            let narrow = self.b.ins().ireduce(integer_type(size), input);
            self.b.ins().sextend(types::I32, narrow)
        } else {
            input
        };
        let result = match i.mnemonic() {
            M::Shl => self.b.ins().ishl(input, count),
            M::Shr => self.b.ins().ushr(input, count),
            M::Sar => self.b.ins().sshr(signed_input, count),
            _ => unreachable!(),
        };
        let result = if size < 4 {
            self.b.ins().band_imm(result, (1i64 << bits) - 1)
        } else {
            result
        };
        let carry = if i.mnemonic() == M::Shl {
            let width = self.constant(bits as u32);
            let shift = self.b.ins().isub(width, count);
            self.b.ins().ushr(input, shift)
        } else {
            let shift = self.b.ins().iadd_imm(count, -1);
            self.b.ins().ushr(signed_input, shift)
        };
        let carry = self.b.ins().band_imm(carry, 1);
        let carry = if size < 4 && i.mnemonic() != M::Sar {
            // CF is undefined beyond the operand width. Match the
            // interpreter's deterministic convention of retaining it.
            let oversized = self
                .b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, count, bits);
            let old_carry = self.flag(0);
            self.b.ins().select(oversized, old_carry, carry)
        } else {
            carry
        };
        let sign = self.b.ins().ushr_imm(result, bits - 1);
        let overflow = match i.mnemonic() {
            M::Shl => self.b.ins().bxor(sign, carry),
            M::Shr => self.b.ins().ushr_imm(input, bits - 1),
            M::Sar => self.constant(0),
            _ => unreachable!(),
        };
        let one = self.b.ins().icmp_imm(IntCC::Equal, count, 1);
        let old_overflow = self.flag(11);
        let overflow = self.b.ins().select(one, overflow, old_overflow);
        let adjust = self.flag(4);
        let saved = self.flags_state();
        // AF and a multi-bit OF are undefined but retained. The result
        // (shifted to bit 31 like narrow ALU) supplies PF/ZF/SF; a places the
        // old AF where a ^ b ^ result reads it, and kind bit 4 puts OF in b.
        let shift = 32 - bits;
        let wide = self.b.ins().ishl_imm(result, shift);
        let adjust = self.b.ins().ishl_imm(adjust, 4 + shift);
        let adjust = self.b.ins().bxor(adjust, wide);
        let zero = self.constant(0);
        let kind = self.constant(5 | (shift as u32) << 8);
        for (var, value) in self
            .lazy
            .iter()
            .zip([adjust, overflow, wide, kind, carry, zero, zero])
        {
            self.b.def_var(*var, value);
        }
        let no_shift = self.b.ins().icmp_imm(IntCC::Equal, count, 0);
        self.keep_flags_if(no_shift, saved);
        self.write(i, None, result);
    }

    /// ROL/ROR change only CF, and OF for a one-bit rotate; a zero masked
    /// count leaves every flag unchanged.
    fn scalar_rotate(&mut self, i: &Instruction) {
        let input = self.read(i, 0);
        let count = self.read(i, 1);
        let count = self.b.ins().band_imm(count, 31);
        let left = i.mnemonic() == M::Rol;
        let result = if left {
            self.b.ins().rotl(input, count)
        } else {
            self.b.ins().rotr(input, count)
        };
        let sign = self.b.ins().ushr_imm(result, 31);
        let (carry, overflow) = if left {
            let carry = self.b.ins().band_imm(result, 1);
            (carry, self.b.ins().bxor(sign, carry))
        } else {
            let next = self.b.ins().ushr_imm(result, 30);
            let next = self.b.ins().band_imm(next, 1);
            (sign, self.b.ins().bxor(sign, next))
        };
        let old_overflow = self.flag(11);
        let one = self.b.ins().icmp_imm(IntCC::Equal, count, 1);
        let overflow = self.b.ins().select(one, overflow, old_overflow);
        let saved = self.flags_state();
        self.set_carry_overflow(carry, overflow);
        let no_rotate = self.b.ins().icmp_imm(IntCC::Equal, count, 0);
        self.keep_flags_if(no_rotate, saved);
        self.write(i, None, result);
    }
}
