//! Guarded x87 lowering to native scalar FP instructions, not calls per opcode.
//!
//! Canonical 80-bit registers are materialized at every observable boundary.
//! Bounded straight-line segments retain admitted binary64 values between ops.
//! Only exact binary64 inputs
//! are admitted; arithmetic additionally requires PC=53, RC=nearest, masked
//! exceptions and the same 2^+-400 bounds as floating::fast. Error-free residuals
//! reproduce PE/C1, including signed zero. Every rejection precedes guest writes
//! and joins the existing transactional helper without leaving the region.
//!
//! Ordinary single-page memory only: shared/file/audio, cross-page and executable
//! stores retain the helper's access ordering, rollback and invalidation. Mixed
//! MMX regions retain the helper too, since their aliases can be live SSA values.
use super::*;
use crate::X87State;
use crate::floating::fast::{Fast, Op, Source};
use cranelift_codegen::ir::condcodes::FloatCC;
mod segment;
pub(super) use segment::segments;

const FP: usize = offset_of!(CpuState, x87);
const C1: i64 = 0x200;
const PE: i64 = 0x20;
const RANGE: i32 = 400;

pub(super) fn enabled() -> bool {
    // ARM keeps its validated helper until native emission is tested on hardware.
    #[cfg(test)]
    if testing::HELPER_ONLY.get() {
        return false;
    }
    cfg!(target_arch = "x86_64")
        && std::env::var_os("ADV_JIT_X87_HELPER_ONLY").is_none_or(|v| v != "1")
}

fn use_fma() -> bool {
    #[cfg(test)]
    if testing::FORCE_DEKKER.get() {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    return std::arch::is_x86_feature_detected!("fma");
    #[cfg(not(target_arch = "x86_64"))]
    false
}

pub(super) fn helper_address() -> *const u8 {
    #[cfg(test)]
    return testing::counted_helper as *const u8;
    #[cfg(not(test))]
    return crate::floating::x87_call as *const u8;
}

impl Emitter<'_> {
    pub(super) fn canonical_x87_instruction(&mut self, i: &Instruction) {
        self.x87_cold_reads |= crate::floating::x87_effects(i).unwrap().read_gpr;
        let pc = self.constant(i.ip32());
        let next = self.constant(i.next_ip32());
        let operation = self
            .b
            .ins()
            .iconst(self.ptr_ty, self.x87[&i.ip32()].0 as i64);
        self.canonical_x87_recovery(pc, next, operation);
    }

    fn canonical_x87_recovery(&mut self, pc: Value, next: Value, operation: Value) {
        let block = if let Some(block) = self.x87_cold {
            block
        } else {
            let block = self.b.create_block();
            self.b.set_cold_block(block);
            self.b.append_block_param(block, types::I32);
            self.b.append_block_param(block, types::I32);
            self.b.append_block_param(block, self.ptr_ty);
            self.x87_cold = Some(block);
            block
        };
        self.b
            .ins()
            .jump(block, &[pc.into(), next.into(), operation.into()]);
    }

    /// Interior recovery entries share one synchronization/helper call.
    /// No resident state reaches this block; each caller commits its prefix.
    pub(super) fn emit_canonical_x87_recovery(&mut self) {
        let Some(block) = self.x87_cold else { return };
        self.b.switch_to_block(block);
        self.mark_source(super::diagnostics::x87_recovery_source());
        let pc = self.b.block_params(block)[0];
        let next = self.b.block_params(block)[1];
        let operation = self.b.block_params(block)[2];
        // This bridge only receives classified segment ops: no integer flags,
        // GPR/XMM writes or MMX SSA aliases. Keep their unrelated values live;
        // reloading them here would add phis to every native guest entry.
        for index in 0..8 {
            if self.x87_cold_reads & (1 << index) != 0 {
                let value = self.b.use_var(self.regs[index]);
                self.b
                    .ins()
                    .store(MemFlags::trusted(), value, self.state, (index * 4) as i32);
            }
        }
        self.b.ins().store(
            MemFlags::trusted(),
            pc,
            self.state,
            offset_of!(CpuState, eip) as i32,
        );
        let call = self
            .b
            .ins()
            .call(self.x87_ref, &[self.bridge, self.state, operation]);
        let result = self.b.inst_results(call)[0];
        let mut switch = Switch::new();
        let done = self.b.create_block();
        let retry = self.b.create_block();
        let read_fault = self.b.create_block();
        let write_fault = self.b.create_block();
        let after = self.b.create_block();
        for block in [done, retry, read_fault, write_fault, after] {
            self.b.set_cold_block(block);
        }
        switch.set_entry(u128::from(crate::floating::X87_DONE), done);
        switch.set_entry(u128::from(crate::floating::X87_FALLBACK), retry);
        switch.set_entry(u128::from(crate::floating::X87_READ_FAULT), read_fault);
        switch.set_entry(u128::from(crate::floating::X87_WRITE_FAULT), write_fault);
        switch.emit(&mut self.b, result, after);
        self.b.switch_to_block(retry);
        self.exit(Exit::Fallback, pc);
        self.b.switch_to_block(read_fault);
        self.exit(Exit::ReadFault, pc);
        self.b.switch_to_block(write_fault);
        self.exit(Exit::WriteFault, pc);
        self.b.switch_to_block(after);
        self.exit(Exit::Fallback, next);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        if self.shadow_ref.is_some() {
            let absent = self.b.ins().iconst(self.ptr_ty, 0);
            self.b
                .ins()
                .jump(self.dispatch, &[next.into(), absent.into()]);
        } else {
            self.b.ins().jump(self.dispatch, &[next.into()]);
        }
    }

    /// Emit an admitted operation plus a cold, in-region helper continuation.
    pub(super) fn native_x87_instruction(
        &mut self,
        i: &Instruction,
        operation: usize,
        opcode: Option<u16>,
    ) {
        let fast = crate::floating::fast::classify(i);
        if !self.x87_native
            || self.mmx_mode.is_some()
            || fast.is_none()
            // The general address emitter is 32-bit. The helper owns addr16.
            || i.memory_base().size() == 2
            || i.memory_index().size() == 2
        {
            self.x87_instruction(i, operation);
            return;
        }
        let fast = fast.unwrap();
        let slow = self.b.create_block();
        self.b.set_cold_block(slow);
        let done = self.b.create_block();
        let mode = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, mmx_mode) as i32,
        );
        let pending_mmx = self.b.ins().icmp_imm(IntCC::NotEqual, mode, 0);
        self.x87_reject(pending_mmx, slow);
        let mut status = self.x87_word(offset_of!(X87State, status));
        let pending = self.b.ins().band_imm(status, 0x80);
        let pending = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
        self.x87_reject(pending, slow);
        let control = self.x87_word(offset_of!(X87State, control));
        let mut tags = self.x87_word(offset_of!(X87State, tag));
        let top = self.b.ins().ushr_imm(status, 11);
        let mut top = self.b.ins().band_imm(top, 7);
        let mut writes = Vec::with_capacity(2);
        let mut freed = Vec::with_capacity(2);
        let mut dp = None;
        let mut metadata = true;
        status = self.b.ins().band_imm(status, !C1);

        match fast {
            Fast::Load(source) => {
                let (value, at) = self.x87_source(i, source, top, tags, 1023, slow);
                let target = self.x87_slot(top, 7);
                let tag = self.x87_tag(tags, target);
                let occupied = self.b.ins().icmp_imm(IntCC::NotEqual, tag, 3);
                self.x87_reject(occupied, slow);
                top = target;
                writes.push((target, value));
                dp = at;
            }
            Fast::Constant(one) => {
                let target = self.x87_slot(top, 7);
                let tag = self.x87_tag(tags, target);
                let occupied = self.b.ins().icmp_imm(IntCC::NotEqual, tag, 3);
                self.x87_reject(occupied, slow);
                top = target;
                let value = self.b.ins().f64const(if one { 1.0 } else { 0.0 });
                writes.push((target, value));
            }
            Fast::Store {
                width,
                integer,
                pop,
            } => {
                let (at, pointer) = self.x87_memory(i, width, true, slow);
                let value = self.x87_double(top, tags, 1023, slow);
                let (bits, rounded) = if integer {
                    let rc = self.b.ins().band_imm(control, 0xc00);
                    let nearest = self.b.ins().icmp_imm(IntCC::Equal, rc, 0);
                    let truncate = self.b.ins().icmp_imm(IntCC::Equal, rc, 0xc00);
                    let valid = self.b.ins().bor(nearest, truncate);
                    self.x87_require(valid, slow);
                    let rn = self.b.ins().nearest(value);
                    let rz = self.b.ins().trunc(value);
                    let rounded = self.b.ins().select(nearest, rn, rz);
                    let limit = if width == 4 {
                        2f64.powi(31)
                    } else {
                        2f64.powi(63)
                    };
                    let lower = self.b.ins().f64const(-limit);
                    let upper = self.b.ins().f64const(limit);
                    let low = self.b.ins().fcmp(FloatCC::LessThan, rounded, lower);
                    let high = self
                        .b
                        .ins()
                        .fcmp(FloatCC::GreaterThanOrEqual, rounded, upper);
                    let bad = self.b.ins().bor(low, high);
                    self.x87_reject(bad, slow);
                    let bits = self
                        .b
                        .ins()
                        .fcvt_to_sint_sat(integer_type(width as usize), rounded);
                    (bits, rounded)
                } else if width == 8 {
                    (
                        self.b.ins().bitcast(types::I64, MemFlags::new(), value),
                        value,
                    )
                } else {
                    let rc = self.b.ins().band_imm(control, 0xc00);
                    let bad = self.b.ins().icmp_imm(IntCC::NotEqual, rc, 0);
                    self.x87_reject(bad, slow);
                    let narrow = self.b.ins().fdemote(types::F32, value);
                    let bits = self.b.ins().bitcast(types::I32, MemFlags::new(), narrow);
                    let exponent = self.b.ins().ushr_imm(bits, 23);
                    let exponent = self.b.ins().band_imm(exponent, 255);
                    let exponent = self.b.ins().iadd_imm(exponent, -1);
                    let normal = self
                        .b
                        .ins()
                        .icmp_imm(IntCC::UnsignedLessThan, exponent, 254);
                    let zero = self.b.ins().f64const(0.0);
                    let was_zero = self.b.ins().fcmp(FloatCC::Equal, value, zero);
                    let valid = self.b.ins().bor(normal, was_zero);
                    self.x87_require(valid, slow);
                    (bits, self.b.ins().fpromote(types::F64, narrow))
                };
                let inexact = self.b.ins().fcmp(FloatCC::NotEqual, rounded, value);
                let unmasked = self.b.ins().band_imm(control, PE);
                let unmasked = self.b.ins().icmp_imm(IntCC::Equal, unmasked, 0);
                let bad = self.b.ins().band(inexact, unmasked);
                self.x87_reject(bad, slow);
                let a = self.b.ins().fabs(rounded);
                let b = self.b.ins().fabs(value);
                let up = self.b.ins().fcmp(FloatCC::GreaterThan, a, b);
                status = self.x87_round_flags(status, inexact, up);
                // All state/value/access guards dominate this first guest write.
                self.b.ins().store(MemFlags::new(), bits, pointer, 0);
                dp = Some(at);
                if pop {
                    freed.push(top);
                    top = self.x87_slot(top, 1);
                }
            }
            Fast::StoreSt { index, pop } => {
                let value = self.x87_double(top, tags, 1023, slow);
                let target = self.x87_slot(top, index);
                writes.push((target, value));
                if pop {
                    freed.push(top);
                    top = self.x87_slot(top, 1);
                }
            }
            Fast::Arith {
                op,
                dest,
                source,
                reverse,
                pop,
            } => {
                let masked = self.b.ins().band_imm(control, 0xf3f);
                let bad = self.b.ins().icmp_imm(IntCC::NotEqual, masked, 0x23f);
                self.x87_reject(bad, slow);
                let target = self.x87_slot(top, dest);
                let mut a = self.x87_double(target, tags, RANGE, slow);
                let (mut b, at) = self.x87_source(i, source, top, tags, RANGE, slow);
                if reverse {
                    std::mem::swap(&mut a, &mut b);
                }
                let zero = self.b.ins().f64const(0.0);
                if op == Op::Div {
                    let bad = self.b.ins().fcmp(FloatCC::Equal, b, zero);
                    self.x87_reject(bad, slow);
                }
                let (result, error) = self.x87_arithmetic(op, a, b);
                let inexact = self.b.ins().fcmp(FloatCC::NotEqual, error, zero);
                let err_bits = self.b.ins().bitcast(types::I64, MemFlags::new(), error);
                let result_bits = self.b.ins().bitcast(types::I64, MemFlags::new(), result);
                let signs = self.b.ins().bxor(err_bits, result_bits);
                let up = self.b.ins().icmp_imm(IntCC::SignedLessThan, signs, 0);
                status = self.x87_round_flags(status, inexact, up);
                writes.push((target, result));
                dp = at;
                if pop {
                    freed.push(top);
                    top = self.x87_slot(top, 1);
                }
            }
            Fast::Compare { source, pops } => {
                let a = self.x87_double(top, tags, 1023, slow);
                let (b, at) = self.x87_source(i, source, top, tags, 1023, slow);
                let equal = self.b.ins().fcmp(FloatCC::Equal, a, b);
                let less = self.b.ins().fcmp(FloatCC::LessThan, a, b);
                let equal = self.b.ins().uextend(types::I32, equal);
                let less = self.b.ins().uextend(types::I32, less);
                let equal = self.b.ins().ishl_imm(equal, 14);
                let less = self.b.ins().ishl_imm(less, 8);
                status = self.b.ins().band_imm(status, !0x4700);
                status = self.b.ins().bor(status, equal);
                status = self.b.ins().bor(status, less);
                dp = at;
                for n in 0..pops {
                    freed.push(self.x87_slot(top, n));
                }
                top = self.x87_slot(top, pops);
            }
            Fast::Exchange(index) => {
                let target = self.x87_slot(top, index);
                let a = self.x87_double(top, tags, 1023, slow);
                let b = self.x87_double(target, tags, 1023, slow);
                writes.extend([(top, b), (target, a)]);
            }
            Fast::Negate | Fast::Absolute => {
                let value = self.x87_double(top, tags, 1023, slow);
                let value = if fast == Fast::Negate {
                    self.b.ins().fneg(value)
                } else {
                    self.b.ins().fabs(value)
                };
                writes.push((top, value));
            }
            Fast::LoadControl => {
                let (_, pointer) = self.x87_memory(i, 2, false, slow);
                let value = self.b.ins().load(types::I16, MemFlags::new(), pointer, 0);
                let value = self.b.ins().uextend(types::I32, value);
                // Control/status transfers leave C1 and instruction/data metadata alone.
                status = self.x87_word(offset_of!(X87State, status));
                let unmasked = self.b.ins().bnot(value);
                let pending = self.b.ins().band(status, unmasked);
                let pending = self.b.ins().band_imm(pending, 0x3f);
                let bad = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
                self.x87_reject(bad, slow);
                self.x87_store_word(offset_of!(X87State, control), value);
                status = self.b.ins().band_imm(status, !0x8080);
                metadata = false;
            }
            Fast::StoreControl | Fast::StoreStatus => {
                status = self.x87_word(offset_of!(X87State, status));
                let value = if fast == Fast::StoreControl {
                    control
                } else {
                    status
                };
                if i.op0_kind() == OpKind::Register {
                    let eax = self.b.use_var(self.regs[0]);
                    let high = self.b.ins().band_imm(eax, 0xffff_0000);
                    let eax = self.b.ins().bor(high, value);
                    self.b.def_var(self.regs[0], eax);
                } else {
                    let (_, pointer) = self.x87_memory(i, 2, true, slow);
                    let value = self.b.ins().ireduce(types::I16, value);
                    self.b.ins().store(MemFlags::new(), value, pointer, 0);
                }
                metadata = false;
            }
        }
        for (slot, value) in writes {
            let zero = self.x87_write(slot, value);
            let tag = self.b.ins().uextend(types::I32, zero);
            tags = self.x87_set_tag(tags, slot, tag);
        }
        for slot in freed {
            let empty = self.constant(3);
            tags = self.x87_set_tag(tags, slot, empty);
        }
        self.x87_store_word(offset_of!(X87State, tag), tags);
        let top = self.b.ins().ishl_imm(top, 11);
        status = self.b.ins().band_imm(status, !0x3800);
        status = self.b.ins().bor(status, top);
        self.x87_store_word(offset_of!(X87State, status), status);
        if metadata {
            let pc = self.constant(i.ip32());
            self.b.ins().store(
                MemFlags::trusted(),
                pc,
                self.state,
                (FP + offset_of!(X87State, ip)) as i32,
            );
            if let Some(opcode) = opcode {
                let opcode = self.constant(u32::from(opcode));
                self.x87_store_word(offset_of!(X87State, opcode), opcode);
            }
        }
        if let Some(dp) = dp {
            self.b.ins().store(
                MemFlags::trusted(),
                dp,
                self.state,
                (FP + offset_of!(X87State, dp)) as i32,
            );
        }
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(slow);
        self.x87_instruction(i, operation);
        self.b.ins().jump(done, &[]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
    }

    fn x87_reject(&mut self, bad: Value, slow: ir::Block) {
        let next = self.b.create_block();
        self.b.ins().brif(bad, slow, &[], next, &[]);
        self.b.switch_to_block(next);
    }
    fn x87_require(&mut self, valid: Value, slow: ir::Block) {
        let bad = self.b.ins().icmp_imm(IntCC::Equal, valid, 0);
        self.x87_reject(bad, slow);
    }
    fn x87_word(&mut self, field: usize) -> Value {
        let value = self.b.ins().load(
            types::I16,
            MemFlags::trusted(),
            self.state,
            (FP + field) as i32,
        );
        self.b.ins().uextend(types::I32, value)
    }
    fn x87_store_word(&mut self, field: usize, value: Value) {
        let value = self.b.ins().ireduce(types::I16, value);
        self.b
            .ins()
            .store(MemFlags::trusted(), value, self.state, (FP + field) as i32);
    }
    fn x87_slot(&mut self, top: Value, index: u8) -> Value {
        let slot = self.b.ins().iadd_imm(top, i64::from(index));
        self.b.ins().band_imm(slot, 7)
    }
    fn x87_tag(&mut self, tags: Value, slot: Value) -> Value {
        let shift = self.b.ins().ishl_imm(slot, 1);
        let tag = self.b.ins().ushr(tags, shift);
        self.b.ins().band_imm(tag, 3)
    }
    fn x87_set_tag(&mut self, tags: Value, slot: Value, tag: Value) -> Value {
        let shift = self.b.ins().ishl_imm(slot, 1);
        let mask = self.constant(3);
        let mask = self.b.ins().ishl(mask, shift);
        let mask = self.b.ins().bnot(mask);
        let tags = self.b.ins().band(tags, mask);
        let tag = self.b.ins().ishl(tag, shift);
        self.b.ins().bor(tags, tag)
    }
    fn x87_register_pointer(&mut self, slot: Value) -> Value {
        let offset = self.b.ins().imul_imm(slot, 10);
        let offset = self.b.ins().uextend(self.ptr_ty, offset);
        let pointer = self.b.ins().iadd(self.state, offset);
        self.b
            .ins()
            .iadd_imm(pointer, (FP + offset_of!(X87State, regs)) as i64)
    }
    fn x87_double(&mut self, slot: Value, tags: Value, range: i32, slow: ir::Block) -> Value {
        let tag = self.x87_tag(tags, slot);
        let empty = self.b.ins().icmp_imm(IntCC::Equal, tag, 3);
        self.x87_reject(empty, slow);
        let pointer = self.x87_register_pointer(slot);
        let sig = self
            .b
            .ins()
            .load(types::I64, MemFlags::trusted(), pointer, 0);
        let se = self
            .b
            .ins()
            .load(types::I16, MemFlags::trusted(), pointer, 8);
        let se = self.b.ins().uextend(types::I64, se);
        let exponent = self.b.ins().band_imm(se, 0x7fff);
        let zero_exp = self.b.ins().icmp_imm(IntCC::Equal, exponent, 0);
        let zero_sig = self.b.ins().icmp_imm(IntCC::Equal, sig, 0);
        let zero = self.b.ins().band(zero_exp, zero_sig);
        // Never encode an extended-only exponent as a binary64 exponent.
        let lower = 0x3fff - range.min(1022);
        let upper = 0x3fff + range.min(1023);
        let bounded = self.b.ins().iadd_imm(exponent, -i64::from(lower));
        let bounded = self.b.ins().icmp_imm(
            IntCC::UnsignedLessThanOrEqual,
            bounded,
            i64::from(upper - lower),
        );
        let low = self.b.ins().band_imm(sig, 0x7ff);
        let exact = self.b.ins().icmp_imm(IntCC::Equal, low, 0);
        let normalized = self.b.ins().icmp_imm(IntCC::SignedLessThan, sig, 0);
        let valid = self.b.ins().band(bounded, exact);
        let valid = self.b.ins().band(valid, normalized);
        let valid = self.b.ins().bor(valid, zero);
        self.x87_require(valid, slow);
        let sign = self.b.ins().band_imm(se, 0x8000);
        let sign = self.b.ins().ishl_imm(sign, 48);
        let exponent = self.b.ins().iadd_imm(exponent, 1023 - 0x3fff);
        let exponent = self.b.ins().ishl_imm(exponent, 52);
        let fraction = self.b.ins().ushr_imm(sig, 11);
        let fraction = self.b.ins().band_imm(fraction, (1i64 << 52) - 1);
        let bits = self.b.ins().bor(sign, exponent);
        let bits = self.b.ins().bor(bits, fraction);
        let bits = self.b.ins().select(zero, sign, bits);
        self.b.ins().bitcast(types::F64, MemFlags::new(), bits)
    }
    fn x87_write(&mut self, slot: Value, value: Value) -> Value {
        let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), value);
        let abs = self.b.ins().ishl_imm(bits, 1);
        let zero = self.b.ins().icmp_imm(IntCC::Equal, abs, 0);
        let sig = self.b.ins().ishl_imm(bits, 11);
        let sig = self.b.ins().bor_imm(sig, i64::MIN);
        let nil = self.b.ins().iconst(types::I64, 0);
        let sig = self.b.ins().select(zero, nil, sig);
        let sign = self.b.ins().ushr_imm(bits, 48);
        let sign = self.b.ins().band_imm(sign, 0x8000);
        let exponent = self.b.ins().ushr_imm(bits, 52);
        let exponent = self.b.ins().band_imm(exponent, 0x7ff);
        let exponent = self.b.ins().iadd_imm(exponent, 0x3fff - 1023);
        let se = self.b.ins().bor(sign, exponent);
        let se = self.b.ins().select(zero, sign, se);
        let se = self.b.ins().ireduce(types::I16, se);
        let pointer = self.x87_register_pointer(slot);
        self.b.ins().store(MemFlags::trusted(), sig, pointer, 0);
        self.b.ins().store(MemFlags::trusted(), se, pointer, 8);
        // The executor refreshes all aliases on entry. No MMX SSA values exist
        // in admitted regions, so updating each changed physical alias suffices.
        let offset = self.b.ins().ishl_imm(slot, 3);
        let offset = self.b.ins().uextend(self.ptr_ty, offset);
        let alias = self.b.ins().iadd(self.state, offset);
        self.b.ins().store(
            MemFlags::trusted(),
            sig,
            alias,
            offset_of!(CpuState, mmx) as i32,
        );
        zero
    }
    fn x87_memory(
        &mut self,
        i: &Instruction,
        width: u8,
        write: bool,
        slow: ir::Block,
    ) -> (Value, Value) {
        let address = self.address(i);
        let offset = self.b.ins().iadd_imm(address, -i64::from(self.base));
        let bad = self.b.ins().icmp_imm(
            IntCC::UnsignedGreaterThan,
            offset,
            (self.len - usize::from(width)) as i64,
        );
        self.x87_reject(bad, slow);
        let in_page = self.b.ins().band_imm(offset, 4095);
        let crosses =
            self.b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, in_page, 4096 - i64::from(width));
        self.x87_reject(crosses, slow);
        let permission = self.permission(offset);
        let need = if write { WRITE } else { READ };
        let mask = need | crate::memory::SHARED | if write { EXEC } else { 0 };
        let permission = self.b.ins().band_imm(permission, i64::from(mask));
        let bad = self
            .b
            .ins()
            .icmp_imm(IntCC::NotEqual, permission, i64::from(need));
        self.x87_reject(bad, slow);
        let offset = self.b.ins().uextend(self.ptr_ty, offset);
        let pointer = self.b.ins().iadd(self.mem, offset);
        (address, pointer)
    }
    fn x87_source(
        &mut self,
        i: &Instruction,
        source: Source,
        top: Value,
        tags: Value,
        range: i32,
        slow: ir::Block,
    ) -> (Value, Option<Value>) {
        if let Source::St(index) = source {
            let slot = self.x87_slot(top, index);
            return (self.x87_double(slot, tags, range, slow), None);
        }
        let width = match source {
            Source::F64 => 8,
            Source::I16 => 2,
            _ => 4,
        };
        let (at, pointer) = self.x87_memory(i, width, false, slow);
        let bits = self
            .b
            .ins()
            .load(integer_type(width as usize), MemFlags::new(), pointer, 0);
        let value = match source {
            Source::I16 => {
                let bits = self.b.ins().sextend(types::I32, bits);
                self.b.ins().fcvt_from_sint(types::F64, bits)
            }
            Source::I32 => self.b.ins().fcvt_from_sint(types::F64, bits),
            Source::F32 | Source::F64 => {
                let (shift, mask) = if width == 4 { (23, 255) } else { (52, 2047) };
                let magnitude = self.b.ins().ishl_imm(bits, 1);
                let zero = self.b.ins().icmp_imm(IntCC::Equal, magnitude, 0);
                let exponent = self.b.ins().ushr_imm(bits, shift);
                let exponent = self.b.ins().band_imm(exponent, mask);
                let exponent = self.b.ins().iadd_imm(exponent, -1);
                let normal = self
                    .b
                    .ins()
                    .icmp_imm(IntCC::UnsignedLessThan, exponent, mask - 1);
                let valid = self.b.ins().bor(zero, normal);
                self.x87_require(valid, slow);
                if width == 4 {
                    let value = self.b.ins().bitcast(types::F32, MemFlags::new(), bits);
                    self.b.ins().fpromote(types::F64, value)
                } else {
                    self.b.ins().bitcast(types::F64, MemFlags::new(), bits)
                }
            }
            Source::St(_) => unreachable!(),
        };
        // Normal f32 and signed 16/32-bit integers already fit 2^+-400.
        if range == RANGE && source == Source::F64 {
            let bits = self.b.ins().bitcast(types::I64, MemFlags::new(), value);
            let magnitude = self.b.ins().ishl_imm(bits, 1);
            let zero = self.b.ins().icmp_imm(IntCC::Equal, magnitude, 0);
            let exponent = self.b.ins().ushr_imm(bits, 52);
            let exponent = self.b.ins().band_imm(exponent, 0x7ff);
            let relative = self.b.ins().iadd_imm(exponent, -i64::from(1023 - RANGE));
            let bounded = self.b.ins().icmp_imm(
                IntCC::UnsignedLessThanOrEqual,
                relative,
                i64::from(2 * RANGE),
            );
            let valid = self.b.ins().bor(zero, bounded);
            self.x87_require(valid, slow);
        }
        (value, Some(at))
    }
    fn x87_round_flags(&mut self, status: Value, inexact: Value, up: Value) -> Value {
        let up = self.b.ins().uextend(types::I32, up);
        let up = self.b.ins().ishl_imm(up, 9);
        let flags = self.b.ins().bor_imm(up, PE);
        let zero = self.constant(0);
        let flags = self.b.ins().select(inexact, flags, zero);
        self.b.ins().bor(status, flags)
    }
    fn x87_product_error(&mut self, a: Value, b: Value, product: Value) -> Value {
        if use_fma() {
            let negative = self.b.ins().fneg(product);
            return self.b.ins().fma(a, b, negative);
        }
        // Exact Dekker residual on hosts without FMA. No library call is emitted.
        let split = self.b.ins().f64const(134_217_729.0);
        let mut halves = |value| {
            let c = self.b.ins().fmul(split, value);
            let d = self.b.ins().fsub(c, value);
            let high = self.b.ins().fsub(c, d);
            let low = self.b.ins().fsub(value, high);
            (high, low)
        };
        let (ah, al) = halves(a);
        let (bh, bl) = halves(b);
        let hh = self.b.ins().fmul(ah, bh);
        let error = self.b.ins().fsub(hh, product);
        let hl = self.b.ins().fmul(ah, bl);
        let error = self.b.ins().fadd(error, hl);
        let lh = self.b.ins().fmul(al, bh);
        let error = self.b.ins().fadd(error, lh);
        let ll = self.b.ins().fmul(al, bl);
        self.b.ins().fadd(error, ll)
    }
    fn x87_arithmetic(&mut self, op: Op, a: Value, b: Value) -> (Value, Value) {
        match op {
            Op::Add | Op::Sub => {
                let b = if op == Op::Sub {
                    self.b.ins().fneg(b)
                } else {
                    b
                };
                let sum = self.b.ins().fadd(a, b);
                let bb = self.b.ins().fsub(sum, a);
                let aa = self.b.ins().fsub(sum, bb);
                let ar = self.b.ins().fsub(a, aa);
                let br = self.b.ins().fsub(b, bb);
                (sum, self.b.ins().fadd(ar, br))
            }
            Op::Mul => {
                let p = self.b.ins().fmul(a, b);
                (p, self.x87_product_error(a, b, p))
            }
            Op::Div => {
                let q = self.b.ins().fdiv(a, b);
                let p = self.b.ins().fmul(q, b);
                let error = self.x87_product_error(q, b, p);
                let difference = self.b.ins().fsub(a, p);
                let residual = self.b.ins().fsub(difference, error);
                // Only the quotient residual's sign matters. Avoid a second
                // division: nonzero r/b has sign(r) XOR sign(b) in this domain.
                let magnitude = self.b.ins().fabs(residual);
                let b_negative = self.b.ins().bitcast(types::I64, MemFlags::new(), b);
                let r_bits = self.b.ins().bitcast(types::I64, MemFlags::new(), residual);
                let sign = self.b.ins().bxor(b_negative, r_bits);
                let sign = self.b.ins().bitcast(types::F64, MemFlags::new(), sign);
                (q, self.b.ins().fcopysign(magnitude, sign))
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::cell::Cell;
    thread_local! {
        pub(crate) static FORCE_DEKKER: Cell<bool> = const { Cell::new(false) };
        pub(crate) static HELPER_ONLY: Cell<bool> = const { Cell::new(false) };
        pub(crate) static SINGLE_ONLY: Cell<bool> = const { Cell::new(false) };
        static CALLS: Cell<u64> = const { Cell::new(0) };
    }
    pub(crate) fn calls() -> u64 {
        CALLS.get()
    }
    pub(super) unsafe extern "C" fn counted_helper(
        bridge: *mut std::ffi::c_void,
        state: *mut CpuState,
        op: *const crate::floating::NativeX87,
    ) -> u32 {
        CALLS.set(CALLS.get() + 1);
        // SAFETY: exact same generated-code ABI and owner lifetime as x87_call.
        unsafe { crate::floating::x87_call(bridge, state, op) }
    }
    pub(crate) fn run(compiled: &Compiled, state: &mut CpuState, memory: &mut Memory) -> u32 {
        let mut bridge = crate::trap::Bridge {
            timestamp: Default::default(),
            memory,
            error: None,
            trap: None,
            calls: 0,
            handler: None,
            parked: false,
            deadline: None,
            bulk_calls: 0,
            bulk_bytes: 0,
            memo: std::ptr::null_mut(),
            recorder: std::ptr::null_mut(),
        };
        let interrupt = std::sync::atomic::AtomicBool::new(false);
        compiled
            .run(
                state,
                memory,
                &mut bridge,
                NativeRun {
                    links: None,
                    stop: 0,
                    interrupt: &interrupt,
                    shadow: None,
                },
            )
            .0
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests;
