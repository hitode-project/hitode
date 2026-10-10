//! SSE/SSE2 floating point, moves and shuffles. Arithmetic, comparisons and
//! floating-format conversions run natively only under the default MXCSR control (all
//! exceptions masked, round to nearest, no DAZ/FTZ); any other control falls
//! back before the instruction changes state. Results, NaN selection and
//! out-of-range conversions follow x86 exactly on every host. Native code does
//! not accumulate arithmetic status flags. Integer conversions instead compute
//! full guest status or guard exact results; see `convert`.
use super::*;
use cranelift_codegen::ir::condcodes::FloatCC;

mod convert;

fn xmm(i: &Instruction, n: u32) -> bool {
    i.op_kind(n) == OpKind::Register && simd::register(i.op_register(n)).is_some()
}

fn gpr(i: &Instruction, n: u32) -> bool {
    i.op_kind(n) == OpKind::Register
        && i.op_register(n).size() == 4
        && reg_index(i.op_register(n)).is_some()
}

fn memory(i: &Instruction, n: u32, size: usize) -> bool {
    i.op_kind(n) == OpKind::Memory
        && i.memory_size().size() == size
        && [i.memory_base(), i.memory_index()]
            .iter()
            .all(|&r| r == R::None || reg_index(r).is_some())
}

fn source(i: &Instruction, n: u32, size: usize) -> bool {
    xmm(i, n) || memory(i, n, size)
}

fn double(m: M) -> bool {
    matches!(
        m,
        M::Addsd
            | M::Subsd
            | M::Mulsd
            | M::Divsd
            | M::Sqrtsd
            | M::Minsd
            | M::Maxsd
            | M::Addpd
            | M::Subpd
            | M::Mulpd
            | M::Divpd
            | M::Sqrtpd
            | M::Minpd
            | M::Maxpd
            | M::Cmpsd
            | M::Cmppd
            | M::Comisd
            | M::Ucomisd
    )
}

fn packed(m: M) -> bool {
    matches!(
        m,
        M::Addps
            | M::Addpd
            | M::Subps
            | M::Subpd
            | M::Mulps
            | M::Mulpd
            | M::Divps
            | M::Divpd
            | M::Sqrtps
            | M::Sqrtpd
            | M::Minps
            | M::Minpd
            | M::Maxps
            | M::Maxpd
            | M::Cmpps
            | M::Cmppd
    )
}

pub(super) fn supported(i: &Instruction) -> bool {
    let count = i.op_count();
    let immediate = |n: u32| i.op_kind(n) == OpKind::Immediate8;
    match i.mnemonic() {
        M::Stmxcsr => count == 1 && memory(i, 0, 4),
        M::Prefetchnta | M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 => count == 1,
        M::Sfence | M::Lfence | M::Mfence => count == 0,
        M::Movnti => count == 2 && memory(i, 0, 4) && gpr(i, 1),
        M::Movaps | M::Movups | M::Movapd | M::Movupd => {
            count == 2 && (xmm(i, 0) && source(i, 1, 16) || memory(i, 0, 16) && xmm(i, 1))
        }
        M::Movntps | M::Movntpd | M::Movntdq => count == 2 && memory(i, 0, 16) && xmm(i, 1),
        M::Movss => count == 2 && (xmm(i, 0) && source(i, 1, 4) || memory(i, 0, 4) && xmm(i, 1)),
        M::Movsd | M::Movq => {
            count == 2 && (xmm(i, 0) && source(i, 1, 8) || memory(i, 0, 8) && xmm(i, 1))
        }
        M::Movd => {
            count == 2
                && (xmm(i, 0) && (gpr(i, 1) || memory(i, 1, 4))
                    || (gpr(i, 0) || memory(i, 0, 4)) && xmm(i, 1))
        }
        M::Movlps | M::Movhps | M::Movlpd | M::Movhpd => {
            count == 2 && (xmm(i, 0) && memory(i, 1, 8) || memory(i, 0, 8) && xmm(i, 1))
        }
        M::Movhlps | M::Movlhps => count == 2 && xmm(i, 0) && xmm(i, 1),
        M::Movmskps | M::Movmskpd => count == 2 && gpr(i, 0) && xmm(i, 1),
        M::Andps
        | M::Andpd
        | M::Andnps
        | M::Andnpd
        | M::Orps
        | M::Orpd
        | M::Xorps
        | M::Xorpd
        | M::Unpcklps
        | M::Unpckhps
        | M::Unpcklpd
        | M::Unpckhpd
        | M::Cvtdq2ps
        | M::Cvtps2dq
        | M::Cvttps2dq
        | M::Cvtpd2ps
        | M::Cvtpd2dq
        | M::Cvttpd2dq => count == 2 && xmm(i, 0) && source(i, 1, 16),
        M::Cvtps2pd | M::Cvtdq2pd => count == 2 && xmm(i, 0) && source(i, 1, 8),
        M::Shufps | M::Shufpd | M::Pshufd => {
            count == 3 && xmm(i, 0) && source(i, 1, 16) && immediate(2)
        }
        M::Cmpss | M::Cmpsd | M::Cmpps | M::Cmppd => {
            let size = if packed(i.mnemonic()) {
                16
            } else if double(i.mnemonic()) {
                8
            } else {
                4
            };
            count == 3 && xmm(i, 0) && source(i, 1, size) && immediate(2)
        }
        M::Addss
        | M::Subss
        | M::Mulss
        | M::Divss
        | M::Sqrtss
        | M::Minss
        | M::Maxss
        | M::Comiss
        | M::Ucomiss
        | M::Cvtss2sd => count == 2 && xmm(i, 0) && source(i, 1, 4),
        M::Addsd
        | M::Subsd
        | M::Mulsd
        | M::Divsd
        | M::Sqrtsd
        | M::Minsd
        | M::Maxsd
        | M::Comisd
        | M::Ucomisd
        | M::Cvtsd2ss => count == 2 && xmm(i, 0) && source(i, 1, 8),
        M::Addps
        | M::Addpd
        | M::Subps
        | M::Subpd
        | M::Mulps
        | M::Mulpd
        | M::Divps
        | M::Divpd
        | M::Sqrtps
        | M::Sqrtpd
        | M::Minps
        | M::Minpd
        | M::Maxps
        | M::Maxpd => count == 2 && xmm(i, 0) && source(i, 1, 16),
        M::Cvtsi2ss | M::Cvtsi2sd => count == 2 && xmm(i, 0) && (gpr(i, 1) || memory(i, 1, 4)),
        M::Cvtss2si | M::Cvttss2si => count == 2 && gpr(i, 0) && source(i, 1, 4),
        M::Cvtsd2si | M::Cvttsd2si => count == 2 && gpr(i, 0) && source(i, 1, 8),
        _ => false,
    }
}

/// Whether the host's NaN results can differ from x86 beyond operand order:
/// AArch64 returns a positive default NaN and prefers a signaling operand.
fn foreign_nans() -> bool {
    #[cfg(test)]
    if testing::FOREIGN_NANS.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    !cfg!(target_arch = "x86_64")
}

#[cfg(test)]
pub(super) mod testing {
    /// Emit the non-x86 NaN fixups on an x86 host as well, so tests can check
    /// them against the interpreter.
    pub(in crate::compile) static FOREIGN_NANS: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
}

fn integer(ty: ir::Type) -> ir::Type {
    ty.as_int()
}

impl Emitter<'_> {
    fn sse_cast(&mut self, value: Value, ty: ir::Type) -> Value {
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

    fn sse_register(&mut self, reg: R, ty: ir::Type) -> Value {
        let value = self
            .b
            .use_var(self.xmm[simd::register(reg).unwrap()].unwrap());
        self.sse_cast(value, ty)
    }

    fn sse_define(&mut self, reg: R, value: Value) {
        let value = self.sse_cast(value, types::I8X16);
        self.b
            .def_var(self.xmm[simd::register(reg).unwrap()].unwrap(), value);
    }

    /// The memory operand's address. Legacy 16-byte operands other than
    /// MOVUPS/MOVUPD must be aligned; the interpreter raises #GP otherwise.
    fn sse_address(&mut self, i: &Instruction) -> Value {
        let addr = self.address(i);
        if i.memory_size().size() == 16 && !matches!(i.mnemonic(), M::Movups | M::Movupd) {
            let misaligned = self.b.ins().band_imm(addr, 15);
            let bad = self.b.ins().icmp_imm(IntCC::NotEqual, misaligned, 0);
            self.guard(bad, Exit::Fallback, None);
        }
        addr
    }

    /// Operand `n` as a whole register of type `ty`, or a 16-byte memory load.
    fn sse_vector(&mut self, i: &Instruction, n: u32, ty: ir::Type) -> Value {
        if i.op_kind(n) == OpKind::Memory {
            let addr = self.sse_address(i);
            let value = self.direct_load(addr, types::I8X16);
            self.sse_cast(value, ty)
        } else {
            self.sse_register(i.op_register(n), ty)
        }
    }

    /// Operand `n`'s low lane of scalar type `ty`.
    fn sse_scalar(&mut self, i: &Instruction, n: u32, ty: ir::Type) -> Value {
        if i.op_kind(n) == OpKind::Memory {
            let addr = self.sse_address(i);
            let bits = self.direct_load(addr, integer(ty));
            if ty.is_float() {
                self.b.ins().bitcast(ty, MemFlags::new(), bits)
            } else {
                bits
            }
        } else {
            let lanes = ty.by(16 / ty.bytes()).unwrap();
            let vector = self.sse_register(i.op_register(n), lanes);
            self.b.ins().extractlane(vector, 0)
        }
    }

    /// Store `value`'s low `size` bytes to the memory operand.
    fn sse_store(&mut self, i: &Instruction, value: Value, size: u32) {
        let addr = self.sse_address(i);
        let value = match size {
            16 => self.sse_cast(value, types::I8X16),
            _ => {
                let ty = if size == 4 { types::I32 } else { types::I64 };
                let lanes = self.sse_cast(value, ty.by(16 / size).unwrap());
                self.b.ins().extractlane(lanes, 0)
            }
        };
        self.direct_store(addr, value);
    }

    /// Falls back unless MXCSR has the default control: all exceptions
    /// masked, round to nearest, no DAZ or FTZ.
    fn sse_default_control(&mut self) {
        let mxcsr = self.b.ins().load(
            types::I32,
            MemFlags::trusted(),
            self.state,
            offset_of!(CpuState, mxcsr) as i32,
        );
        let control = self.b.ins().band_imm(mxcsr, 0xffc0);
        let bad = self.b.ins().icmp_imm(IntCC::NotEqual, control, 0x1f80);
        self.guard(bad, Exit::Fallback, None);
    }

    fn sse_splat(&mut self, ty: ir::Type, bits: u64) -> Value {
        let lane = integer(ty.lane_type());
        let value = self.b.ins().iconst(lane, bits as i64);
        let value = if ty.is_vector() {
            self.b.ins().splat(integer(ty), value)
        } else {
            value
        };
        self.sse_cast(value, ty)
    }

    fn sse_select(&mut self, condition: Value, yes: Value, no: Value) -> Value {
        let ty = self.b.func.dfg.value_type(yes);
        if ty.is_vector() {
            let mask = self.sse_cast(condition, ty);
            self.b.ins().bitselect(mask, yes, no)
        } else {
            self.b.ins().select(condition, yes, no)
        }
    }

    fn sse_quiet(&mut self, value: Value) -> Value {
        let ty = self.b.func.dfg.value_type(value);
        let bit = if ty.lane_type() == types::F32 {
            1 << 22
        } else {
            1 << 51
        };
        let bit = self.sse_splat(integer(ty), bit);
        let bits = self.sse_cast(value, integer(ty));
        let bits = self.b.ins().bor(bits, bit);
        self.sse_cast(bits, ty)
    }

    /// `result` with x86's NaN: the first NaN operand quieted, else the
    /// negative default NaN when the operation itself is invalid.
    fn sse_nan(&mut self, a: Option<Value>, b: Value, result: Value, commutes: bool) -> Value {
        let foreign = foreign_nans();
        let mut result = result;
        if foreign {
            let ty = self.b.func.dfg.value_type(result);
            let default = if ty.lane_type() == types::F32 {
                0xffc0_0000
            } else {
                0xfff8_0000_0000_0000
            };
            let default = self.sse_splat(ty, default);
            let invalid = self.b.ins().fcmp(FloatCC::Unordered, result, result);
            result = self.sse_select(invalid, default, result);
            let nan = self.b.ins().fcmp(FloatCC::Unordered, b, b);
            let quiet = self.sse_quiet(b);
            result = self.sse_select(nan, quiet, result);
        }
        // Cranelift may swap commutative operands, which changes which of two
        // NaNs x86 hardware returns.
        if let Some(a) = a
            && (foreign || commutes)
        {
            let nan = self.b.ins().fcmp(FloatCC::Unordered, a, a);
            let quiet = self.sse_quiet(a);
            result = self.sse_select(nan, quiet, result);
        }
        result
    }

    fn sse_arithmetic(&mut self, m: M, a: Value, b: Value) -> Value {
        match m {
            M::Addss | M::Addsd | M::Addps | M::Addpd => {
                let r = self.b.ins().fadd(a, b);
                self.sse_nan(Some(a), b, r, true)
            }
            M::Subss | M::Subsd | M::Subps | M::Subpd => {
                let r = self.b.ins().fsub(a, b);
                self.sse_nan(Some(a), b, r, false)
            }
            M::Mulss | M::Mulsd | M::Mulps | M::Mulpd => {
                let r = self.b.ins().fmul(a, b);
                self.sse_nan(Some(a), b, r, true)
            }
            M::Divss | M::Divsd | M::Divps | M::Divpd => {
                let r = self.b.ins().fdiv(a, b);
                self.sse_nan(Some(a), b, r, false)
            }
            M::Sqrtss | M::Sqrtsd | M::Sqrtps | M::Sqrtpd => {
                let r = self.b.ins().sqrt(b);
                self.sse_nan(None, b, r, false)
            }
            // MIN/MAX return the second operand unless the first is strictly
            // smaller/larger, which covers NaNs and signed zeros.
            M::Minss | M::Minsd | M::Minps | M::Minpd => {
                let less = self.b.ins().fcmp(FloatCC::LessThan, a, b);
                self.sse_select(less, a, b)
            }
            M::Maxss | M::Maxsd | M::Maxps | M::Maxpd => {
                let greater = self.b.ins().fcmp(FloatCC::GreaterThan, a, b);
                self.sse_select(greater, a, b)
            }
            _ => unreachable!(),
        }
    }

    /// CMPccSS/SD/PS/PD: all ones where the predicate holds. NLT and NLE are
    /// the negated orders; AArch64 lowers no unordered-or-order condition.
    fn sse_compare(&mut self, predicate: u8, a: Value, b: Value) -> Value {
        let (cc, negate) = match predicate & 7 {
            0 => (FloatCC::Equal, false),
            1 => (FloatCC::LessThan, false),
            2 => (FloatCC::LessThanOrEqual, false),
            3 => (FloatCC::Unordered, false),
            4 => (FloatCC::NotEqual, false),
            5 => (FloatCC::LessThan, true),
            6 => (FloatCC::LessThanOrEqual, true),
            _ => (FloatCC::Ordered, false),
        };
        let ty = self.b.func.dfg.value_type(a);
        let result = self.b.ins().fcmp(cc, a, b);
        if ty.is_vector() {
            if negate {
                self.b.ins().bnot(result)
            } else {
                result
            }
        } else {
            let ones = self.b.ins().iconst(integer(ty), -1);
            let zero = self.b.ins().iconst(integer(ty), 0);
            let (yes, no) = if negate { (zero, ones) } else { (ones, zero) };
            self.b.ins().select(result, yes, no)
        }
    }

    fn sse_shuffle(&mut self, a: Value, b: Value, lanes: &[u8], width: u8) -> Value {
        let mask: Vec<u8> = lanes
            .iter()
            .flat_map(|&lane| (0..width).map(move |byte| lane * width + byte))
            .collect();
        let mask = self
            .b
            .func
            .dfg
            .immediates
            .push(ir::ConstantData::from(mask));
        let a = self.sse_cast(a, types::I8X16);
        let b = self.sse_cast(b, types::I8X16);
        self.b.ins().shuffle(a, b, mask)
    }

    pub(super) fn sse_instruction(&mut self, i: &Instruction) {
        let m = i.mnemonic();
        if m == M::Stmxcsr {
            let address = self.address(i);
            let offset = self.b.ins().band_imm(address, 4095);
            let crosses = self
                .b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, offset, 4092);
            // The exact SIMD store reports the first inaccessible byte. A wide
            // native fault reports its start, so retain that contract at pages.
            self.guard(crosses, Exit::Fallback, None);
            let value = self.b.ins().load(
                types::I32,
                MemFlags::trusted(),
                self.state,
                offset_of!(CpuState, mxcsr) as i32,
            );
            self.direct_store(address, value);
            return;
        }
        if matches!(
            m,
            M::Cvtsi2ss
                | M::Cvtsi2sd
                | M::Cvtss2si
                | M::Cvttss2si
                | M::Cvtsd2si
                | M::Cvttsd2si
                | M::Cvtdq2ps
                | M::Cvtps2dq
                | M::Cvttps2dq
                | M::Cvtpd2dq
                | M::Cvttpd2dq
        ) {
            self.sse_conversion_memory(i);
        }
        match m {
            M::Prefetchnta | M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 => {}
            M::Sfence | M::Lfence => {}
            M::Mfence => {
                self.b.ins().fence();
            }
            M::Movnti => {
                let value = self.read(i, 1);
                let addr = self.address(i);
                self.direct_store(addr, value);
            }
            M::Movaps
            | M::Movups
            | M::Movapd
            | M::Movupd
            | M::Movntps
            | M::Movntpd
            | M::Movntdq => {
                let value = self.sse_vector(i, 1, types::I8X16);
                if i.op0_kind() == OpKind::Memory {
                    self.sse_store(i, value, 16);
                } else {
                    self.sse_define(i.op0_register(), value);
                }
            }
            M::Movss | M::Movsd | M::Movq | M::Movd => {
                let ty = match m {
                    M::Movss | M::Movd => types::I32,
                    _ => types::I64,
                };
                if i.op0_kind() == OpKind::Memory {
                    let value = self.sse_register(i.op1_register(), types::I8X16);
                    self.sse_store(i, value, ty.bytes());
                } else if !xmm(i, 0) {
                    // MOVD r32, xmm
                    let value = self.sse_scalar(i, 1, types::I32);
                    self.write(i, None, value);
                } else if xmm(i, 1) && matches!(m, M::Movss | M::Movsd) {
                    // Register MOVSS/MOVSD merge the low lane.
                    let lanes = ty.by(16 / ty.bytes()).unwrap();
                    let low = self.sse_scalar(i, 1, ty);
                    let old = self.sse_register(i.op0_register(), lanes);
                    let value = self.b.ins().insertlane(old, low, 0);
                    self.sse_define(i.op0_register(), value);
                } else {
                    // Loads, MOVQ and MOVD zero the rest of the register.
                    let low = if gpr(i, 1) {
                        self.read(i, 1)
                    } else {
                        self.sse_scalar(i, 1, ty)
                    };
                    let value = self
                        .b
                        .ins()
                        .scalar_to_vector(ty.by(16 / ty.bytes()).unwrap(), low);
                    self.sse_define(i.op0_register(), value);
                }
            }
            M::Movlps | M::Movhps | M::Movlpd | M::Movhpd | M::Movhlps | M::Movlhps => {
                let high = matches!(m, M::Movhps | M::Movhpd);
                if i.op0_kind() == OpKind::Memory {
                    let value = self.sse_register(i.op1_register(), types::I64X2);
                    let half = self.b.ins().extractlane(value, u8::from(high));
                    let addr = self.sse_address(i);
                    self.direct_store(addr, half);
                    return;
                }
                let half = match m {
                    M::Movhlps => {
                        let source = self.sse_register(i.op1_register(), types::I64X2);
                        self.b.ins().extractlane(source, 1)
                    }
                    M::Movlhps => {
                        let source = self.sse_register(i.op1_register(), types::I64X2);
                        self.b.ins().extractlane(source, 0)
                    }
                    _ => self.sse_scalar(i, 1, types::I64),
                };
                let lane = u8::from(matches!(m, M::Movhps | M::Movhpd | M::Movlhps));
                let old = self.sse_register(i.op0_register(), types::I64X2);
                let value = self.b.ins().insertlane(old, half, lane);
                self.sse_define(i.op0_register(), value);
            }
            M::Movmskps | M::Movmskpd => {
                let ty = if m == M::Movmskps {
                    types::I32X4
                } else {
                    types::I64X2
                };
                let value = self.sse_register(i.op1_register(), ty);
                let mask = self.b.ins().vhigh_bits(types::I32, value);
                self.write(i, None, mask);
            }
            M::Andps
            | M::Andpd
            | M::Andnps
            | M::Andnpd
            | M::Orps
            | M::Orpd
            | M::Xorps
            | M::Xorpd => {
                let a = self.sse_register(i.op0_register(), types::I8X16);
                let b = self.sse_vector(i, 1, types::I8X16);
                let value = match m {
                    M::Andps | M::Andpd => self.b.ins().band(a, b),
                    M::Andnps | M::Andnpd => {
                        let inverted = self.b.ins().bnot(a);
                        self.b.ins().band(inverted, b)
                    }
                    M::Orps | M::Orpd => self.b.ins().bor(a, b),
                    _ => self.b.ins().bxor(a, b),
                };
                self.sse_define(i.op0_register(), value);
            }
            M::Unpcklps
            | M::Unpckhps
            | M::Unpcklpd
            | M::Unpckhpd
            | M::Shufps
            | M::Shufpd
            | M::Pshufd => {
                let b = self.sse_vector(i, 1, types::I8X16);
                let a = if m == M::Pshufd {
                    b
                } else {
                    self.sse_register(i.op0_register(), types::I8X16)
                };
                let imm = if i.op_count() == 3 { i.immediate8() } else { 0 };
                let pick = |shift: u8| (imm >> shift) & 3;
                let (lanes, width): ([u8; 4], u8) = match m {
                    M::Unpcklps => ([0, 4, 1, 5], 4),
                    M::Unpckhps => ([2, 6, 3, 7], 4),
                    M::Unpcklpd => ([0, 2, 0, 0], 8),
                    M::Unpckhpd => ([1, 3, 0, 0], 8),
                    M::Shufps => ([pick(0), pick(2), 4 + pick(4), 4 + pick(6)], 4),
                    M::Shufpd => ([imm & 1, 2 + (imm >> 1 & 1), 0, 0], 8),
                    _ => ([pick(0), pick(2), pick(4), pick(6)], 4),
                };
                let count = 16 / usize::from(width);
                let value = self.sse_shuffle(a, b, &lanes[..count], width);
                self.sse_define(i.op0_register(), value);
            }
            M::Comiss | M::Ucomiss | M::Comisd | M::Ucomisd => {
                self.sse_default_control();
                let ty = if double(m) { types::F64 } else { types::F32 };
                let a = self.sse_scalar(i, 0, ty);
                let b = self.sse_scalar(i, 1, ty);
                let unordered = self.b.ins().fcmp(FloatCC::Unordered, a, b);
                let equal = self.b.ins().fcmp(FloatCC::Equal, a, b);
                let less = self.b.ins().fcmp(FloatCC::LessThan, a, b);
                let zero = self.b.ins().bor(unordered, equal);
                let carry = self.b.ins().bor(unordered, less);
                let [zero, parity, carry] =
                    [zero, unordered, carry].map(|flag| self.b.ins().uextend(types::I32, flag));
                // ZF, PF and CF report the order; OF, SF and AF are cleared.
                let old = self.b.use_var(self.flags);
                let mut flags = self.b.ins().band_imm(old, !0x8d5_i64);
                let zero_bit = self.b.ins().ishl_imm(zero, 6);
                let parity_bit = self.b.ins().ishl_imm(parity, 2);
                flags = self.b.ins().bor(flags, zero_bit);
                flags = self.b.ins().bor(flags, parity_bit);
                flags = self.b.ins().bor(flags, carry);
                self.set_raw_flags(flags);
            }
            M::Cmpss | M::Cmpsd | M::Cmpps | M::Cmppd => {
                self.sse_default_control();
                let lane = if double(m) { types::F64 } else { types::F32 };
                let lanes = lane.by(16 / lane.bytes()).unwrap();
                let a = self.sse_register(i.op0_register(), lanes);
                let value = if packed(m) {
                    let b = self.sse_vector(i, 1, lanes);
                    self.sse_compare(i.immediate8(), a, b)
                } else {
                    let low = self.b.ins().extractlane(a, 0);
                    let b = self.sse_scalar(i, 1, lane);
                    let mask = self.sse_compare(i.immediate8(), low, b);
                    let old = self.sse_cast(a, integer(lanes));
                    self.b.ins().insertlane(old, mask, 0)
                };
                self.sse_define(i.op0_register(), value);
            }
            _ if packed(m) => {
                self.sse_default_control();
                let ty = if double(m) {
                    types::F64X2
                } else {
                    types::F32X4
                };
                let b = self.sse_vector(i, 1, ty);
                let a = self.sse_register(i.op0_register(), ty);
                let value = self.sse_arithmetic(m, a, b);
                self.sse_define(i.op0_register(), value);
            }
            M::Addss
            | M::Subss
            | M::Mulss
            | M::Divss
            | M::Sqrtss
            | M::Minss
            | M::Maxss
            | M::Addsd
            | M::Subsd
            | M::Mulsd
            | M::Divsd
            | M::Sqrtsd
            | M::Minsd
            | M::Maxsd => {
                self.sse_default_control();
                let lane = if double(m) { types::F64 } else { types::F32 };
                let lanes = lane.by(16 / lane.bytes()).unwrap();
                let b = self.sse_scalar(i, 1, lane);
                let old = self.sse_register(i.op0_register(), lanes);
                let a = self.b.ins().extractlane(old, 0);
                let result = self.sse_arithmetic(m, a, b);
                let value = self.b.ins().insertlane(old, result, 0);
                self.sse_define(i.op0_register(), value);
            }
            M::Cvtsi2ss | M::Cvtsi2sd => {
                let lane = if m == M::Cvtsi2sd {
                    types::F64
                } else {
                    types::F32
                };
                let integer = self.read(i, 1);
                let result = self.sse_from_int_exact(integer, lane);
                let old = self.sse_register(i.op0_register(), lane.by(16 / lane.bytes()).unwrap());
                let value = self.b.ins().insertlane(old, result, 0);
                self.sse_define(i.op0_register(), value);
            }
            M::Cvtss2sd | M::Cvtsd2ss => {
                self.sse_default_control();
                let (from, to) = if m == M::Cvtss2sd {
                    (types::F32, types::F64)
                } else {
                    (types::F64, types::F32)
                };
                let source = self.sse_scalar(i, 1, from);
                let result = if m == M::Cvtss2sd {
                    self.b.ins().fpromote(to, source)
                } else {
                    self.b.ins().fdemote(to, source)
                };
                let old = self.sse_register(i.op0_register(), to.by(16 / to.bytes()).unwrap());
                let value = self.b.ins().insertlane(old, result, 0);
                self.sse_define(i.op0_register(), value);
            }
            M::Cvtss2si | M::Cvttss2si | M::Cvtsd2si | M::Cvttsd2si => {
                let mxcsr = self.sse_conversion_control();
                let lane = if matches!(m, M::Cvtsd2si | M::Cvttsd2si) {
                    types::F64
                } else {
                    types::F32
                };
                let source = self.sse_scalar(i, 1, lane);
                let (value, flags) =
                    self.sse_to_int(source, matches!(m, M::Cvttss2si | M::Cvttsd2si), mxcsr);
                self.sse_conversion_status(mxcsr, flags);
                self.write(i, None, value);
            }
            M::Cvtdq2ps => {
                let source = self.sse_vector(i, 1, types::I32X4);
                let value = self.sse_from_int_exact(source, types::F32X4);
                self.sse_define(i.op0_register(), value);
            }
            M::Cvtps2dq | M::Cvttps2dq => {
                let mxcsr = self.sse_conversion_control();
                let source = self.sse_vector(i, 1, types::F32X4);
                let (value, flags) = self.sse_to_int(source, m == M::Cvttps2dq, mxcsr);
                self.sse_conversion_status(mxcsr, flags);
                self.sse_define(i.op0_register(), value);
            }
            M::Cvtps2pd | M::Cvtdq2pd => {
                self.sse_default_control();
                let ty = if m == M::Cvtps2pd {
                    types::F32X4
                } else {
                    types::I32X4
                };
                let source = if i.op1_kind() == OpKind::Memory {
                    let low = self.sse_scalar(i, 1, types::I64);
                    let vector = self.b.ins().scalar_to_vector(types::I64X2, low);
                    self.sse_cast(vector, ty)
                } else {
                    self.sse_register(i.op1_register(), ty)
                };
                let value = if m == M::Cvtps2pd {
                    self.b.ins().fvpromote_low(source)
                } else {
                    // Exact: every int32 is a double.
                    let low = self.b.ins().extractlane(source, 0);
                    let high = self.b.ins().extractlane(source, 1);
                    let low = self.b.ins().fcvt_from_sint(types::F64, low);
                    let high = self.b.ins().fcvt_from_sint(types::F64, high);
                    let value = self.b.ins().scalar_to_vector(types::F64X2, low);
                    self.b.ins().insertlane(value, high, 1)
                };
                self.sse_define(i.op0_register(), value);
            }
            M::Cvtpd2ps => {
                self.sse_default_control();
                let source = self.sse_vector(i, 1, types::F64X2);
                let value = self.b.ins().fvdemote(source);
                self.sse_define(i.op0_register(), value);
            }
            M::Cvtpd2dq | M::Cvttpd2dq => {
                let mxcsr = self.sse_conversion_control();
                let source = self.sse_vector(i, 1, types::F64X2);
                let truncate = m == M::Cvttpd2dq;
                let low = self.b.ins().extractlane(source, 0);
                let high = self.b.ins().extractlane(source, 1);
                let (low, low_flags) = self.sse_to_int(low, truncate, mxcsr);
                let (high, high_flags) = self.sse_to_int(high, truncate, mxcsr);
                let flags = self.b.ins().bor(low_flags, high_flags);
                self.sse_conversion_status(mxcsr, flags);
                let value = self.b.ins().scalar_to_vector(types::I32X4, low);
                let value = self.b.ins().insertlane(value, high, 1);
                self.sse_define(i.op0_register(), value);
            }
            _ => unreachable!("{m:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::FOREIGN_NANS;
    use crate::{EXEC, Executor, READ, Region, RunOutcome, WRITE};
    use iced_x86::code_asm::*;
    use std::sync::atomic::Ordering;

    const CODE: u32 = 0x10000;
    const STOP: u32 = 0x18000;
    const DATA: u32 = 0x20000;
    const STACK: u32 = 0x2f800;

    const SINGLES: [u32; 16] = [
        0x0000_0000,
        0x8000_0000,
        0x3f80_0000,
        0xbfc0_0000,
        0x4020_0000,
        0x0001_0000, // denormal
        0x7f7f_ffff,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0001, // QNaN with payload
        0xffc0_0002, // negative QNaN
        0x7f80_0003, // SNaN
        0x4f32_d05e, // 3e9
        0xcf00_0000, // -2^31
        0xcf00_0001, // just below -2^31
        0x4eff_ffff, // just below 2^31
    ];
    const DOUBLES: [u64; 16] = [
        0,
        0x8000_0000_0000_0000,
        0x3ff0_0000_0000_0000,
        0xbff8_0000_0000_0000,
        0x4004_0000_0000_0000,
        0x0000_0000_0000_0001,
        0x7fef_ffff_ffff_ffff,
        0x7ff0_0000_0000_0000,
        0xfff0_0000_0000_0000,
        0x7ff8_0000_0000_0001,
        0xfff8_0000_0000_0002,
        0x7ff0_0000_0000_0003,
        0xc1e0_0000_0010_0000, // -2^31 - 0.5
        0xc1e0_0000_0020_0000, // -2^31 - 1
        0x41df_ffff_ffe0_0000, // 2^31 - 1
        0x41e0_0000_0000_0000, // 2^31
    ];

    type Build = fn(&mut CodeAssembler);

    fn program(op: Build, memory_source: bool) -> Vec<u8> {
        let mut a = CodeAssembler::new(32).unwrap();
        a.movups(xmm0, xmmword_ptr(DATA)).unwrap();
        a.movups(xmm1, xmmword_ptr(DATA + 16)).unwrap();
        a.movups(xmm2, xmmword_ptr(DATA + 16)).unwrap();
        a.mov(eax, 0x5a5a_5a5a).unwrap();
        if memory_source {
            // Memory forms read DATA+16 through EBX.
            a.mov(ebx, DATA + 16).unwrap();
        }
        op(&mut a);
        a.movups(xmmword_ptr(DATA + 32), xmm0).unwrap();
        a.mov(dword_ptr(DATA + 48), eax).unwrap();
        a.ret().unwrap();
        a.assemble(u64::from(CODE)).unwrap()
    }

    fn machine(bytes: &[u8], jit: bool) -> Executor {
        let mut m = Executor::new(CODE, 0x20000).unwrap();
        m.protect(CODE, 0x20000, READ | WRITE).unwrap();
        m.write(CODE, bytes).unwrap();
        m.protect(CODE, 4096, READ | EXEC).unwrap();
        m.protect(STOP, 4096, READ | EXEC).unwrap();
        if jit {
            m.add_region(Region::new(CODE, bytes.len())).unwrap();
        }
        m
    }

    fn run(m: &mut Executor, a: [u8; 16], b: [u8; 16], mxcsr: u32) {
        m.state.eip = CODE;
        m.state.regs[4] = STACK;
        m.state.eflags = 0xcd7;
        m.state.mxcsr = mxcsr;
        m.write(STACK, &STOP.to_le_bytes()).unwrap();
        m.write(DATA, &a).unwrap();
        m.write(DATA + 16, &b).unwrap();
        assert_eq!(m.run(STOP, 10_000).unwrap(), RunOutcome::ReachedStop);
    }

    fn lanes<const N: usize>(values: [u64; N]) -> [u8; 16] {
        let width = 16 / N;
        let mut bytes = [0; 16];
        for (lane, value) in values.iter().enumerate() {
            bytes[lane * width..(lane + 1) * width].copy_from_slice(&value.to_le_bytes()[..width]);
        }
        bytes
    }

    /// Every ordered pair of special values, packed into as few runs as the
    /// lane count allows.
    fn pairs(double: bool) -> Vec<([u8; 16], [u8; 16])> {
        let values: Vec<u64> = if double {
            DOUBLES.to_vec()
        } else {
            SINGLES.iter().map(|&v| u64::from(v)).collect()
        };
        let all: Vec<(u64, u64)> = values
            .iter()
            .flat_map(|&a| values.iter().map(move |&b| (a, b)))
            .collect();
        let count = if double { 2 } else { 4 };
        all.chunks(count)
            .map(|chunk| {
                let mut a = [0u64; 4];
                let mut b = [0u64; 4];
                for (lane, &(x, y)) in chunk.iter().enumerate() {
                    a[lane] = x;
                    b[lane] = y;
                }
                if double {
                    (lanes([a[0], a[1]]), lanes([b[0], b[1]]))
                } else {
                    (lanes(a), lanes(b))
                }
            })
            .collect()
    }

    fn operations() -> Vec<(&'static str, Build, bool)> {
        vec![
            ("addss", |a| a.addss(xmm0, xmm1).unwrap(), false),
            ("addss m", |a| a.addss(xmm0, dword_ptr(ebx)).unwrap(), false),
            ("subss", |a| a.subss(xmm0, xmm1).unwrap(), false),
            ("mulss", |a| a.mulss(xmm0, xmm1).unwrap(), false),
            ("divss", |a| a.divss(xmm0, xmm1).unwrap(), false),
            ("sqrtss", |a| a.sqrtss(xmm0, xmm1).unwrap(), false),
            ("minss", |a| a.minss(xmm0, xmm1).unwrap(), false),
            ("maxss", |a| a.maxss(xmm0, xmm1).unwrap(), false),
            ("addps", |a| a.addps(xmm0, xmm1).unwrap(), false),
            (
                "addps m",
                |a| a.addps(xmm0, xmmword_ptr(ebx)).unwrap(),
                false,
            ),
            ("subps", |a| a.subps(xmm0, xmm1).unwrap(), false),
            ("mulps", |a| a.mulps(xmm0, xmm1).unwrap(), false),
            ("divps", |a| a.divps(xmm0, xmm1).unwrap(), false),
            ("sqrtps", |a| a.sqrtps(xmm0, xmm1).unwrap(), false),
            ("minps", |a| a.minps(xmm0, xmm1).unwrap(), false),
            ("maxps", |a| a.maxps(xmm0, xmm1).unwrap(), false),
            ("addsd", |a| a.addsd(xmm0, xmm1).unwrap(), true),
            ("subsd m", |a| a.subsd(xmm0, qword_ptr(ebx)).unwrap(), true),
            ("mulsd", |a| a.mulsd(xmm0, xmm1).unwrap(), true),
            ("divsd", |a| a.divsd(xmm0, xmm1).unwrap(), true),
            ("sqrtsd", |a| a.sqrtsd(xmm0, xmm1).unwrap(), true),
            ("minsd", |a| a.minsd(xmm0, xmm1).unwrap(), true),
            ("maxsd", |a| a.maxsd(xmm0, xmm1).unwrap(), true),
            ("addpd", |a| a.addpd(xmm0, xmm1).unwrap(), true),
            ("mulpd", |a| a.mulpd(xmm0, xmm1).unwrap(), true),
            ("divpd", |a| a.divpd(xmm0, xmm1).unwrap(), true),
            ("sqrtpd", |a| a.sqrtpd(xmm0, xmm1).unwrap(), true),
            ("minpd", |a| a.minpd(xmm0, xmm1).unwrap(), true),
            ("maxpd", |a| a.maxpd(xmm0, xmm1).unwrap(), true),
            ("cmpltss", |a| a.cmpss(xmm0, xmm1, 1).unwrap(), false),
            ("cmpunordss", |a| a.cmpss(xmm0, xmm1, 3).unwrap(), false),
            ("cmpeqps", |a| a.cmpps(xmm0, xmm1, 0).unwrap(), false),
            ("cmpleps", |a| a.cmpps(xmm0, xmm1, 2).unwrap(), false),
            ("cmpneqps", |a| a.cmpps(xmm0, xmm1, 4).unwrap(), false),
            ("cmpnltps", |a| a.cmpps(xmm0, xmm1, 5).unwrap(), false),
            ("cmpnleps", |a| a.cmpps(xmm0, xmm1, 6).unwrap(), false),
            ("cmpordps", |a| a.cmpps(xmm0, xmm1, 7).unwrap(), false),
            ("cmpnltsd", |a| a.cmpsd_3(xmm0, xmm1, 5).unwrap(), true),
            ("cmpltpd", |a| a.cmppd(xmm0, xmm1, 1).unwrap(), true),
            ("comiss", |a| a.comiss(xmm0, xmm1).unwrap(), false),
            (
                "ucomiss m",
                |a| a.ucomiss(xmm0, dword_ptr(ebx)).unwrap(),
                false,
            ),
            ("comisd", |a| a.comisd(xmm0, xmm1).unwrap(), true),
            ("ucomisd", |a| a.ucomisd(xmm0, xmm1).unwrap(), true),
            ("cvtss2sd", |a| a.cvtss2sd(xmm0, xmm1).unwrap(), false),
            ("cvtsd2ss", |a| a.cvtsd2ss(xmm0, xmm1).unwrap(), true),
            ("cvtps2pd", |a| a.cvtps2pd(xmm0, xmm1).unwrap(), false),
            (
                "cvtps2pd m",
                |a| a.cvtps2pd(xmm0, qword_ptr(ebx)).unwrap(),
                false,
            ),
            ("cvtpd2ps", |a| a.cvtpd2ps(xmm0, xmm1).unwrap(), true),
            ("cvtss2si", |a| a.cvtss2si(eax, xmm1).unwrap(), false),
            ("cvttss2si", |a| a.cvttss2si(eax, xmm1).unwrap(), false),
            ("cvtsd2si", |a| a.cvtsd2si(eax, xmm1).unwrap(), true),
            (
                "cvttsd2si m",
                |a| a.cvttsd2si(eax, qword_ptr(ebx)).unwrap(),
                true,
            ),
            ("cvtps2dq", |a| a.cvtps2dq(xmm0, xmm1).unwrap(), false),
            ("cvttps2dq", |a| a.cvttps2dq(xmm0, xmm1).unwrap(), false),
            ("cvtpd2dq", |a| a.cvtpd2dq(xmm0, xmm1).unwrap(), true),
            ("cvttpd2dq", |a| a.cvttpd2dq(xmm0, xmm1).unwrap(), true),
            ("cvtdq2ps", |a| a.cvtdq2ps(xmm0, xmm1).unwrap(), false),
            ("cvtdq2pd", |a| a.cvtdq2pd(xmm0, xmm1).unwrap(), false),
            (
                "cvtsi2ss",
                |a| a.cvtsi2ss(xmm0, dword_ptr(ebx)).unwrap(),
                false,
            ),
            (
                "cvtsi2sd",
                |a| a.cvtsi2sd(xmm0, dword_ptr(ebx)).unwrap(),
                false,
            ),
            ("andps", |a| a.andps(xmm0, xmm1).unwrap(), false),
            ("andnpd", |a| a.andnpd(xmm0, xmm1).unwrap(), true),
            ("orps", |a| a.orps(xmm0, xmm1).unwrap(), false),
            (
                "xorps m",
                |a| a.xorps(xmm0, xmmword_ptr(ebx)).unwrap(),
                false,
            ),
            ("unpcklps", |a| a.unpcklps(xmm0, xmm1).unwrap(), false),
            ("unpckhps", |a| a.unpckhps(xmm0, xmm1).unwrap(), false),
            ("unpcklpd", |a| a.unpcklpd(xmm0, xmm1).unwrap(), true),
            ("unpckhpd", |a| a.unpckhpd(xmm0, xmm1).unwrap(), true),
            ("shufps", |a| a.shufps(xmm0, xmm1, 0x1b).unwrap(), false),
            ("shufps 2", |a| a.shufps(xmm0, xmm1, 0xe4).unwrap(), false),
            ("shufpd", |a| a.shufpd(xmm0, xmm1, 2).unwrap(), true),
            ("pshufd", |a| a.pshufd(xmm0, xmm1, 0x93).unwrap(), false),
            ("movss", |a| a.movss(xmm0, xmm1).unwrap(), false),
            ("movss m", |a| a.movss(xmm0, dword_ptr(ebx)).unwrap(), false),
            (
                "movss store",
                |a| a.movss(dword_ptr(DATA + 36), xmm1).unwrap(),
                false,
            ),
            ("movsd", |a| a.movsd_2(xmm0, xmm1).unwrap(), true),
            (
                "movsd m",
                |a| a.movsd_2(xmm0, qword_ptr(ebx)).unwrap(),
                true,
            ),
            ("movq", |a| a.movq(xmm0, xmm1).unwrap(), false),
            (
                "movq store",
                |a| a.movq(qword_ptr(DATA + 40), xmm1).unwrap(),
                false,
            ),
            (
                "movd load",
                |a| a.movd(xmm0, dword_ptr(ebx)).unwrap(),
                false,
            ),
            ("movd gpr", |a| a.movd(eax, xmm1).unwrap(), false),
            ("movd from gpr", |a| a.movd(xmm0, eax).unwrap(), false),
            ("movlps", |a| a.movlps(xmm0, qword_ptr(ebx)).unwrap(), false),
            ("movhps", |a| a.movhps(xmm0, qword_ptr(ebx)).unwrap(), false),
            (
                "movhps store",
                |a| a.movhps(qword_ptr(DATA + 40), xmm1).unwrap(),
                false,
            ),
            ("movhlps", |a| a.movhlps(xmm0, xmm1).unwrap(), false),
            ("movlhps", |a| a.movlhps(xmm0, xmm1).unwrap(), false),
            ("movmskps", |a| a.movmskps(eax, xmm1).unwrap(), false),
            ("movmskpd", |a| a.movmskpd(eax, xmm1).unwrap(), true),
            (
                "movaps",
                |a| a.movaps(xmm0, xmmword_ptr(ebx)).unwrap(),
                false,
            ),
            (
                "movntps",
                |a| a.movntps(xmmword_ptr(DATA + 32), xmm1).unwrap(),
                false,
            ),
            (
                "movnti",
                |a| a.movnti(dword_ptr(DATA + 44), eax).unwrap(),
                false,
            ),
            ("prefetch", |a| a.prefetchnta(byte_ptr(ebx)).unwrap(), false),
            ("mfence", |a| a.mfence().unwrap(), false),
        ]
    }

    fn compare_all(foreign: bool) {
        FOREIGN_NANS.store(foreign, Ordering::Relaxed);
        for (name, op, double) in operations() {
            let bytes = program(op, true);
            let mut jit = machine(&bytes, true);
            let mut reference = machine(&bytes, false);
            for (a, b) in pairs(double) {
                run(&mut jit, a, b, 0x1f80);
                run(&mut reference, a, b, 0x1f80);
                let context = format!("{name}, a {a:02x?}, b {b:02x?}, foreign {foreign}");
                assert_eq!(jit.state.xmm, reference.state.xmm, "{context}");
                assert_eq!(jit.state.regs, reference.state.regs, "{context}");
                assert_eq!(jit.state.eflags, reference.state.eflags, "{context}");
                if matches!(
                    name,
                    "cvtps2dq"
                        | "cvttps2dq"
                        | "cvtpd2dq"
                        | "cvttpd2dq"
                        | "cvtdq2ps"
                        | "cvtsi2ss"
                        | "cvtsi2sd"
                        | "cvtss2si"
                        | "cvttss2si"
                        | "cvtsd2si"
                        | "cvttsd2si"
                ) {
                    assert_eq!(jit.state.mxcsr, reference.state.mxcsr, "{context}");
                }
                assert_eq!(
                    jit.memory().read(DATA + 32, 32).unwrap(),
                    reference.memory().read(DATA + 32, 32).unwrap(),
                    "{context}"
                );
            }
            if matches!(name, "cvtdq2ps" | "cvtsi2ss") {
                // Inexact int32 -> binary32 lanes now require the guarded
                // exact path so rounding and precision status stay observable.
                assert!(jit.stats.interpreter_entries > 0, "{name}");
                assert!(jit.stats.native_entries > 0, "{name}");
            } else {
                assert_eq!(jit.stats.interpreter_entries, 0, "{name}");
            }
        }
        FOREIGN_NANS.store(false, Ordering::Relaxed);
    }

    #[test]
    fn native_sse_matches_the_interpreter_on_special_values() {
        compare_all(false);
        compare_all(true);
    }

    #[test]
    fn other_mxcsr_control_and_misalignment_fall_back_exactly() {
        let bytes = program(|a| a.addps(xmm0, xmmword_ptr(ebx)).unwrap(), true);
        let mut jit = machine(&bytes, true);
        let mut reference = machine(&bytes, false);
        let a = lanes([0x3f80_0000u64, 0x3eaa_aaab, 0x0001_0000, 0x7f7f_ffff]);
        let b = lanes([0x3eaa_aaabu64, 0x3f80_0000, 0x0000_0001, 0x7f7f_ffff]);
        // Round toward zero, DAZ, FTZ.
        for mxcsr in [0x7f80, 0x1fc0, 0x9f80] {
            run(&mut jit, a, b, mxcsr);
            run(&mut reference, a, b, mxcsr);
            assert_eq!(jit.state.xmm, reference.state.xmm, "{mxcsr:#x}");
            assert_eq!(jit.state.mxcsr, reference.state.mxcsr, "{mxcsr:#x}");
        }
        assert!(jit.stats.interpreter_entries > 0);
        // A misaligned packed operand raises #GP in the interpreter.
        let bytes = program(|a| a.addps(xmm0, xmmword_ptr(ebx + 4)).unwrap(), true);
        let mut jit = machine(&bytes, true);
        jit.state.eip = CODE;
        jit.state.regs[4] = STACK;
        jit.state.mxcsr = 0x1f80;
        let outcome = jit.run(STOP, 10_000).unwrap();
        assert!(
            matches!(outcome, RunOutcome::Trap { vector: 13, .. }),
            "{outcome:?}"
        );
    }
}
