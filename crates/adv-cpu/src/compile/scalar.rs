//! Scalar arithmetic and flag consumers used by profiled guest loops.
use super::*;
use iced_x86::{Code, ConditionCode};

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

/// A general register or flat memory operand of the given widths.
fn operand(i: &Instruction, n: u32, sizes: &[usize]) -> bool {
    match i.op_kind(n) {
        OpKind::Register => {
            let reg = i.op_register(n);
            (reg.is_gpr8() || reg.is_gpr16() || reg.is_gpr32()) && sizes.contains(&reg.size())
        }
        OpKind::Memory => {
            sizes.contains(&i.memory_size().size())
                && [i.memory_base(), i.memory_index()]
                    .iter()
                    .all(|&reg| reg == R::None || reg_index(reg).is_some())
        }
        _ => false,
    }
}

fn operand_size(i: &Instruction, n: u32) -> usize {
    if i.op_kind(n) == OpKind::Memory {
        i.memory_size().size()
    } else {
        i.op_register(n).size()
    }
}

/// The conditional jump testing the same flags as a CMOVcc.
fn move_condition(i: &Instruction) -> Option<M> {
    if !matches!(
        i.mnemonic(),
        M::Cmovo
            | M::Cmovno
            | M::Cmovb
            | M::Cmovae
            | M::Cmove
            | M::Cmovne
            | M::Cmovbe
            | M::Cmova
            | M::Cmovs
            | M::Cmovns
            | M::Cmovp
            | M::Cmovnp
            | M::Cmovl
            | M::Cmovge
            | M::Cmovle
            | M::Cmovg
    ) {
        return None;
    }
    Some(match i.condition_code() {
        ConditionCode::o => M::Jo,
        ConditionCode::no => M::Jno,
        ConditionCode::b => M::Jb,
        ConditionCode::ae => M::Jae,
        ConditionCode::e => M::Je,
        ConditionCode::ne => M::Jne,
        ConditionCode::be => M::Jbe,
        ConditionCode::a => M::Ja,
        ConditionCode::s => M::Js,
        ConditionCode::ns => M::Jns,
        ConditionCode::p => M::Jp,
        ConditionCode::np => M::Jnp,
        ConditionCode::l => M::Jl,
        ConditionCode::ge => M::Jge,
        ConditionCode::le => M::Jle,
        ConditionCode::g => M::Jg,
        _ => return None,
    })
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
    if move_condition(i).is_some() {
        return i.op_count() == 2
            && i.op0_kind() == OpKind::Register
            && i.op0_register().is_gpr32()
            && source32(i, 1);
    }
    let count = |i: &Instruction| {
        i.op1_kind() == OpKind::Immediate8
            || (i.op1_kind() == OpKind::Register && i.op1_register() == R::CL)
    };
    let register = |i: &Instruction, n: u32| {
        i.op_kind(n) == OpKind::Register && operand(i, n, &[operand_size(i, 0)])
    };
    match i.mnemonic() {
        M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror => {
            i.op_count() == 2 && operand(i, 0, &[1, 2, 4]) && count(i)
        }
        // Only the one-bit form, which is the one compilers emit.
        M::Rcl | M::Rcr => {
            i.op_count() == 2
                && operand(i, 0, &[1, 2, 4])
                && i.op1_kind() == OpKind::Immediate8
                && i.immediate8() == 1
        }
        // A register bit offset addresses memory outside the operand, which
        // loop versioning would not see; leave that form to the interpreter.
        M::Bt | M::Bts | M::Btr | M::Btc => {
            i.op_count() == 2
                && operand(i, 0, &[4])
                && (i.op1_kind() == OpKind::Immediate8
                    || i.op0_kind() == OpKind::Register && register(i, 1))
        }
        M::Xlatb => i.code() == Code::Xlat_m8 && i.memory_base() == R::EBX,
        M::Xchg => i.op_count() == 2 && operand(i, 0, &[4]) && register(i, 1),
        M::Xadd | M::Cmpxchg => i.op_count() == 2 && operand(i, 0, &[1, 2, 4]) && register(i, 1),
        M::Clc | M::Stc | M::Cmc | M::Cld | M::Std | M::Lahf | M::Sahf => i.op_count() == 0,
        M::Bswap => i.code() == Code::Bswap_r32,
        M::Pushf | M::Pushfd | M::Popf | M::Popfd => matches!(
            i.code(),
            Code::Pushfw | Code::Pushfd | Code::Popfw | Code::Popfd
        ),
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

    /// Make `flags` the whole EFLAGS, with no lazily derived bits.
    pub(super) fn set_raw_flags(&mut self, flags: Value) {
        self.b.def_var(self.flags, flags);
        let zero = self.constant(0);
        for var in self.lazy {
            self.b.def_var(var, zero);
        }
        let carry = self.b.ins().band_imm(flags, 1);
        self.b.def_var(self.lazy[4], carry);
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
        if matches!(i.mnemonic(), M::Clc | M::Stc | M::Cmc) {
            let carry = match i.mnemonic() {
                M::Clc => self.constant(0),
                M::Stc => self.constant(1),
                M::Cmc => {
                    let carry = self.flag(0);
                    self.b.ins().bxor_imm(carry, 1)
                }
                _ => unreachable!(),
            };
            // Only CF changes. Keep the arithmetic producer and raw EFLAGS
            // intact, including any lazily carried OF from a rotate/MUL.
            let zero = self.constant(0);
            for (var, value) in self.lazy[4..].iter().zip([carry, zero, zero]) {
                self.b.def_var(*var, value);
            }
            return;
        }
        if matches!(i.mnemonic(), M::Cld | M::Std) {
            let flags = self.b.use_var(self.flags);
            let flags = if i.mnemonic() == M::Std {
                self.b.ins().bor_imm(flags, 0x400)
            } else {
                self.b.ins().band_imm(flags, !0x400_i64)
            };
            self.b.def_var(self.flags, flags);
            return;
        }
        if i.mnemonic() == M::Lahf {
            let flags = self.materialize_flags();
            let flags = self.b.ins().band_imm(flags, 0xd5);
            let flags = self.b.ins().bor_imm(flags, 2);
            let high = self.b.ins().ishl_imm(flags, 8);
            let eax = self.b.use_var(self.regs[0]);
            let eax = self.b.ins().band_imm(eax, !0xff00_i64);
            let eax = self.b.ins().bor(eax, high);
            self.b.def_var(self.regs[0], eax);
            return;
        }
        if i.mnemonic() == M::Sahf {
            // SF, ZF, AF, PF and CF come from AH; OF is kept.
            let overflow = self.flag(11);
            let overflow = self.b.ins().ishl_imm(overflow, 11);
            let eax = self.b.use_var(self.regs[0]);
            let high = self.b.ins().ushr_imm(eax, 8);
            let loaded = self.b.ins().band_imm(high, 0xd5);
            let flags = self.b.use_var(self.flags);
            let flags = self.b.ins().band_imm(flags, !0x8d5_i64);
            let flags = self.b.ins().bor(flags, loaded);
            let flags = self.b.ins().bor(flags, overflow);
            self.set_raw_flags(flags);
            return;
        }
        if i.mnemonic() == M::Bswap {
            let index = reg_index(i.op0_register()).unwrap();
            let value = self.b.use_var(self.regs[index]);
            let value = self.b.ins().bswap(value);
            self.b.def_var(self.regs[index], value);
            return;
        }
        if matches!(i.code(), Code::Pushfw | Code::Pushfd) {
            let width = if i.code() == Code::Pushfw { 2 } else { 4 };
            // PUSHF stores VM and RF clear.
            let flags = self.materialize_flags();
            let flags = self.b.ins().band_imm(flags, !0x30000_i64);
            let sp = self.b.use_var(self.regs[4]);
            let sp = self.b.ins().iadd_imm(sp, -(width as i64));
            let ptr = self.pointer(sp, width);
            let value = if width == 2 {
                self.b.ins().ireduce(types::I16, flags)
            } else {
                flags
            };
            self.store_memory(ptr, value);
            self.b.def_var(self.regs[4], sp);
            return;
        }
        if matches!(i.code(), Code::Popfw | Code::Popfd) {
            let width: i64 = if i.code() == Code::Popfw { 2 } else { 4 };
            let sp = self.b.use_var(self.regs[4]);
            let value = if width == 2 {
                let word = self.direct_load(sp, types::I16);
                self.b.ins().uextend(types::I32, word)
            } else {
                self.direct_load(sp, types::I32)
            };
            // Setting TF single-steps the next instruction: the interpreter does that.
            let trap = self.b.ins().band_imm(value, 0x100);
            self.guard(trap, Exit::Fallback, None);
            // At CPL3, POPF cannot change IF or IOPL.
            let writable = 0x24_4dd5 & if width == 2 { 0xffff } else { -1 };
            let kept = self.b.use_var(self.flags);
            let kept = self.b.ins().band_imm(kept, !writable);
            let loaded = self.b.ins().band_imm(value, writable);
            let flags = self.b.ins().bor(kept, loaded);
            let flags = self.b.ins().bor_imm(flags, 2);
            self.set_raw_flags(flags);
            let sp = self.b.ins().iadd_imm(sp, width);
            self.b.def_var(self.regs[4], sp);
            return;
        }
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
        match i.mnemonic() {
            M::Xchg => return self.exchange(i),
            M::Xadd => return self.exchange_add(i),
            M::Cmpxchg => return self.compare_exchange(i),
            M::Bt | M::Bts | M::Btr | M::Btc => return self.bit_test(i),
            M::Xlatb => return self.translate(i),
            M::Rcl | M::Rcr => return self.rotate_through_carry(i),
            _ => {}
        }
        if let Some(condition) = move_condition(i) {
            // CMOV reads its source even when the condition is false.
            let value = self.read(i, 1);
            let old = self.read(i, 0);
            let condition = self.condition(condition);
            let value = self.b.ins().select(condition, value, old);
            self.write(i, None, value);
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
        if matches!(i.mnemonic(), M::Shl | M::Sal | M::Shr | M::Sar) {
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
        // The group-2 /6 SAL encoding is SHL.
        let kind = if i.mnemonic() == M::Sal {
            M::Shl
        } else {
            i.mnemonic()
        };
        let input = self.read(i, 0);
        let count = self.read(i, 1);
        // A zero count still requires a writable destination.
        let ptr = self.destination(i);
        let count = self.b.ins().band_imm(count, 31);
        let size = operand_size(i, 0);
        let bits = (size * 8) as i64;
        // Keep the operation in I32: x86 masks the count to five bits even
        // for a byte/word, so counts >= the operand width must not wrap.
        let signed_input = if kind == M::Sar && size < 4 {
            let narrow = self.b.ins().ireduce(integer_type(size), input);
            self.b.ins().sextend(types::I32, narrow)
        } else {
            input
        };
        let result = match kind {
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
        let carry = if kind == M::Shl {
            let width = self.constant(bits as u32);
            let shift = self.b.ins().isub(width, count);
            self.b.ins().ushr(input, shift)
        } else {
            let shift = self.b.ins().iadd_imm(count, -1);
            self.b.ins().ushr(signed_input, shift)
        };
        let carry = self.b.ins().band_imm(carry, 1);
        let carry = if size < 4 && kind != M::Sar {
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
        let overflow = match kind {
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
        self.write(i, ptr, result);
    }

    /// ROL/ROR change only CF, and OF for a one-bit rotate; a zero masked
    /// count leaves every flag unchanged.
    fn scalar_rotate(&mut self, i: &Instruction) {
        let input = self.read(i, 0);
        let count = self.read(i, 1);
        let ptr = self.destination(i);
        let count = self.b.ins().band_imm(count, 31);
        let size = operand_size(i, 0);
        let bits = (size * 8) as i64;
        // Repeat a byte or word across 32 bits: its low part then rotates by
        // the count modulo its own width.
        let repeated = match size {
            1 => self.b.ins().imul_imm(input, 0x0101_0101),
            2 => {
                let high = self.b.ins().ishl_imm(input, 16);
                self.b.ins().bor(input, high)
            }
            _ => input,
        };
        let left = i.mnemonic() == M::Rol;
        let result = if left {
            self.b.ins().rotl(repeated, count)
        } else {
            self.b.ins().rotr(repeated, count)
        };
        let result = if size < 4 {
            self.b.ins().band_imm(result, (1i64 << bits) - 1)
        } else {
            result
        };
        let sign = self.b.ins().ushr_imm(result, bits - 1);
        let (carry, overflow) = if left {
            let carry = self.b.ins().band_imm(result, 1);
            (carry, self.b.ins().bxor(sign, carry))
        } else {
            let next = self.b.ins().ushr_imm(result, bits - 2);
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
        self.write(i, ptr, result);
    }

    /// RCL/RCR by one: the carry rotates in and the bit leaving becomes CF.
    fn rotate_through_carry(&mut self, i: &Instruction) {
        let input = self.read(i, 0);
        let ptr = self.destination(i);
        let bits = (operand_size(i, 0) * 8) as i64;
        let old_carry = self.flag(0);
        let (result, carry) = if i.mnemonic() == M::Rcl {
            let shifted = self.b.ins().ishl_imm(input, 1);
            let result = self.b.ins().bor(shifted, old_carry);
            let carry = self.b.ins().ushr_imm(input, bits - 1);
            (result, self.b.ins().band_imm(carry, 1))
        } else {
            let shifted = self.b.ins().ushr_imm(input, 1);
            let carry_in = self.b.ins().ishl_imm(old_carry, bits - 1);
            let result = self.b.ins().bor(shifted, carry_in);
            (result, self.b.ins().band_imm(input, 1))
        };
        let sign = self.b.ins().ushr_imm(result, bits - 1);
        let sign = self.b.ins().band_imm(sign, 1);
        let overflow = if i.mnemonic() == M::Rcl {
            self.b.ins().bxor(sign, carry)
        } else {
            let next = self.b.ins().ushr_imm(result, bits - 2);
            let next = self.b.ins().band_imm(next, 1);
            self.b.ins().bxor(sign, next)
        };
        self.set_carry_overflow(carry, overflow);
        self.write(i, ptr, result);
    }

    /// Flags of an ADD or SUB of the given width, as `narrow_alu` records them.
    fn arithmetic_flags(&mut self, a: Value, b: Value, result: Value, size: usize, subtract: bool) {
        let shift = (32 - size * 8) as i64;
        let a = self.b.ins().ishl_imm(a, shift);
        let b = self.b.ins().ishl_imm(b, shift);
        let result = self.b.ins().ishl_imm(result, shift);
        self.flags(a, b, result, subtract, false, false);
        let kind = self.constant(if subtract { 2 } else { 1 } | (shift as u32) << 8);
        self.b.def_var(self.lazy[3], kind);
    }

    /// XCHG r/m32, r32. Memory is always the first operand.
    fn exchange(&mut self, i: &Instruction) {
        let a = self.read(i, 0);
        let b = self.read(i, 1);
        let ptr = self.destination(i);
        self.write(i, ptr, b);
        self.write_register(i.op1_register(), a);
    }

    fn exchange_add(&mut self, i: &Instruction) {
        let a = self.read(i, 0);
        let b = self.read(i, 1);
        let ptr = self.destination(i);
        let sum = self.b.ins().iadd(a, b);
        self.arithmetic_flags(a, b, sum, operand_size(i, 0), false);
        // The destination wins when both operands name one register.
        self.write_register(i.op1_register(), a);
        self.write(i, ptr, sum);
    }

    /// CMPXCHG compares the accumulator with the destination. A memory
    /// destination is written either way, as the processor does.
    fn compare_exchange(&mut self, i: &Instruction) {
        let value = self.read(i, 0);
        let source = self.read(i, 1);
        let ptr = self.destination(i);
        let size = operand_size(i, 0);
        let accumulator = match size {
            1 => R::AL,
            2 => R::AX,
            _ => R::EAX,
        };
        let old = self.read_register(accumulator);
        let difference = self.b.ins().isub(old, value);
        self.arithmetic_flags(old, value, difference, size, true);
        let equal = self.b.ins().icmp(IntCC::Equal, old, value);
        let stored = self.b.ins().select(equal, source, value);
        let loaded = self.b.ins().select(equal, old, value);
        // Accumulator first: when it is also the destination, equality stores
        // the source into it.
        self.write_register(accumulator, loaded);
        self.write(i, ptr, stored);
    }

    /// BT/BTS/BTR/BTC on a doubleword: only CF changes.
    fn bit_test(&mut self, i: &Instruction) {
        let value = self.read(i, 0);
        let index = self.read(i, 1);
        let ptr = if i.mnemonic() == M::Bt {
            None
        } else {
            self.destination(i)
        };
        let index = self.b.ins().band_imm(index, 31);
        let carry = self.b.ins().ushr(value, index);
        let carry = self.b.ins().band_imm(carry, 1);
        let overflow = self.flag(11);
        self.set_carry_overflow(carry, overflow);
        let one = self.constant(1);
        let bit = self.b.ins().ishl(one, index);
        let result = match i.mnemonic() {
            M::Bt => return,
            M::Bts => self.b.ins().bor(value, bit),
            M::Btr => self.b.ins().band_not(value, bit),
            M::Btc => self.b.ins().bxor(value, bit),
            _ => unreachable!(),
        };
        self.write(i, ptr, result);
    }

    /// XLATB: AL = [EBX + AL].
    fn translate(&mut self, i: &Instruction) {
        let table = self.b.use_var(self.regs[3]);
        let index = self.read_register(R::AL);
        let mut address = self.b.ins().iadd(table, index);
        if i.segment_prefix() == R::FS {
            let fs = self.b.ins().load(
                types::I32,
                MemFlags::trusted(),
                self.state,
                offset_of!(CpuState, fs_base) as i32,
            );
            address = self.b.ins().iadd(address, fs);
        }
        let value = self.direct_load(address, types::I8);
        let value = self.b.ins().uextend(types::I32, value);
        self.write_register(R::AL, value);
    }
}
