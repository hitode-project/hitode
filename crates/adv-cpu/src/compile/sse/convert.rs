//! Integer conversions compute guest status explicitly, independent of host FP flags.
use super::*;

impl Emitter<'_> {
    /// A host wide-load fault reports the operand start. Let the exact memory
    /// path locate the first inaccessible guest byte when a load crosses pages.
    pub(in crate::compile) fn sse_conversion_memory(&mut self, i: &Instruction) {
        if i.op1_kind() == OpKind::Memory {
            let address = self.address(i);
            let offset = self.b.ins().band_imm(address, 4095);
            let crosses = self.b.ins().icmp_imm(
                IntCC::UnsignedGreaterThan,
                offset,
                4096 - i.memory_size().size() as i64,
            );
            self.guard(crosses, Exit::Fallback, None);
        }
    }

    /// Masked conversions can commit their result and sticky flags together.
    /// DAZ and unmasked exceptions retain the transactional exact path.
    pub(super) fn sse_conversion_control(&mut self) -> Value {
        let mxcsr = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, mxcsr) as i32,
        );
        let control = self.b.ins().band_imm(mxcsr, 0x1fc0);
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, control, 0x1f80);
        self.guard(bad, Exit::Fallback, None);
        mxcsr
    }

    fn conversion_flag(&mut self, mask: Value, bit: i64) -> Value {
        let any = if self.b.func.dfg.value_type(mask).is_vector() {
            self.b.ins().vany_true(mask)
        } else {
            mask
        };
        let yes = self.b.ins().iconst(types::I32, bit);
        let no = self.constant(0);
        self.b.ins().select(any, yes, no)
    }

    pub(super) fn sse_conversion_status(&mut self, mxcsr: Value, flags: Value) {
        let value = self.b.ins().bor(mxcsr, flags);
        self.b.ins().store(
            MemFlags::trusted(),
            value,
            self.state,
            offset_of!(CpuState, mxcsr) as i32,
        );
    }

    /// Return the int32 result and IE/DE/PE without committing either. All
    /// lanes must finish before the caller changes architectural state.
    pub(super) fn sse_to_int(
        &mut self,
        value: Value,
        truncate: bool,
        mxcsr: Value,
    ) -> (Value, Value) {
        let ty = self.b.func.dfg.value_type(value);
        let rounded = if truncate {
            self.b.ins().trunc(value)
        } else {
            let rc = self.b.ins().band_imm(mxcsr, 0x6000);
            let nearest = self.b.ins().nearest(value);
            let down = self.b.ins().floor(value);
            let up = self.b.ins().ceil(value);
            let zero = self.b.ins().trunc(value);
            let is_down = self.b.ins().icmp_imm(IntCC::Equal, rc, 0x2000);
            let is_up = self.b.ins().icmp_imm(IntCC::Equal, rc, 0x4000);
            let is_zero = self.b.ins().icmp_imm(IntCC::Equal, rc, 0x6000);
            let result = self.b.ins().select(is_down, down, nearest);
            let result = self.b.ins().select(is_up, up, result);
            self.b.ins().select(is_zero, zero, result)
        };
        let wide = ty.lane_type() == types::F64;
        let low = self.sse_splat(
            ty,
            if wide {
                (-2_147_483_648f64).to_bits()
            } else {
                u64::from((-2_147_483_648f32).to_bits())
            },
        );
        let high = self.sse_splat(
            ty,
            if wide {
                2_147_483_648f64.to_bits()
            } else {
                u64::from(2_147_483_648f32.to_bits())
            },
        );
        let above = self.b.ins().fcmp(FloatCC::GreaterThanOrEqual, rounded, low);
        let below = self.b.ins().fcmp(FloatCC::LessThan, rounded, high);
        let fits = self.b.ins().band(above, below);
        // Scalar comparison results are i8 booleans, not all-ones masks.
        let invalid = if ty.is_vector() {
            self.b.ins().bnot(fits)
        } else {
            self.b.ins().icmp_imm(IntCC::Equal, fits, 0)
        };
        let changed = self.b.ins().fcmp(FloatCC::NotEqual, value, rounded);
        let inexact = self.b.ins().band(fits, changed);
        let ie = self.conversion_flag(invalid, 1);
        let pe = self.conversion_flag(inexact, 0x20);
        // Match the interpreter's input-denormal status, even when rounding
        // produces an exact zero. DAZ was excluded before loading the operand.
        let bits = self.sse_cast(value, integer(ty));
        let magnitude = self.sse_splat(
            integer(ty),
            if wide {
                0x7fff_ffff_ffff_ffff
            } else {
                0x7fff_ffff
            },
        );
        let magnitude = self.b.ins().band(bits, magnitude);
        let minimum = self.sse_splat(
            integer(ty),
            if wide {
                0x0010_0000_0000_0000
            } else {
                0x0080_0000
            },
        );
        let zero = self.sse_splat(integer(ty), 0);
        let tiny = self
            .b
            .ins()
            .icmp(IntCC::UnsignedLessThan, magnitude, minimum);
        let nonzero = self.b.ins().icmp(IntCC::NotEqual, magnitude, zero);
        let denormal = self.b.ins().band(tiny, nonzero);
        let de = self.conversion_flag(denormal, 2);
        let flags = self.b.ins().bor(ie, pe);
        let flags = self.b.ins().bor(flags, de);
        let target = if ty.is_vector() {
            types::I32X4
        } else {
            types::I32
        };
        let converted = self.b.ins().fcvt_to_sint_sat(target, rounded);
        let indefinite = self.sse_splat(target, 0x8000_0000);
        // ARM64 FCVT saturates (and maps NaN to zero). Explicitly select x86's
        // indefinite result, including for the positive endpoint and NaNs.
        let result = if ty.is_vector() {
            let fits = self.sse_cast(fits, target);
            self.b.ins().bitselect(fits, converted, indefinite)
        } else {
            self.b.ins().select(fits, converted, indefinite)
        };
        (result, flags)
    }

    /// Exact int32 -> float conversions are independent of RC and never raise
    /// status. Reject inexact lanes before writing anything, so the exact
    /// interpreter handles guest rounding and exception delivery.
    pub(super) fn sse_from_int_exact(&mut self, value: Value, target: ir::Type) -> Value {
        let result = self.b.ins().fcvt_from_sint(target, value);
        if target == types::F64 {
            // Every int32 is exactly representable in binary64.
            return result;
        }
        let count = if target.is_vector() { 4 } else { 1 };
        let mut bad = self.b.ins().iconst(types::I8, 0);
        for lane in 0..count {
            let (source, converted) = if target.is_vector() {
                (
                    self.b.ins().extractlane(value, lane),
                    self.b.ins().extractlane(result, lane),
                )
            } else {
                (value, result)
            };
            // int64 keeps the rounded-up i32::MAX distinct from its input.
            let back = self.b.ins().fcvt_to_sint_sat(types::I64, converted);
            let source = self.b.ins().sextend(types::I64, source);
            let changed = self.b.ins().icmp(IntCC::NotEqual, source, back);
            bad = self.b.ins().bor(bad, changed);
        }
        self.guard(bad, Exit::Fallback, None);
        result
    }

    pub(in crate::compile) fn sse_mmx_conversion(&mut self, i: &Instruction) {
        // Legacy MMX conversions deliver pending x87 exceptions before even
        // reading memory. The common MMX writeback handles TOP/tag/payloads.
        if i.mnemonic() != M::Cvtpi2ps || i.op1_kind() == OpKind::Register {
            let status = self.b.ins().load(
                types::I16,
                MemFlags::trusted(),
                self.state,
                (offset_of!(CpuState, x87) + offset_of!(crate::X87State, status)) as i32,
            );
            let pending = self.b.ins().band_imm(status, 0x80);
            let bad = self.b.ins().icmp_imm(IntCC::NotEqual, pending, 0);
            self.guard(bad, Exit::Fallback, None);
        }
        self.sse_conversion_memory(i);
        if i.mnemonic() == M::Cvtpi2ps {
            let source = self.mmx_read(i, 1);
            // Only the low two lanes belong to the MMX source.
            let source = self.sse_cast(source, types::I64X2);
            let low = self.b.ins().extractlane(source, 0);
            let source = self.b.ins().scalar_to_vector(types::I64X2, low);
            let source = self.sse_cast(source, types::I32X4);
            let result = self.sse_from_int_exact(source, types::F32X4);
            let old = self.sse_register(i.op0_register(), types::I8X16);
            let result = self.sse_shuffle(result, old, &[0, 3], 8);
            self.sse_define(i.op0_register(), result);
        } else {
            let mxcsr = self.sse_conversion_control();
            let low = self.sse_scalar(i, 1, types::I64);
            let source = self.b.ins().scalar_to_vector(types::I64X2, low);
            let source = self.sse_cast(source, types::F32X4);
            let (result, flags) = self.sse_to_int(source, i.mnemonic() == M::Cvttps2pi, mxcsr);
            self.sse_conversion_status(mxcsr, flags);
            self.set_mmx(i, result);
        }
    }
}
