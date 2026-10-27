//! x87 instructions on host doubles.
//!
//! Each instruction `fast::classify` accepts runs natively under the rules
//! of `adv_cpu_core::floating::fast`, reading and writing the stack in
//! `CpuState` directly, so fault sites, exits and helpers need no spill.
//! Whenever a rule does not hold (an exception pending, MMX state live, an
//! empty or full slot, an encoding that is not a zero or a normal, a value
//! outside the range or precision the rule admits) the instruction's
//! interpreter helper runs it in place and the unit goes on. Nothing is
//! stored before every check has passed, and memory is accessed only with
//! the guest flags in NZCV. Runs of such instructions keep the stack in
//! host registers instead (see `cache`).
//!
//! Arithmetic needs exact doubles within `2^±fast::RANGE`, masked
//! exceptions, round to nearest and precision 24, 53 or 64; it rounds once in
//! binary64 and takes the error from TwoSum, a fused multiply-subtract or
//! the division residual. Precision 64 admits only exact results. Precision 24
//! requires single-exact operands and rounds the significand in integers,
//! preserving the extended exponent range and the residual when rounding is unchanged.
//!
//! Copies keep the extended encoding. Stores and compares see a value as
//! the double holding its top 53 significand bits with the lowest one ORed
//! with the rest: rounding that double to 24 bits or to an integer below
//! 2^51 rounds the value, and it orders values as they do unless two equal
//! ones lost bits. Stores to binary64 round the significand in integers.

use adv_cpu_core::floating::fast::{self, Fast, Op as Arith, PE, RANGE, Source};
use iced_x86::{Instruction, OpKind};

use crate::asm::Label;
use crate::encode::Shift::Lsl;
use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::jit_map::Part;
use crate::runtime;

use super::decode::{Mem, memory};
use super::flags::Operand;
use super::{RUNTIME, Translator, WINDOW, X0, X1, X2, X3, X4, X5, X6, X13, X14, X15, X16, X17};

mod cache;
mod pair;
pub(super) use cache::{Arrive, Cache, Cold, Leave, Model};

const X7: Reg = Reg::new(7);
const X8: Reg = Reg::new(8);
const V0: VReg = VReg::new(0);
const V1: VReg = VReg::new(1);
const V2: VReg = VReg::new(2);
const V3: VReg = VReg::new(3);
const V4: VReg = VReg::new(4);
const V5: VReg = VReg::new(5);

const C0: u32 = 0x100;
const C1: u32 = 0x200;
const C2: u32 = 0x400;
const C3: u32 = 0x4000;
/// The extended exponent bias minus the binary64 one.
const REBIAS: u32 = 0x3fff - 1023;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct X87 {
    pub fast: Fast,
    /// The memory operand, if any.
    pub mem: Option<Mem>,
    /// The helper that runs the instruction otherwise.
    pub helper: u8,
    /// The opcode the instruction records, if it records one.
    pub opcode: Option<u16>,
}

/// The `X87` for `instruction`, whose interpreter helper is `helper`.
pub fn plan(instruction: &Instruction, helper: u8, opcode: Option<u16>) -> Option<X87> {
    let fast = fast::classify(instruction)?;
    let mem = if instruction.op0_kind() == OpKind::Memory {
        Some(memory(instruction)?)
    } else {
        None
    };
    if matches!(fast, Fast::StoreControl | Fast::StoreStatus) && mem.is_none() {
        // Only `fnstsw ax` stores to a register.
        if instruction.op0_register() != iced_x86::Register::AX {
            return None;
        }
    }
    Some(X87 {
        fast,
        mem,
        helper,
        opcode,
    })
}

/// How `double` takes an extended value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Take {
    /// Exactly, within `2^±range`.
    Exact(u32),
    /// Its top 53 bits, the lowest ORed with the rest, which sets the
    /// register to 1 when the rest is not zero; binary64 range.
    Sticky(Reg),
}

/// Per-instruction register use: x0 and x1 hold one extended value
/// (significand, sign and exponent), x2 and x3 another, x4 the guest NZCV,
/// x6 the guest address, x8 the control word, w13 the status word, w14 the
/// tag word, w15 TOP; x7, x16 and x17 are scratch.
impl Translator<'_> {
    pub(super) fn fild64(&mut self, mem: Mem, helper: u8, opcode: Option<u16>) {
        self.flush_simd();
        self.canonicalize();
        let address = self.effective(mem);
        self.emit(mov(W, X6, address));
        self.cpu();
        let slow = self.asm.label();
        let memory_slow = self.asm.label();
        let done = self.asm.label();
        self.emit_all(&[
            mrs_nzcv(X4),
            ldrh(X13, X5, runtime::X87_STATUS),
            ldr(W, X16, X5, runtime::MMX_MODE),
        ]);
        self.asm.tbz(true, X13, 7, slow);
        self.asm.cbz(W, true, X16, slow);
        self.emit_all(&[ubfx(W, X15, X13, 11, 3), ldrh(X14, X5, runtime::X87_TAG)]);
        let load = ldr_reg(X, X0, WINDOW, X6, false);
        if self.checking {
            self.emit(mov(X, X7, X4));
            self.checked_memory(X6, 8, false, load, memory_slow);
            self.emit(mov(X, X4, X7));
        } else {
            self.site();
            self.emit(load);
        }
        // Normalize the unsigned magnitude straight into raw80. No host-double
        // conversion or rounding loses the low bits of large signed integers.
        self.emit_all(&[
            asr_imm(X, X1, X0, 63),
            eor(X, X16, X0, X1, Lsl, 0),
            sub(X, X0, X16, X1, Lsl, 0),
            and_imm(W, X1, X1, 0x8000),
        ]);
        let zero = self.asm.label();
        self.asm.cbz(X, false, X0, zero);
        self.emit_all(&[
            clz(X, X16, X0),
            shiftv(X, Shift::Lsl, X0, X0, X16),
            movz(W, X17, 0x403e, 0),
            sub(W, X17, X17, X16, Lsl, 0),
            orr(W, X1, X1, X17, Lsl, 0),
        ]);
        self.asm.bind(zero);
        self.st_push(X0, X1, slow);
        self.clear_c1();
        self.emit_all(&[
            bfi(W, X13, X15, 11, 3),
            strh(X13, X5, runtime::X87_STATUS),
            strh(X14, X5, runtime::X87_TAG),
        ]);
        self.mov_imm(X16, self.trace.insts[self.index].pc);
        self.emit(str(W, X16, X5, runtime::X87_IP));
        if let Some(opcode) = opcode {
            self.emit_all(&[movz(W, X16, opcode, 0), strh(X16, X5, runtime::X87_OPCODE)]);
        }
        self.emit_all(&[str(W, X6, X5, runtime::X87_DP), msr_nzcv(X4)]);
        self.asm.b(done);
        self.asm.bind(memory_slow);
        if self.checking {
            self.emit(mov(X, X4, X7));
        }
        self.asm.bind(slow);
        self.emit(msr_nzcv(X4));
        self.helper(helper);
        self.asm.bind(done);
        self.vector = super::simd::Vector::default();
    }

    pub(super) fn x87(&mut self, x: X87) {
        if self.x87_cached(x) {
            return;
        }
        self.flush_simd();
        if self.checking && x.mem.is_some() {
            self.helper(x.helper);
            return;
        }
        self.canonicalize();
        if let Some(mem) = x.mem {
            let address = self.effective(mem);
            self.emit(mov(W, X6, address));
        }
        self.cpu();
        let slow = self.asm.label();
        let done = self.asm.label();
        self.emit_all(&[
            mrs_nzcv(X4),
            ldrh(X13, X5, runtime::X87_STATUS),
            ldr(W, X16, X5, runtime::MMX_MODE),
        ]);
        self.asm.tbz(true, X13, 7, slow);
        self.asm.cbz(W, true, X16, slow);
        if x.fast != Fast::Wait {
            self.emit_all(&[
                ubfx(W, X15, X13, 11, 3),
                ldrh(X14, X5, runtime::X87_TAG),
                ldrh(X8, X5, runtime::X87_CONTROL),
            ]);
        }
        let (dp, metadata) = self.x87_body(x, slow);
        if x.fast != Fast::Wait {
            self.emit_all(&[
                bfi(W, X13, X15, 11, 3),
                strh(X13, X5, runtime::X87_STATUS),
                strh(X14, X5, runtime::X87_TAG),
            ]);
        }
        if metadata {
            let pc = self.trace.insts[self.index].pc;
            self.mov_imm(X16, pc);
            self.emit(str(W, X16, X5, runtime::X87_IP));
            if let Some(opcode) = x.opcode {
                self.emit_all(&[movz(W, X16, opcode, 0), strh(X16, X5, runtime::X87_OPCODE)]);
            }
        }
        if dp {
            self.emit(str(W, X6, X5, runtime::X87_DP));
        }
        self.emit(msr_nzcv(X4));
        self.asm.b(done);
        self.asm.bind(slow);
        self.emit(msr_nzcv(X4));
        self.helper(x.helper);
        self.emit(ldr(X, X5, RUNTIME, runtime::CPU_STATE));
        self.asm.bind(done);
        // The stack may have changed under the MMX registers.
        self.vector = super::simd::Vector::default();
        self.vector.cpu = true;
    }

    /// What instruction `x` does once its checks have run; whether it
    /// records the data pointer and the instruction pointer and opcode.
    fn x87_body(&mut self, x: X87, slow: Label) -> (bool, bool) {
        let mut dp = x.mem.is_some();
        let mut metadata = true;
        match x.fast {
            Fast::Wait => metadata = false,
            Fast::Round => self.x87_round(slow),
            Fast::Scale => self.x87_scale(slow),
            Fast::Examine => {
                self.x87_examine();
                metadata = false;
            }
            Fast::Load(source) => self.x87_load(source, slow),
            Fast::Constant(one) => {
                if one {
                    self.emit_all(&[movz(X, X0, 0x8000, 48), movz(W, X1, 0x3fff, 0)]);
                } else {
                    self.emit_all(&[movz(X, X0, 0, 0), movz(W, X1, 0, 0)]);
                }
                self.st_push(X0, X1, slow);
                self.clear_c1();
            }
            Fast::Store {
                width,
                integer,
                pop,
            } => self.x87_store(width, integer, pop, slow),
            Fast::StoreSt { index, pop } => {
                self.read(0, X0, X1, slow);
                self.ordinary(X0, X1, slow);
                let to = self.slot(index, X2);
                self.commit_value(to, X0, X1);
                self.clear_c1();
                if pop {
                    self.pop();
                }
            }
            Fast::Arith {
                op,
                dest,
                source,
                reverse,
                pop,
            } => self.x87_arith(op, dest, source, reverse, pop, slow),
            Fast::Compare { source, pops } => self.x87_compare(source, pops, slow),
            Fast::Exchange(index) => {
                self.read(0, X0, X1, slow);
                self.ordinary(X0, X1, slow);
                self.read(index, X2, X3, slow);
                self.ordinary(X2, X3, slow);
                let other = self.slot(index, X7);
                self.commit_value(X15, X2, X3);
                self.commit_value(other, X0, X1);
                self.clear_c1();
            }
            Fast::Negate | Fast::Absolute => {
                self.read(0, X0, X1, slow);
                self.ordinary(X0, X1, slow);
                self.emit(if x.fast == Fast::Negate {
                    eor_imm(W, X1, X1, 0x8000)
                } else {
                    and_imm(W, X1, X1, 0x7fff)
                });
                // The tag follows the value, as on every write.
                self.st_write(0, X0, X1);
                self.clear_c1();
            }
            Fast::LoadControl => {
                self.site();
                self.emit(ldr_narrow(Narrow::Half, X0, WINDOW, X6));
                // Unmasking a pending exception takes the interpreter.
                self.emit_all(&[bic(W, X16, X13, X0, Lsl, 0), and_imm(W, X16, X16, 0x3f)]);
                self.asm.cbz(W, true, X16, slow);
                self.emit_all(&[
                    strh(X0, X5, runtime::X87_CONTROL),
                    movz(W, X16, 0x8080, 0),
                    bic(W, X13, X13, X16, Lsl, 0),
                ]);
                (dp, metadata) = (false, false);
            }
            Fast::StoreControl | Fast::StoreStatus => {
                let value = if x.fast == Fast::StoreControl {
                    X8
                } else {
                    X13
                };
                match x.mem {
                    Some(_) => {
                        self.site();
                        self.emit(str_narrow(Narrow::Half, value, WINDOW, X6));
                    }
                    None => {
                        self.clobber(Operand::Guest(0));
                        self.emit(bfi(W, super::host(0), value, 0, 16));
                    }
                }
                (dp, metadata) = (false, false);
            }
        }
        (dp, metadata)
    }

    fn x87_round(&mut self, slow: Label) {
        self.emit(and_imm(W, X16, X8, 0x3f));
        self.emit(cmp_imm(W, X16, 0x3f, false));
        self.asm.b_cond(Cond::Ne, slow);
        self.read(0, X0, X1, slow);
        self.double(X0, X1, V0, Take::Exact(RANGE as u32), slow);
        self.emit(ubfx(W, X2, X8, 10, 2));
        let rounded = self.asm.label();
        for (rc, op) in [Unary::Frintn, Unary::Frintm, Unary::Frintp, Unary::Frintz]
            .into_iter()
            .enumerate()
        {
            let next = self.asm.label();
            self.emit(cmp_imm(W, X2, rc as u32, false));
            self.asm.b_cond(Cond::Ne, next);
            self.emit(unary(op, V1, V0));
            self.asm.b(rounded);
            self.asm.bind(next);
        }
        self.asm.bind(rounded);
        self.grew(V1, V0);
        self.extended(V1, X0, X1);
        self.st_write(0, X0, X1);
        self.emit_all(&[orr(W, X13, X13, X3, Lsl, 5), bfi(W, X13, X7, 9, 1)]);
    }

    fn x87_scale(&mut self, slow: Label) {
        self.read(0, X0, X1, slow);
        self.ordinary(X0, X1, slow);
        self.read(1, X2, X3, slow);
        self.double(X2, X3, V0, Take::Exact(14), slow);
        self.emit(fcvt_int(W, true, X2, V0));
        let commit = self.asm.label();
        self.asm.cbz(X, false, X0, commit);
        self.emit_all(&[
            and_imm(W, X16, X1, 0x7fff),
            add(W, X16, X16, X2, Lsl, 0),
            sub_imm(W, X17, X16, 1, false),
            movz(W, X2, 0x7ffd, 0),
            cmp(W, X17, X2, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Hi, slow);
        self.emit(bfi(W, X1, X16, 0, 15));
        self.asm.bind(commit);
        self.st_write(0, X0, X1);
        self.clear_c1();
    }

    fn x87_examine(&mut self) {
        let [empty, zero_exp, normal, infinite, nan, denormal, zero, done] =
            std::array::from_fn(|_| self.asm.label());
        self.slot_base(X15, X17);
        self.emit_all(&[
            ldr(X, X0, X17, 0),
            ldrh(X1, X17, 8),
            movz(W, X16, (C0 | C1 | C2 | C3) as u16, 0),
            bic(W, X13, X13, X16, Lsl, 0),
            ubfx(W, X16, X1, 15, 1),
            bfi(W, X13, X16, 9, 1),
            lsl_imm(W, X16, X15, 1),
            shiftv(W, Shift::Lsr, X16, X14, X16),
            and_imm(W, X16, X16, 3),
            cmp_imm(W, X16, 3, false),
        ]);
        self.asm.b_cond(Cond::Eq, empty);
        self.emit(and_imm(W, X1, X1, 0x7fff));
        self.asm.cbz(W, false, X1, zero_exp);
        self.asm.tbz(false, X0, 63, done);
        self.emit_all(&[movz(W, X16, 0x7fff, 0), cmp(W, X1, X16, Lsl, 0)]);
        self.asm.b_cond(Cond::Ne, normal);
        self.emit(eor_imm(X, X16, X0, 1 << 63));
        self.asm.cbz(X, false, X16, infinite);
        self.asm.b(nan);
        self.asm.bind(zero_exp);
        self.asm.cbz(X, false, X0, zero);
        self.asm.b(denormal);
        for (label, flags) in [
            (empty, C3 | C0),
            (zero, C3),
            (denormal, C3 | C2),
            (normal, C2),
            (infinite, C2 | C0),
            (nan, C0),
        ] {
            self.asm.bind(label);
            self.emit_all(&[movz(W, X16, flags as u16, 0), orr(W, X13, X13, X16, Lsl, 0)]);
            self.asm.b(done);
        }
        self.asm.bind(done);
    }

    fn x87_load(&mut self, source: Source, slow: Label) {
        match source {
            Source::St(index) => {
                self.read(index, X0, X1, slow);
                self.ordinary(X0, X1, slow);
            }
            source => {
                self.memory_operand(source, V0, None, slow);
                self.extended(V0, X0, X1);
            }
        }
        self.st_push(X0, X1, slow);
        self.clear_c1();
    }

    fn x87_store(&mut self, width: u8, integer: bool, pop: bool, slow: Label) {
        self.read(0, X0, X1, slow);
        self.ordinary(X0, X1, slow);
        if integer {
            self.store_rounding(width, true, slow);
        }
        if integer || width == 4 {
            self.double(X0, X1, V0, Take::Sticky(X3), slow);
            self.store_sticky(width, integer, slow);
        } else {
            // Round the significand to 53 bits, nearest even, in integers.
            let zero = self.asm.label();
            self.emit_all(&[
                lsr_imm(W, X16, X1, 15),
                lsl_imm(X, X2, X16, 63),
                movz(W, X3, 0, 0),
                movz(W, X7, 0, 0),
            ]);
            self.asm.cbz(X, false, X0, zero);
            self.emit(and_imm(W, X16, X1, 0x7fff));
            self.unbias(X16, 1022);
            self.emit(cmp_imm(W, X16, 2045, false));
            self.asm.b_cond(Cond::Hi, slow);
            // w16 is the binary64 exponent - 1, which the explicit bit of
            // the rounded significand completes; a carry out of the
            // significand carries into the exponent.
            self.emit_all(&[
                and_imm(X, X3, X0, 0x7ff),
                ubfx(X, X17, X0, 11, 1),
                add(X, X17, X3, X17, Lsl, 0),
                cmp_imm(X, X17, 0x400, false),
                cset(W, X7, Cond::Hi),
                lsr_imm(X, X17, X0, 11),
                add(X, X17, X17, X7, Lsl, 0),
                add(X, X17, X17, X16, Lsl, 52),
                orr(X, X2, X2, X17, Lsl, 0),
            ]);
            self.asm.bind(zero);
            self.store_rounding(width, false, slow);
            self.emit_all(&[ubfx(X, X17, X2, 52, 11), cmp_imm(W, X17, 0x7ff, false)]);
            self.asm.b_cond(Cond::Eq, slow);
        }
        self.store_flags(slow);
        self.emit(msr_nzcv(X4));
        self.site();
        let size = if width == 8 { X } else { W };
        self.emit(str_reg(size, X2, WINDOW, X6, false));
        if pop {
            self.pop();
        }
    }

    /// For integers, checks the control and leaves it in w7. For floats,
    /// adjusts nearest x2 and its magnitude increment w7 when w3 is inexact.
    /// Uses x16 and x17.
    fn store_rounding(&mut self, width: u8, integer: bool, slow: Label) {
        self.emit(ubfx(W, X17, X8, 10, 2));
        if integer {
            let nearest = self.asm.label();
            self.asm.cbz(W, false, X17, nearest);
            self.emit(cmp_imm(W, X17, 3, false));
            self.asm.b_cond(Cond::Ne, slow);
            self.asm.bind(nearest);
            self.emit(mov(W, X7, X17));
        } else {
            let done = self.asm.label();
            self.asm.cbz(W, false, X17, done);
            self.asm.cbz(W, false, X3, done);
            let size = if width == 8 { X } else { W };
            // Truncate the magnitude, then add a unit for down on a
            // negative value or up on a positive one.
            self.emit_all(&[
                lsr_imm(size, X16, X2, u32::from(width) * 8 - 1),
                sub(size, X2, X2, X7, Lsl, 0),
                sub_imm(W, X17, X17, 1, false),
                eor(W, X17, X17, X16, Lsl, 0),
                cmp_imm(W, X17, 1, false),
                cset(W, X7, Cond::Eq),
                add(size, X2, X2, X7, Lsl, 0),
            ]);
            self.asm.bind(done);
        }
    }

    /// The bits of an integer or binary32 store in x2 from the value's
    /// sticky double in d0 (w3 whether it lost bits), w3 whether they are
    /// inexact and w7 whether rounding grew the magnitude.
    fn store_sticky(&mut self, width: u8, integer: bool, slow: Label) {
        if integer {
            // Below 2^63, and below 2^51 when bits were lost.
            let whole = self.asm.label();
            self.emit_all(&[fmov_from_v(X, X16, V0), ubfx(X, X16, X16, 52, 11)]);
            self.asm.cbz(W, false, X3, whole);
            self.emit(cmp_imm(W, X16, 1023 + 50, false));
            self.asm.b_cond(Cond::Hi, slow);
            self.asm.bind(whole);
            self.emit(cmp_imm(W, X16, 1023 + 62, false));
            self.asm.b_cond(Cond::Hi, slow);
            let rounded = self.asm.label();
            self.emit(unary(Unary::Frintn, V1, V0));
            self.asm.cbz(W, false, X7, rounded);
            self.emit(unary(Unary::Frintz, V1, V0));
            self.asm.bind(rounded);
            self.emit(fcvt_int(X, true, X2, V1));
            if width == 4 {
                self.emit_all(&[sbfx(X, X16, X2, 0, 32), cmp(X, X16, X2, Lsl, 0)]);
                self.asm.b_cond(Cond::Ne, slow);
            }
            self.grew(V1, V0);
        } else {
            // A normal input and output: no underflow or overflow.
            let zero = self.asm.label();
            self.emit_all(&[fmov_from_v(X, X16, V0), ubfx(X, X16, X16, 52, 11)]);
            self.asm.cbz(W, false, X16, zero);
            self.emit_all(&[
                sub_imm(W, X16, X16, 1023 - 126, false),
                cmp_imm(W, X16, 253, false),
            ]);
            self.asm.b_cond(Cond::Hi, slow);
            self.asm.bind(zero);
            self.emit_all(&[unary(Unary::Narrow, V1, V0), fmov_from_v(W, X2, V1)]);
            self.emit(unary(Unary::Widen, V1, V1));
            self.grew(V1, V0);
            self.store_rounding(width, false, slow);
            self.emit_all(&[ubfx(W, X16, X2, 23, 8), cmp_imm(W, X16, 0xff, false)]);
            self.asm.b_cond(Cond::Eq, slow);
        }
    }

    /// The flags of a store whose w3 says it is inexact and w7 that it
    /// grew: inexact needs PE masked, sets PE, and C1 when rounded up.
    fn store_flags(&mut self, slow: Label) {
        let exact = self.asm.label();
        let flagged = self.asm.label();
        self.asm.cbz(W, false, X3, exact);
        self.emit(and_imm(W, X16, X8, u64::from(PE)));
        self.asm.cbz(W, false, X16, slow);
        self.emit_all(&[orr_imm(W, X13, X13, u64::from(PE)), bfi(W, X13, X7, 9, 1)]);
        self.asm.b(flagged);
        self.asm.bind(exact);
        self.clear_c1();
        self.asm.bind(flagged);
    }

    /// w3 whether `rounded` differs from `value`, w7 whether it is larger
    /// in magnitude.
    fn grew(&mut self, rounded: VReg, value: VReg) {
        self.emit_all(&[
            fcmp(rounded, value),
            cset(W, X3, Cond::Ne),
            unary(Unary::Fabs, V2, rounded),
            unary(Unary::Fabs, V3, value),
            fcmp(V2, V3),
            cset(W, X7, Cond::Gt),
        ]);
    }

    fn x87_arith(
        &mut self,
        op: Arith,
        dest: u8,
        source: Source,
        reverse: bool,
        pop: bool,
        slow: Label,
    ) {
        let range = RANGE as u32;
        if !matches!(source, Source::St(_)) {
            self.memory_operand(source, V1, Some(range), slow);
        }
        // Reserved precision takes the interpreter. Precision 64, directed
        // rounding and operands that are not exact doubles in range take
        // the integer path.
        let wide = self.asm.label();
        let end = self.asm.label();
        self.emit_all(&[ubfx(W, X16, X8, 8, 2), cmp_imm(W, X16, 1, false)]);
        self.asm.b_cond(Cond::Eq, slow);
        self.emit(cmp_imm(W, X16, 3, false));
        self.asm.b_cond(Cond::Eq, wide);
        self.emit(ubfx(W, X16, X8, 10, 2));
        self.asm.cbz(W, true, X16, wide);
        self.read(dest, X0, X1, slow);
        self.double(X0, X1, V0, Take::Exact(range), wide);
        if let Source::St(index) = source {
            self.read(index, X0, X1, slow);
            self.double(X0, X1, V1, Take::Exact(range), wide);
        }
        let full = self.asm.label();
        self.asm.tbz(true, X8, 9, full);
        self.emit_all(&[
            fmov_from_v(X, X16, V0),
            fmov_from_v(X, X17, V1),
            orr(X, X16, X16, X17, Lsl, 0),
            and_imm(X, X16, X16, fast::SINGLE_TAIL),
        ]);
        self.asm.cbz(X, true, X16, wide);
        self.asm.bind(full);
        let (a, b) = if reverse { (V1, V0) } else { (V0, V1) };
        // d2 the result; x16 the bits of the error, whose sign is that of
        // exact - rounded.
        match op {
            Arith::Add | Arith::Sub => {
                let b = if op == Arith::Sub {
                    self.emit(unary(Unary::Fneg, V5, b));
                    V5
                } else {
                    b
                };
                // TwoSum.
                self.emit_all(&[
                    dfloat(Float::Fadd, V2, a, b),
                    dfloat(Float::Fsub, V3, V2, a),
                    dfloat(Float::Fsub, V4, V2, V3),
                    dfloat(Float::Fsub, V4, a, V4),
                    dfloat(Float::Fsub, V3, b, V3),
                    dfloat(Float::Fadd, V3, V4, V3),
                    fmov_from_v(X, X16, V3),
                ]);
            }
            Arith::Mul => self.emit_all(&[
                dfloat(Float::Fmul, V2, a, b),
                fnmsub(V3, a, b, V2),
                fmov_from_v(X, X16, V3),
            ]),
            Arith::Div => {
                self.emit(fcmp_zero(b));
                self.asm.b_cond(Cond::Eq, slow);
                // The residual a - q*b, whose sign over b's is the error's.
                self.emit_all(&[
                    dfloat(Float::Fdiv, V2, a, b),
                    dfloat(Float::Fmul, V3, V2, b),
                    dfloat(Float::Fsub, V4, a, V3),
                    fnmsub(V5, V2, b, V3),
                    dfloat(Float::Fsub, V3, V4, V5),
                    fmov_from_v(X, X16, V3),
                    fmov_from_v(X, X17, b),
                    and_imm(X, X17, X17, 1 << 63),
                    eor(X, X16, X16, X17, Lsl, 0),
                ]);
                // A quotient within the range keeps its residual exact.
                let zero = self.asm.label();
                self.emit_all(&[fmov_from_v(X, X17, V2), ubfx(X, X17, X17, 52, 11)]);
                self.asm.cbz(W, false, X17, zero);
                self.emit_all(&[
                    sub_imm(W, X17, X17, 1023 - range, false),
                    cmp_imm(W, X17, 2 * range, false),
                ]);
                self.asm.b_cond(Cond::Hi, wide);
                self.asm.bind(zero);
            }
        }
        let exact = self.asm.label();
        let flagged = self.asm.label();
        self.emit(lsl_imm(X, X17, X16, 1));
        self.asm.cbz(X, false, X17, exact);
        // Inexact needs PE masked.
        self.asm.tbz(false, X8, 5, slow);
        self.emit_all(&[
            orr_imm(W, X13, X13, u64::from(PE)),
            fmov_from_v(X, X17, V2),
            eor(X, X17, X17, X16, Lsl, 0),
            lsr_imm(X, X17, X17, 63),
            bfi(W, X13, X17, 9, 1),
        ]);
        self.asm.b(flagged);
        self.asm.bind(exact);
        self.clear_c1();
        self.asm.bind(flagged);
        let rounded = self.asm.label();
        self.asm.tbz(true, X8, 9, rounded);
        // Round the significand, without binary32's exponent restriction.
        self.emit_all(&[
            fmov_from_v(X, X0, V2),
            and_imm(X, X3, X0, fast::SINGLE_TAIL),
        ]);
        self.asm.cbz(X, false, X3, rounded);
        self.mov_imm(X1, (1 << 28) - 1);
        self.emit_all(&[
            ubfx(X, X17, X0, 29, 1),
            add(X, X1, X0, X1, Lsl, 0),
            add(X, X1, X1, X17, Lsl, 0),
            and_imm(X, X1, X1, !fast::SINGLE_TAIL),
            cmp(X, X1, X0, Lsl, 0),
            cset(W, X17, Cond::Hi),
            orr_imm(W, X13, X13, u64::from(PE)),
            bfi(W, X13, X17, 9, 1),
            fmov_to_v(X, V2, X1),
        ]);
        self.asm.bind(rounded);
        self.extended(V2, X0, X1);
        self.asm.b(end);
        self.asm.bind(wide);
        self.remark(Part::X87Wide);
        self.x87_wide(op, dest, source, reverse, slow);
        self.asm.bind(end);
        self.remark(Part::Body);
        self.st_write(dest, X0, X1);
        if pop {
            self.pop();
        }
    }

    /// Arithmetic on the 64-bit significands in integers, rounded once to
    /// the precision and rounding control, into x0 and x1. Each operation
    /// leaves the significand in x0 with its top bit set, the bits below
    /// it in x7 (any lost bits ORed into bit 0), the biased exponent in w17
    /// and the sign in bit 15 of w1, or x1 the sign of a zero result.
    fn x87_wide(&mut self, op: Arith, dest: u8, source: Source, reverse: bool, slow: Label) {
        match source {
            Source::St(index) => self.read(index, X2, X3, slow),
            _ => self.extended(V1, X2, X3),
        }
        self.ordinary(X2, X3, slow);
        self.read(dest, X0, X1, slow);
        self.ordinary(X0, X1, slow);
        self.wide_ops(op, reverse, slow);
    }

    /// `x87_wide` on the destination in x0 and x1 and the source in x2
    /// and x3, both zeros or normals.
    fn wide_ops(&mut self, op: Arith, reverse: bool, slow: Label) {
        if reverse {
            for (a, b) in [(X0, X2), (X1, X3)] {
                self.emit_all(&[mov(X, X16, a), mov(X, a, b), mov(X, b, X16)]);
            }
        }
        let round = self.asm.label();
        let zero = self.asm.label();
        let store = self.asm.label();
        match op {
            Arith::Add | Arith::Sub => self.wide_add(op == Arith::Sub, round, zero),
            Arith::Mul => self.wide_mul(round, zero),
            Arith::Div => self.wide_div(round, zero, slow),
        }
        self.asm.bind(round);
        self.wide_round(slow);
        self.asm.b(store);
        self.asm.bind(zero);
        self.emit(movz(X, X0, 0, 0));
        self.clear_c1();
        self.asm.bind(store);
    }

    /// x0 + x2, or x0 - x2: the operands are aligned in a 128-bit window
    /// with the larger magnitude first, so a subtraction never borrows out.
    fn wide_add(&mut self, subtract: bool, round: Label, zero: Label) {
        if subtract {
            self.emit(eor_imm(W, X3, X3, 0x8000));
        }
        let (b_zero, a_zero, both) = (self.asm.label(), self.asm.label(), self.asm.label());
        self.asm.cbz(X, false, X2, b_zero);
        self.asm.cbz(X, false, X0, a_zero);
        let (ordered, swap) = (self.asm.label(), self.asm.label());
        self.emit_all(&[
            and_imm(W, X16, X1, 0x7fff),
            and_imm(W, X17, X3, 0x7fff),
            cmp(W, X16, X17, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Hi, ordered);
        self.asm.b_cond(Cond::Lo, swap);
        self.emit(cmp(X, X0, X2, Lsl, 0));
        self.asm.b_cond(Cond::Hs, ordered);
        self.asm.bind(swap);
        for (a, b) in [(X0, X2), (X1, X3)] {
            self.emit_all(&[mov(X, X16, a), mov(X, a, b), mov(X, b, X16)]);
        }
        self.asm.bind(ordered);
        // w16 the exponent difference, at most 127: beyond it the smaller
        // operand only makes the sum inexact. w8 bit 15 a subtraction.
        self.emit_all(&[
            and_imm(W, X16, X1, 0x7fff),
            and_imm(W, X17, X3, 0x7fff),
            sub(W, X16, X16, X17, Lsl, 0),
            movz(W, X17, 127, 0),
            cmp(W, X16, X17, Lsl, 0),
            csel(W, X16, X16, X17, Cond::Ls),
            eor(W, X8, X1, X3, Lsl, 0),
        ]);
        // x2:x3 the smaller significand shifted right by w16.
        let (same, far, aligned) = (self.asm.label(), self.asm.label(), self.asm.label());
        self.asm.cbz(W, false, X16, same);
        self.emit(cmp_imm(W, X16, 64, false));
        self.asm.b_cond(Cond::Hs, far);
        self.emit_all(&[
            movz(W, X17, 64, 0),
            sub(W, X17, X17, X16, Lsl, 0),
            shiftv(X, Shift::Lsl, X3, X2, X17),
            shiftv(X, Shift::Lsr, X2, X2, X16),
        ]);
        self.asm.b(aligned);
        self.asm.bind(far);
        self.emit_all(&[
            sub_imm(W, X16, X16, 64, false),
            shiftv(X, Shift::Lsr, X3, X2, X16),
            shiftv(X, Shift::Lsl, X17, X3, X16),
            cmp(X, X17, X2, Lsl, 0),
            cset(W, X17, Cond::Ne),
            orr(X, X3, X3, X17, Lsl, 0),
            movz(X, X2, 0, 0),
        ]);
        self.asm.b(aligned);
        self.asm.bind(same);
        self.emit(movz(X, X3, 0, 0));
        self.asm.bind(aligned);
        self.emit_all(&[and_imm(W, X17, X1, 0x7fff), and_imm(W, X1, X1, 0x8000)]);
        let difference = self.asm.label();
        self.asm.tbz(true, X8, 15, difference);
        // A carry out shifts right, keeping the lost bit.
        self.emit_all(&[mov(X, X7, X3), adds(X, X0, X0, X2, Lsl, 0)]);
        self.asm.b_cond(Cond::Lo, round);
        self.emit_all(&[
            and_imm(X, X16, X7, 1),
            orr(X, X7, X16, X7, Shift::Lsr, 1),
            orr(X, X7, X7, X0, Lsl, 63),
            lsr_imm(X, X0, X0, 1),
            orr_imm(X, X0, X0, 1 << 63),
            add_imm(W, X17, X17, 1, false),
        ]);
        self.asm.b(round);
        self.asm.bind(difference);
        let (high, cancel) = (self.asm.label(), self.asm.label());
        self.emit_all(&[subs(X, X7, Reg::ZR, X3, Lsl, 0), sbcs(X, X0, X0, X2)]);
        self.asm.cbz(X, true, X0, high);
        self.asm.cbz(X, false, X7, cancel);
        self.emit_all(&[
            mov(X, X0, X7),
            movz(X, X7, 0, 0),
            sub_imm(W, X17, X17, 64, false),
        ]);
        self.asm.bind(high);
        self.emit(clz(X, X16, X0));
        self.asm.cbz(W, false, X16, round);
        self.emit_all(&[
            shiftv(X, Shift::Lsl, X0, X0, X16),
            movz(W, X2, 64, 0),
            sub(W, X2, X2, X16, Lsl, 0),
            shiftv(X, Shift::Lsr, X2, X7, X2),
            orr(X, X0, X0, X2, Lsl, 0),
            shiftv(X, Shift::Lsl, X7, X7, X16),
            sub(W, X17, X17, X16, Lsl, 0),
        ]);
        self.asm.b(round);
        // An exact cancellation is +0, or -0 rounding down.
        self.asm.bind(cancel);
        self.emit_all(&[
            ldrh(X8, X5, runtime::X87_CONTROL),
            ubfx(W, X16, X8, 10, 2),
            cmp_imm(W, X16, 1, false),
            cset(W, X1, Cond::Eq),
            lsl_imm(W, X1, X1, 15),
        ]);
        self.asm.b(zero);
        // A zero operand leaves the other one, still to round.
        self.asm.bind(b_zero);
        self.asm.cbz(X, false, X0, both);
        self.emit_all(&[
            and_imm(W, X17, X1, 0x7fff),
            and_imm(W, X1, X1, 0x8000),
            movz(X, X7, 0, 0),
        ]);
        self.asm.b(round);
        self.asm.bind(a_zero);
        self.emit_all(&[
            mov(X, X0, X2),
            and_imm(W, X17, X3, 0x7fff),
            and_imm(W, X1, X3, 0x8000),
            movz(X, X7, 0, 0),
        ]);
        self.asm.b(round);
        // Two zeros: negative only if both are, or either rounding down.
        self.asm.bind(both);
        self.emit_all(&[
            ldrh(X8, X5, runtime::X87_CONTROL),
            ubfx(W, X16, X8, 10, 2),
            and(W, X17, X1, X3, Lsl, 0),
            orr(W, X2, X1, X3, Lsl, 0),
            cmp_imm(W, X16, 1, false),
            csel(W, X1, X2, X17, Cond::Eq),
            and_imm(W, X1, X1, 0x8000),
        ]);
        self.asm.b(zero);
    }

    /// x0 * x2: the 128-bit product, normalized by at most one bit.
    fn wide_mul(&mut self, round: Label, zero: Label) {
        self.emit_all(&[
            and_imm(W, X17, X1, 0x7fff),
            and_imm(W, X16, X3, 0x7fff),
            add(W, X17, X17, X16, Lsl, 0),
            eor(W, X1, X1, X3, Lsl, 0),
            and_imm(W, X1, X1, 0x8000),
        ]);
        self.asm.cbz(X, false, X0, zero);
        self.asm.cbz(X, false, X2, zero);
        self.emit_all(&[
            mul(X, X7, X0, X2),
            umulh(X0, X0, X2),
            // e_a + e_b - bias + 1, for a product of 2^127 or more.
            sub_imm(W, X17, X17, 0x3ffe >> 12, true),
            sub_imm(W, X17, X17, 0x3ffe & 0xfff, false),
        ]);
        self.asm.tbz(true, X0, 63, round);
        self.emit_all(&[
            lsl_imm(X, X0, X0, 1),
            orr(X, X0, X0, X7, Shift::Lsr, 63),
            lsl_imm(X, X7, X7, 1),
            sub_imm(W, X17, X17, 1, false),
        ]);
        self.asm.b(round);
    }

    /// x0 / x2 by long division in 32-bit digits (Hacker's Delight
    /// `divlu`), the remainder reduced to the bits `wide_round` reads.
    fn wide_div(&mut self, round: Label, zero: Label, slow: Label) {
        // A zero divisor raises ZE or IE.
        self.asm.cbz(X, false, X2, slow);
        self.emit_all(&[
            and_imm(W, X17, X1, 0x7fff),
            and_imm(W, X16, X3, 0x7fff),
            sub(W, X17, X17, X16, Lsl, 0),
            // e_a - e_b + bias - 1, for a quotient below 1.
            add_imm(W, X17, X17, 0x3ffe >> 12, true),
            add_imm(W, X17, X17, 0x3ffe & 0xfff, false),
            eor(W, X1, X1, X3, Lsl, 0),
            and_imm(W, X1, X1, 0x8000),
        ]);
        self.asm.cbz(X, false, X0, zero);
        // The dividend x0 * 2^(63 + s) as x0:x1 with s = (a >= b), which
        // keeps the high half below the divisor; x3 the divisor's high digit.
        self.emit_all(&[
            cmp(X, X0, X2, Lsl, 0),
            cset(W, X16, Cond::Hs),
            add(W, X17, X17, X16, Lsl, 0),
            fmov_to_v(X, V2, X17),
            fmov_to_v(X, V3, X1),
            and(X, X1, X0, X16, Lsl, 0),
            lsl_imm(X, X1, X1, 31),
            shiftv(X, Shift::Lsr, X0, X0, X16),
            lsr_imm(X, X3, X2, 32),
        ]);
        // x7 the high quotient digit; x16 its remainder digit.
        self.emit_all(&[udiv(X, X7, X0, X3), msub(X, X16, X7, X3, X0)]);
        self.wide_digit(X7, X1);
        self.emit_all(&[
            orr(X, X8, X1, X0, Lsl, 32),
            mul(X, X17, X7, X2),
            sub(X, X0, X8, X17, Lsl, 0),
            lsl_imm(X, X7, X7, 32),
            udiv(X, X1, X0, X3),
            msub(X, X16, X1, X3, X0),
        ]);
        self.wide_digit(X1, Reg::ZR);
        // x16 the remainder, then x8 the bits below the quotient: 0, 1
        // below half, 2^63 at half or 2^63 + 1 above it.
        let tail = self.asm.label();
        self.emit_all(&[
            orr(X, X7, X7, X1, Lsl, 0),
            lsl_imm(X, X8, X0, 32),
            mul(X, X17, X1, X2),
            sub(X, X16, X8, X17, Lsl, 0),
            sub(X, X17, X2, X16, Lsl, 0),
            movz(X, X8, 0, 0),
        ]);
        self.asm.cbz(X, false, X16, tail);
        self.emit_all(&[movz(X, X8, 1, 0), cmp(X, X16, X17, Lsl, 0)]);
        self.asm.b_cond(Cond::Lo, tail);
        self.emit(movz(X, X8, 0x8000, 48));
        self.asm.b_cond(Cond::Eq, tail);
        self.emit(orr_imm(X, X8, X8, 1));
        self.asm.bind(tail);
        self.emit_all(&[
            mov(X, X0, X7),
            mov(X, X7, X8),
            fmov_from_v(X, X17, V2),
            fmov_from_v(X, X1, V3),
        ]);
        self.asm.b(round);
    }

    /// Corrects the quotient digit `q`, whose remainder digit is x16, by
    /// the divisor x2 (high digit x3) and the next dividend digit `next`.
    fn wide_digit(&mut self, q: Reg, next: Reg) {
        let (again, lower, done) = (self.asm.label(), self.asm.label(), self.asm.label());
        self.asm.bind(again);
        self.emit(lsr_imm(X, X17, q, 32));
        self.asm.cbz(X, true, X17, lower);
        self.emit_all(&[
            mov(W, X17, X2),
            mul(X, X17, q, X17),
            orr(X, X8, next, X16, Lsl, 32),
            cmp(X, X17, X8, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Ls, done);
        self.asm.bind(lower);
        self.emit_all(&[
            sub_imm(X, q, q, 1, false),
            add(X, X16, X16, X3, Lsl, 0),
            lsr_imm(X, X17, X16, 32),
        ]);
        self.asm.cbz(X, false, X17, again);
        self.asm.bind(done);
    }

    /// Rounds x0:x7 to the precision and rounding control, sets PE and C1,
    /// and packs the result into x0 and x1; slow when PE is unmasked or
    /// the exponent leaves the normal range.
    fn wide_round(&mut self, slow: Label) {
        // w2 the bits below the precision: 40, 11 or 0. Below 64 bits,
        // x7 becomes the dropped bits with the old x7 ORed into bit 0.
        let full = self.asm.label();
        self.emit_all(&[
            ldrh(X8, X5, runtime::X87_CONTROL),
            ubfx(W, X16, X8, 8, 2),
            movz(W, X2, 0, 0),
            cmp_imm(W, X16, 3, false),
        ]);
        self.asm.b_cond(Cond::Eq, full);
        self.emit_all(&[
            movz(W, X2, 40, 0),
            movz(W, X3, 11, 0),
            cmp_imm(W, X16, 2, false),
            csel(W, X2, X3, X2, Cond::Eq),
            cmp_imm(X, X7, 0, false),
            cset(W, X3, Cond::Ne),
            movz(W, X16, 64, 0),
            sub(W, X16, X16, X2, Lsl, 0),
            shiftv(X, Shift::Lsl, X7, X0, X16),
            orr(X, X7, X7, X3, Lsl, 0),
            shiftv(X, Shift::Lsr, X0, X0, X2),
        ]);
        self.asm.bind(full);
        let (exact, directed, decided, ranged) = (
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
            self.asm.label(),
        );
        self.asm.cbz(X, false, X7, exact);
        self.asm.tbz(false, X8, 5, slow);
        // w3 whether to round the magnitude up.
        self.emit(ubfx(W, X16, X8, 10, 2));
        self.asm.cbz(W, true, X16, directed);
        self.emit_all(&[
            movz(X, X3, 0x8000, 48),
            cmp(X, X7, X3, Lsl, 0),
            cset(W, X3, Cond::Hi),
        ]);
        self.asm.b_cond(Cond::Ne, decided);
        self.emit(and_imm(W, X3, X0, 1));
        self.asm.b(decided);
        // Down rounds negative magnitudes up, up positive ones; chop none.
        self.asm.bind(directed);
        self.emit_all(&[ubfx(W, X3, X1, 15, 1), cmp_imm(W, X16, 1, false)]);
        self.asm.b_cond(Cond::Eq, decided);
        self.emit_all(&[eor_imm(W, X3, X3, 1), cmp_imm(W, X16, 2, false)]);
        self.asm.b_cond(Cond::Eq, decided);
        self.emit(movz(W, X3, 0, 0));
        self.asm.bind(decided);
        // A carry out of the significand carries into the exponent.
        self.emit_all(&[
            orr_imm(W, X13, X13, u64::from(PE)),
            bfi(W, X13, X3, 9, 1),
            add(X, X0, X0, X3, Lsl, 0),
            shiftv(X, Shift::Lsl, X0, X0, X2),
        ]);
        self.asm.cbz(X, true, X0, ranged);
        self.emit_all(&[movz(X, X0, 0x8000, 48), add_imm(W, X17, X17, 1, false)]);
        self.asm.b(ranged);
        self.asm.bind(exact);
        self.clear_c1();
        self.emit(shiftv(X, Shift::Lsl, X0, X0, X2));
        self.asm.bind(ranged);
        // Underflow and overflow take the interpreter.
        self.emit_all(&[
            sub_imm(W, X16, X17, 1, false),
            movz(W, X3, 0x7ffd, 0),
            cmp(W, X16, X3, Lsl, 0),
        ]);
        self.asm.b_cond(Cond::Hi, slow);
        self.emit(orr(W, X1, X1, X17, Lsl, 0));
    }

    fn x87_compare(&mut self, source: Source, pops: u8, slow: Label) {
        // w3 and w7 whether either operand lost bits.
        if !matches!(source, Source::St(_)) {
            self.memory_operand(source, V1, None, slow);
            self.emit(movz(W, X7, 0, 0));
        }
        self.read(0, X0, X1, slow);
        self.ordinary(X0, X1, slow);
        self.double(X0, X1, V0, Take::Sticky(X3), slow);
        if let Source::St(index) = source {
            self.read(index, X0, X1, slow);
            self.ordinary(X0, X1, slow);
            self.double(X0, X1, V1, Take::Sticky(X7), slow);
        }
        // w16 whether the values are equal, w17 whether ST(0) is less.
        let (ordered, tie, join) = (self.asm.label(), self.asm.label(), self.asm.label());
        self.emit_all(&[orr(W, X16, X3, X7, Lsl, 0), fcmp(V0, V1)]);
        self.asm.b_cond(Cond::Ne, ordered);
        self.asm.cbz(W, true, X16, tie);
        self.asm.bind(ordered);
        self.emit_all(&[cset(W, X16, Cond::Eq), cset(W, X17, Cond::Mi)]);
        self.asm.b(join);
        // Equal doubles that lost bits share sign and exponent: the
        // significands order the magnitudes.
        self.asm.bind(tie);
        self.read(0, X0, X1, slow);
        match source {
            Source::St(index) => self.read(index, X2, X3, slow),
            _ => self.extended(V1, X2, X3),
        }
        self.emit_all(&[
            cmp(X, X0, X2, Lsl, 0),
            cset(W, X16, Cond::Eq),
            cset(W, X17, Cond::Lo),
            cset(W, X2, Cond::Hi),
            tst_imm(W, X1, 0x8000),
            csel(W, X17, X2, X17, Cond::Ne),
        ]);
        self.asm.bind(join);
        self.emit_all(&[
            movz(W, X2, (C0 | C1 | C2 | C3) as u16, 0),
            bic(W, X13, X13, X2, Lsl, 0),
            orr(W, X13, X13, X16, Lsl, 14),
            orr(W, X13, X13, X17, Lsl, 8),
        ]);
        if pops == 2 {
            let second = self.slot(1, X7);
            self.free(second);
        }
        if pops > 0 {
            self.free(X15);
            self.emit_all(&[
                add_imm(W, X15, X15, u32::from(pops), false),
                and_imm(W, X15, X15, 7),
            ]);
        }
    }

    /// The memory operand as a double in `d`, rejecting encodings other
    /// than zeros and normals and doubles outside `2^±range`. Uses x16
    /// and x17.
    fn memory_operand(&mut self, source: Source, d: VReg, range: Option<u32>, slow: Label) {
        let bounds = match range {
            Some(range) => (1023 - range, 2 * range),
            None => (1, 2045),
        };
        self.memory_bounded(source, d, bounds, slow);
    }

    /// `memory_operand` with a binary64 operand's biased exponent minus
    /// `bounds.0` at most `bounds.1`, unless it is a zero.
    fn memory_bounded(&mut self, source: Source, d: VReg, bounds: (u32, u32), slow: Label) {
        self.site();
        match source {
            Source::F32 => {
                let ok = self.asm.label();
                self.emit_all(&[
                    vldr_reg(VMem::S, d, WINDOW, X6),
                    fmov_from_v(W, X16, d),
                    lsl_imm(W, X17, X16, 1),
                ]);
                self.asm.cbz(W, false, X17, ok);
                self.emit_all(&[
                    ubfx(W, X17, X16, 23, 8),
                    sub_imm(W, X17, X17, 1, false),
                    cmp_imm(W, X17, 253, false),
                ]);
                self.asm.b_cond(Cond::Hi, slow);
                self.asm.bind(ok);
                // Every single-precision normal is within any range used.
                self.emit(unary(Unary::Widen, d, d));
            }
            Source::F64 => {
                let ok = self.asm.label();
                self.emit_all(&[
                    vldr_reg(VMem::D, d, WINDOW, X6),
                    fmov_from_v(X, X16, d),
                    lsl_imm(X, X17, X16, 1),
                ]);
                self.asm.cbz(X, false, X17, ok);
                let (low, span) = bounds;
                self.emit_all(&[
                    ubfx(X, X17, X16, 52, 11),
                    sub_imm(W, X17, X17, low, false),
                    cmp_imm(W, X17, span, false),
                ]);
                self.asm.b_cond(Cond::Hi, slow);
                self.asm.bind(ok);
            }
            Source::I16 => self.emit_all(&[
                ldr_narrow(Narrow::SignedHalf, X16, WINDOW, X6),
                scvtf(W, d, X16),
            ]),
            Source::I32 => self.emit_all(&[ldr_reg(W, X16, WINDOW, X6, false), scvtf(W, d, X16)]),
            Source::St(_) => unreachable!("not a memory operand"),
        }
    }

    /// The physical register of stack slot `index`: w15 itself for 0,
    /// else `out`.
    fn slot(&mut self, index: u8, out: Reg) -> Reg {
        if index == 0 {
            return X15;
        }
        self.emit_all(&[
            add_imm(W, out, X15, u32::from(index), false),
            and_imm(W, out, out, 7),
        ]);
        out
    }

    /// The host address of physical register `p`, in `out`.
    fn slot_base(&mut self, p: Reg, out: Reg) {
        self.emit_all(&[
            add_imm(X, out, X5, runtime::X87_REGS, false),
            add(X, out, out, p, Lsl, 3),
            add(X, out, out, p, Lsl, 1),
        ]);
    }

    /// Loads stack slot `index`, which must not be empty; uses x7, x16
    /// and x17.
    fn read(&mut self, index: u8, sig: Reg, sexp: Reg, slow: Label) {
        let p = self.slot(index, X7);
        self.read_slot(p, sig, sexp, slow);
    }

    /// Writes an ordinary value to stack slot `index`; uses x7, x16 and
    /// x17.
    fn st_write(&mut self, index: u8, sig: Reg, sexp: Reg) {
        let p = self.slot(index, X7);
        self.commit_value(p, sig, sexp);
    }

    /// Pushes an ordinary value; uses x2, x16 and x17.
    fn st_push(&mut self, sig: Reg, sexp: Reg, slow: Label) {
        let target = self.push_target(slow);
        self.commit_value(target, sig, sexp);
        self.emit(mov(W, X15, target));
    }

    /// Loads physical register `p`, which must not be empty; uses x16 and
    /// x17.
    fn read_slot(&mut self, p: Reg, sig: Reg, sexp: Reg, slow: Label) {
        self.emit_all(&[
            lsl_imm(W, X16, p, 1),
            shiftv(W, Shift::Lsr, X16, X14, X16),
            mvn(W, X16, X16),
            and_imm(W, X16, X16, 3),
        ]);
        self.asm.cbz(W, false, X16, slow);
        self.slot_base(p, X17);
        self.emit_all(&[ldr(X, sig, X17, 0), ldrh(sexp, X17, 8)]);
    }

    /// The slot a push fills, in w2, which must be empty.
    fn push_target(&mut self, slow: Label) -> Reg {
        self.emit_all(&[
            sub_imm(W, X2, X15, 1, false),
            and_imm(W, X2, X2, 7),
            lsl_imm(W, X16, X2, 1),
            shiftv(W, Shift::Lsr, X16, X14, X16),
            mvn(W, X16, X16),
            and_imm(W, X16, X16, 3),
        ]);
        self.asm.cbz(W, true, X16, slow);
        X2
    }

    /// Rejects extended encodings other than zeros and normals; uses x16
    /// and x17.
    fn ordinary(&mut self, sig: Reg, sexp: Reg, slow: Label) {
        let zero = self.asm.label();
        let ok = self.asm.label();
        self.emit(and_imm(W, X16, sexp, 0x7fff));
        self.asm.tbz(false, sig, 63, zero);
        // A normal: the exponent is neither 0 nor 0x7fff.
        self.asm.cbz(W, false, X16, slow);
        self.emit(add_imm(W, X17, X16, 1, false));
        self.asm.tbz(true, X17, 15, slow);
        self.asm.b(ok);
        self.asm.bind(zero);
        self.emit(orr(X, X17, sig, X16, Lsl, 0));
        self.asm.cbz(X, true, X17, slow);
        self.asm.bind(ok);
    }

    /// Subtracts `0x3fff - low` from the extended exponent in `w`: the
    /// exponent relative to `2^-low`.
    fn unbias(&mut self, w: Reg, low: u32) {
        let k = 0x3fff - low;
        self.emit_all(&[
            sub_imm(W, w, w, k >> 12, true),
            sub_imm(W, w, w, k & 0xfff, false),
        ]);
    }

    /// The extended value as a double in `d` as `take` says; slow unless
    /// it is a zero or a normal in range. Uses x16 and x17.
    fn double(&mut self, sig: Reg, sexp: Reg, d: VReg, take: Take, slow: Label) {
        let zero = self.asm.label();
        let join = self.asm.label();
        self.emit_all(&[lsr_imm(W, X16, sexp, 15), lsl_imm(X, X16, X16, 63)]);
        if let Take::Sticky(lost) = take {
            self.emit(movz(W, lost, 0, 0));
        }
        self.asm.cbz(X, false, sig, zero);
        self.asm.tbz(false, sig, 63, slow);
        self.emit(tst_imm(X, sig, 0x7ff));
        let (low, high) = match take {
            Take::Exact(range) => {
                self.asm.b_cond(Cond::Ne, slow);
                (range, range)
            }
            Take::Sticky(lost) => {
                self.emit(cset(W, lost, Cond::Ne));
                (1022, 1023)
            }
        };
        self.emit(and_imm(W, X17, sexp, 0x7fff));
        self.unbias(X17, low);
        self.emit(cmp_imm(W, X17, low + high, false));
        self.asm.b_cond(Cond::Hi, slow);
        self.emit_all(&[
            add_imm(W, X17, X17, 1023 - low, false),
            orr(X, X16, X16, X17, Lsl, 52),
            ubfx(X, X17, sig, 11, 52),
            orr(X, X16, X16, X17, Lsl, 0),
        ]);
        if let Take::Sticky(lost) = take {
            self.emit(orr(X, X16, X16, lost, Lsl, 0));
        }
        self.asm.b(join);
        self.asm.bind(zero);
        self.emit(and_imm(W, X17, sexp, 0x7fff));
        self.asm.cbz(W, true, X17, slow);
        self.asm.bind(join);
        self.emit(fmov_to_v(X, d, X16));
    }

    /// The extended encoding of the zero or normal double in `d`; uses
    /// x16 and x17.
    fn extended(&mut self, d: VReg, sig: Reg, sexp: Reg) {
        let zero = self.asm.label();
        self.emit_all(&[
            fmov_from_v(X, X16, d),
            lsr_imm(X, sexp, X16, 63),
            lsl_imm(W, sexp, sexp, 15),
            ubfx(X, X17, X16, 52, 11),
            lsl_imm(X, sig, X16, 12),
            lsr_imm(X, sig, sig, 1),
        ]);
        self.asm.cbz(W, false, X17, zero);
        self.emit_all(&[
            orr_imm(X, sig, sig, 1 << 63),
            add_imm(W, X17, X17, REBIAS >> 12, true),
            add_imm(W, X17, X17, REBIAS & 0xfff, false),
            orr(W, sexp, sexp, X17, Lsl, 0),
        ]);
        self.asm.bind(zero);
    }

    /// Stores an ordinary value to physical register `p`, its MMX alias
    /// and its tag; uses x16 and x17.
    fn commit_value(&mut self, p: Reg, sig: Reg, sexp: Reg) {
        self.slot_base(p, X17);
        self.emit_all(&[
            str(X, sig, X17, 0),
            strh(sexp, X17, 8),
            add(X, X17, X5, p, Lsl, 3),
            str(X, sig, X17, runtime::MMX),
            // Tag 1 for a zero, else 0.
            lsl_imm(W, X16, p, 1),
            movz(W, X17, 3, 0),
            shiftv(W, Shift::Lsl, X17, X17, X16),
            bic(W, X14, X14, X17, Lsl, 0),
            clz(X, X17, sig),
            lsr_imm(X, X17, X17, 6),
            shiftv(W, Shift::Lsl, X17, X17, X16),
            orr(W, X14, X14, X17, Lsl, 0),
        ]);
    }

    /// Marks physical register `p` empty; uses x16 and x17.
    fn free(&mut self, p: Reg) {
        self.emit_all(&[
            lsl_imm(W, X16, p, 1),
            movz(W, X17, 3, 0),
            shiftv(W, Shift::Lsl, X17, X17, X16),
            orr(W, X14, X14, X17, Lsl, 0),
        ]);
    }

    fn pop(&mut self) {
        self.free(X15);
        self.emit_all(&[add_imm(W, X15, X15, 1, false), and_imm(W, X15, X15, 7)]);
    }

    fn clear_c1(&mut self) {
        self.emit(and_imm(W, X13, X13, u64::from(!C1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_x86::code_asm::*;

    fn decode(build: impl FnOnce(&mut CodeAssembler)) -> Instruction {
        let mut a = CodeAssembler::new(32).unwrap();
        build(&mut a);
        let bytes = a.assemble(0x1000).unwrap();
        iced_x86::Decoder::with_ip(32, &bytes, 0x1000, 0).decode()
    }

    // spec: A64F-FP-2
    #[test]
    fn plans_take_the_fast_forms_with_their_operands() {
        let load = plan(
            &decode(|a| a.fld(dword_ptr(esi + 8)).unwrap()),
            3,
            Some(0x146),
        )
        .unwrap();
        assert_eq!(load.fast, Fast::Load(Source::F32));
        assert_eq!(
            (load.mem.unwrap().disp, load.helper, load.opcode),
            (8, 3, Some(0x146))
        );
        let add = plan(&decode(|a| a.faddp(st1, st0).unwrap()), 0, None).unwrap();
        assert!(
            matches!(
                add.fast,
                Fast::Arith {
                    pop: true,
                    dest: 1,
                    ..
                }
            ) && add.mem.is_none()
        );
        assert!(plan(&decode(|a| a.fnstsw(ax).unwrap()), 0, None).is_some());
        let control = plan(&decode(|a| a.fldcw(word_ptr(esp + 4)).unwrap()), 0, None).unwrap();
        assert_eq!(control.fast, Fast::LoadControl);
        assert!(plan(&decode(|a| a.fld(tword_ptr(esi)).unwrap()), 0, None).is_none());
        assert!(plan(&decode(|a| a.fld(dword_ptr(esi).gs()).unwrap()), 0, None).is_none());
        assert!(plan(&decode(|a| a.fsqrt().unwrap()), 0, None).is_none());
    }
}
