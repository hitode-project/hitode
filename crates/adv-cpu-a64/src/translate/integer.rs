//! Integer instructions outside the lazy flag forms: rotates through
//! carry, exchanges, the widening multiply and divide, bit scans, byte
//! swaps and `sahf`.
//!
//! A divide whose divisor is zero or whose quotient does not fit leaves
//! the work, and the #DE, to the `interpret` helper.
//! Registers change only after every access, so a fault leaves the
//! instruction undone, as in the interpreter.

use iced_x86::{Instruction, Mnemonic as M, OpKind};

use crate::encode::Shift::{Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;

use super::decode::{Mem, Part, ShiftKind, Src, memory};
use super::flags::FlagSet;
use super::{
    Carry, Operand, Recipe, Sense, State, Translator, X6, X10, X11, X13, X14, X15, X16, X17,
    biased, host, temp,
};

const EAX: u8 = 0;
const ECX: u8 = 1;
const EDX: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integer {
    NarrowShift(ShiftKind, u8, u8, u32),
    NarrowNeg(u8, u8),
    /// BSF (false) or BSR of a word/dword source, retaining dst on zero.
    Scan(bool, u8, Src, u8),
    /// `rcl` (true) or `rcr` of a 32-bit register by 1 to 31.
    Through(bool, u8, u32),
    /// `xadd dst, src`.
    Xadd(Src, u8),
    /// `xchg dst, src`.
    Xchg(Src, u8),
    CompareExchange(Src, u8, u8),
    CompareExchange8(Mem, u8),
    Cbw,
    Bswap(u8),
    /// `mul` or `imul` (true) of EAX into EDX:EAX.
    Multiply(Src, bool),
    /// `div` or `idiv` (true) of EDX:EAX, with the helper that raises #DE.
    Divide(Src, bool, u8),
    Sahf,
    TestBit(Src, u8),
}

impl Integer {
    /// Flags read, flags written, and whether it has a fault site.
    pub fn effects(self) -> (FlagSet, FlagSet, bool) {
        use FlagSet as F;
        let mem = |src: Src| matches!(src, Src::Mem(_));
        match self {
            Integer::NarrowNeg(..) => (F::NONE, F::ALL, false),
            Integer::NarrowShift(kind, _, width, count) => {
                let mut writes = F::PF.or(F::ZF).or(F::SF);
                if kind == ShiftKind::Sar || count <= u32::from(width) * 8 {
                    writes = writes.or(F::CF);
                }
                if count == 1 {
                    writes = writes.or(F::OF);
                }
                if count == 0 {
                    writes = F::NONE;
                }
                (F::NONE, writes, false)
            }
            Integer::Scan(_, _, src, _) => (F::NONE, F::ZF, mem(src)),
            Integer::Through(_, _, count) => {
                let overflow = if count == 1 { F::OF } else { F::NONE };
                (F::CF, F::CF.or(overflow), false)
            }
            Integer::Xadd(dst, _) => (F::NONE, F::ALL, mem(dst)),
            Integer::Xchg(dst, _) => (F::NONE, F::NONE, mem(dst)),
            Integer::CompareExchange(dst, ..) => (F::NONE, F::ALL, mem(dst)),
            Integer::CompareExchange8(..) => (F::NONE, F::ZF, true),
            Integer::Bswap(_) | Integer::Cbw => (F::NONE, F::NONE, false),
            Integer::Multiply(src, _) => (F::NONE, F::CF.or(F::OF), mem(src)),
            // The helper reads every flag.
            Integer::Divide(..) => (F::NONE, F::NONE, true),
            Integer::Sahf => (F::NONE, F::ALL.minus(F::OF), false),
            Integer::TestBit(src, _) => (F::NONE, F::CF, mem(src)),
        }
    }
}

/// The integer instruction `instruction` is, when the translator handles
/// it; `helper` runs it when a divide faults.
pub fn plan(instruction: &Instruction, helper: u8) -> Option<Integer> {
    // ESP and EBP are biased in their host registers.
    let register = |n| {
        let register = instruction.op_register(n);
        (instruction.op_kind(n) == OpKind::Register && register.is_gpr32())
            .then_some(register.number() as u8)
            .filter(|&index| !biased(index))
    };
    let operand = |n| match instruction.op_kind(n) {
        OpKind::Register => register(n).map(Src::Reg),
        OpKind::Memory if instruction.memory_size().size() == 4 => {
            memory(instruction).map(Src::Mem)
        }
        _ => None,
    };
    let mnemonic = instruction.mnemonic();
    // Locked exchanges are ordinary ones: guest threads take turns.
    if instruction.has_lock_prefix()
        && !(matches!(mnemonic, M::Xadd | M::Xchg | M::Cmpxchg | M::Cmpxchg8b)
            && instruction.op_kind(0) == OpKind::Memory)
    {
        return None;
    }
    if mnemonic == M::Cmpxchg8b && instruction.op0_kind() == OpKind::Memory {
        return Some(Integer::CompareExchange8(memory(instruction)?, helper));
    }
    let dst = instruction.op0_register();
    if instruction.op0_kind() == OpKind::Register && (dst.is_gpr8() || dst.is_gpr16()) {
        let width = dst.size() as u8;
        if mnemonic == M::Neg {
            return Some(Integer::NarrowNeg(dst.number() as u8, width));
        }
        if matches!(mnemonic, M::Shl | M::Sal | M::Shr | M::Sar)
            && instruction.op1_kind() == OpKind::Immediate8
        {
            let kind = match mnemonic {
                M::Shr => ShiftKind::Shr,
                M::Sar => ShiftKind::Sar,
                _ => ShiftKind::Shl,
            };
            return Some(Integer::NarrowShift(
                kind,
                dst.number() as u8,
                width,
                u32::from(instruction.immediate8()) & 31,
            ));
        }
    }
    Some(match mnemonic {
        M::Cbw => Integer::Cbw,
        M::Bsf | M::Bsr | M::Tzcnt | M::Lzcnt => {
            let dst = instruction.op0_register();
            let width = if dst.is_gpr16() {
                2
            } else if dst.is_gpr32() {
                4
            } else {
                return None;
            };
            let src = match instruction.op1_kind() {
                OpKind::Register => {
                    let src = instruction.op1_register();
                    if !(src.is_gpr16() || src.is_gpr32()) || src.size() != width {
                        return None;
                    }
                    Src::Reg(src.number() as u8)
                }
                OpKind::Memory if instruction.memory_size().size() == width => {
                    Src::Mem(memory(instruction)?)
                }
                _ => return None,
            };
            Integer::Scan(
                adv_cpu_core::cpu_profile::reverse_scan(mnemonic),
                dst.number() as u8,
                src,
                width as u8,
            )
        }
        M::Rcl | M::Rcr if instruction.op_kind(1) == OpKind::Immediate8 => {
            match u32::from(instruction.immediate8()) & 31 {
                0 => return None,
                count => Integer::Through(mnemonic == M::Rcl, register(0)?, count),
            }
        }
        M::Xadd => Integer::Xadd(operand(0)?, register(1)?),
        M::Xchg => Integer::Xchg(operand(0)?, register(1)?),
        M::Cmpxchg => Integer::CompareExchange(operand(0)?, register(1)?, helper),
        M::Bswap => Integer::Bswap(register(0)?),
        M::Mul | M::Imul if instruction.op_count() == 1 => {
            Integer::Multiply(operand(0)?, mnemonic == M::Imul)
        }
        M::Div | M::Idiv => Integer::Divide(operand(0)?, mnemonic == M::Idiv, helper),
        M::Sahf => Integer::Sahf,
        M::Bt if instruction.op1_kind() == OpKind::Immediate8 => {
            Integer::TestBit(operand(0)?, instruction.immediate8() & 31)
        }
        _ => return None,
    })
}

impl Translator<'_> {
    pub(super) fn integer(&mut self, op: Integer) {
        match op {
            Integer::NarrowNeg(dst, width) => {
                self.canonicalize();
                self.narrow_read(Part::Reg(dst), width, Reg::ZR, X13);
                self.emit(sub(W, X6, Reg::ZR, X13, Lsl, 0));
                self.narrow_write(Part::Reg(dst), width, Reg::ZR, X6);
                self.emit_all(&[
                    mov(W, X10, X6),
                    eor(W, X11, X13, X6, Lsl, 0),
                    lsl_imm(W, X13, X13, 32 - 8 * u32::from(width)),
                    subs(W, Reg::ZR, Reg::ZR, X13, Lsl, 0),
                ]);
                self.state = State::CANONICAL;
            }
            Integer::NarrowShift(kind, dst, width, count) => {
                self.narrow_shift(kind, dst, width, count)
            }
            Integer::Scan(reverse, dst, src, width) => self.scan(reverse, dst, src, width),
            Integer::Through(left, dst, count) => self.through(left, dst, count),
            Integer::Xadd(dst, src) => self.xadd(dst, src),
            Integer::Xchg(dst, src) => self.xchg(dst, src),
            Integer::CompareExchange(dst, src, helper) => self.compare_exchange(dst, src, helper),
            Integer::CompareExchange8(mem, helper) => self.compare_exchange8(mem, helper),
            Integer::Cbw => {
                self.clobber(Operand::Guest(EAX));
                self.emit_all(&[sbfx(W, X13, host(EAX), 0, 8), bfi(W, host(EAX), X13, 0, 16)]);
            }
            Integer::Bswap(index) => {
                self.clobber(Operand::Guest(index));
                self.emit(rev(W, host(index), host(index)));
            }
            Integer::Multiply(src, signed) => self.multiply(src, signed),
            Integer::Divide(src, signed, helper) => self.divide(src, signed, helper),
            Integer::Sahf => self.sahf(),
            Integer::TestBit(src, bit) => {
                let source = self.source(src);
                self.convert(FlagSet::ALL.minus(FlagSet::CF));
                self.insert_flags(Some((source, u32::from(bit))), None);
                self.state.sense = Sense::Addition;
                self.state.carry = Carry::Host;
                self.state.derivable = None;
            }
        }
    }

    fn compare_exchange(&mut self, dst: Src, src: u8, helper: u8) {
        self.flush_simd();
        self.canonicalize();
        let slow = self.asm.label();
        let done = self.asm.label();
        match dst {
            Src::Reg(index) => self.emit(mov(W, X6, host(index))),
            Src::Mem(mem) => {
                let address = self.effective(mem);
                self.emit_all(&[mov(W, X17, address), and_imm(W, X13, X17, 3)]);
                self.asm.cbz(W, true, X13, slow);
                let load = ldr_reg(W, X6, super::WINDOW, X17, false);
                if self.checking {
                    self.checked_memory(X17, 4, false, load, slow);
                } else {
                    self.site();
                    self.emit(load);
                }
            }
            Src::Imm(_) => unreachable!(),
        }
        self.emit_all(&[mov(W, X15, host(EAX)), eor(W, X13, X15, X6, Lsl, 0)]);
        let unequal = self.asm.label();
        let flags = self.asm.label();
        self.asm.cbz(W, true, X13, unequal);
        match dst {
            Src::Reg(index) => self.emit(mov(W, host(index), host(src))),
            Src::Mem(_) => {
                let store = str_reg(W, host(src), super::WINDOW, X17, false);
                if self.checking {
                    self.checked_memory(X17, 4, true, store, slow);
                } else {
                    self.site();
                    self.emit(store);
                }
            }
            Src::Imm(_) => unreachable!(),
        }
        self.asm.b(flags);
        self.asm.bind(unequal);
        if matches!(dst, Src::Mem(_)) {
            // A mismatch still writes: read-only destinations must fault before
            // changing either EAX or the comparison flags.
            let store = str_reg(W, X6, super::WINDOW, X17, false);
            if self.checking {
                self.checked_memory(X17, 4, true, store, slow);
            } else {
                self.site();
                self.emit(store);
            }
        }
        self.emit(mov(W, host(EAX), X6));
        self.asm.bind(flags);
        self.emit_all(&[
            subs(W, X14, X15, X6, Lsl, 0),
            mov(W, X10, X14),
            eor(W, X11, X15, X6, Lsl, 0),
            eor(W, X11, X11, X14, Lsl, 0),
        ]);
        self.asm.b(done);
        self.asm.bind(slow);
        self.helper(helper);
        self.asm.bind(done);
        self.state = State::CANONICAL;
    }

    fn compare_exchange8(&mut self, mem: Mem, helper: u8) {
        self.flush_simd();
        if self.checking {
            self.helper(helper);
            return;
        }
        self.canonicalize();
        let address = self.effective(mem);
        self.emit(mov(W, X17, address));
        let slow = self.asm.label();
        self.emit(and_imm(W, X13, X17, 7));
        self.asm.cbz(W, true, X13, slow);
        self.site();
        self.emit(ldr_reg(X, X6, super::WINDOW, X17, false));
        self.emit_all(&[
            orr(X, X13, host(EAX), host(EDX), Lsl, 32),
            eor(X, X13, X13, X6, Lsl, 0),
        ]);
        let unequal = self.asm.label();
        let done = self.asm.label();
        self.asm.cbz(X, true, X13, unequal);
        self.emit(orr(X, X15, host(3), host(ECX), Lsl, 32));
        self.site();
        self.emit(str_reg(X, X15, super::WINDOW, X17, false));
        self.emit_all(&[mrs_nzcv(X14), orr_imm(X, X14, X14, 1 << 30), msr_nzcv(X14)]);
        self.asm.b(done);
        self.asm.bind(unequal);
        self.site();
        self.emit(str_reg(X, X6, super::WINDOW, X17, false));
        self.emit_all(&[
            mov(W, host(EAX), X6),
            lsr_imm(X, X13, X6, 32),
            mov(W, host(EDX), X13),
            mrs_nzcv(X14),
            and_imm(X, X14, X14, !(1u64 << 30)),
            msr_nzcv(X14),
        ]);
        self.asm.b(done);
        self.asm.bind(slow);
        self.helper(helper);
        self.asm.bind(done);
        self.state = State::CANONICAL;
    }

    fn narrow_shift(&mut self, kind: ShiftKind, dst: u8, width: u8, count: u32) {
        if count == 0 {
            return;
        }
        let bits = 8 * u32::from(width);
        self.canonicalize();
        self.emit(mrs_nzcv(X14));
        self.narrow_read(Part::Reg(dst), width, Reg::ZR, X13);
        let word = match kind {
            ShiftKind::Shl => lsl_imm(W, X6, X13, count),
            ShiftKind::Shr => lsr_imm(W, X6, X13, count),
            ShiftKind::Sar => {
                self.emit(sbfx(W, X6, X13, 0, bits));
                asr_imm(W, X6, X6, count)
            }
            _ => unreachable!(),
        };
        self.emit(word);
        self.emit(ubfx(W, X6, X6, 0, bits));
        self.narrow_write(Part::Reg(dst), width, Reg::ZR, X6);
        self.emit_all(&[
            mov(W, X10, X6),
            adds(W, Reg::ZR, Reg::ZR, X6, Lsl, 32 - bits),
        ]);
        if count > bits && kind != ShiftKind::Sar {
            // Canonical C is the inverse of the guest CF.
            self.emit_all(&[mvn(W, X15, X14), ubfx(W, X15, X15, 29, 1)]);
        } else {
            let bit = if kind == ShiftKind::Shl {
                bits - count
            } else {
                count.min(bits) - 1
            };
            self.emit(ubfx(W, X15, X13, bit, 1));
        }
        if count != 1 {
            self.emit(ubfx(W, X17, X14, 28, 1));
        } else {
            match kind {
                ShiftKind::Shr => self.emit(ubfx(W, X17, X13, bits - 1, 1)),
                ShiftKind::Shl => {
                    self.emit_all(&[eor(W, X17, X13, X6, Lsl, 0), ubfx(W, X17, X17, bits - 1, 1)])
                }
                _ => self.emit(movz(W, X17, 0, 0)),
            }
        }
        self.insert_flags(Some((X15, 0)), Some((X17, 0)));
        self.state = State {
            sense: Sense::Addition,
            ..State::CANONICAL
        };
    }

    /// A 32-bit source in a register: its own, or w6 loaded.
    fn source(&mut self, src: Src) -> Reg {
        match src {
            Src::Reg(index) => host(index),
            Src::Mem(mem) => {
                let addr = self.address(mem);
                self.load(addr, X6);
                X6
            }
            Src::Imm(_) => unreachable!(),
        }
    }

    fn scan(&mut self, reverse: bool, dst: u8, src: Src, width: u8) {
        // Capture the source, including a same-register operand, before any
        // write. The memory site observes the complete original flag recipe.
        match (src, width) {
            (Src::Reg(index), 4) => self.emit(mov(W, X6, host(index))),
            (Src::Reg(index), 2) => self.emit(ubfx(W, X6, host(index), 0, 16)),
            (Src::Mem(mem), 4) => {
                let addr = self.address(mem);
                self.load(addr, X6);
            }
            (Src::Mem(mem), 2) => {
                let addr = self.effective(mem);
                self.narrow_read(Part::Mem(mem), 2, addr, X6);
            }
            _ => unreachable!("validated bit scan source"),
        }
        if self.out.any(FlagSet::ZF) {
            self.canonicalize();
            self.emit_all(&[
                mrs_nzcv(X13),
                cmp_imm(W, X6, 0, false),
                cset(W, X14, Cond::Eq),
                bfi(X, X13, X14, 30, 1),
                msr_nzcv(X13),
            ]);
        }
        self.clobber(Operand::Guest(dst));
        let done = self.asm.label();
        self.asm.cbz(W, false, X6, done);
        if !reverse {
            self.emit(rbit(W, X6, X6));
        }
        self.emit(clz(W, X6, X6));
        if reverse {
            self.emit(eor_imm(W, X6, X6, 31));
        }
        let result = super::ops::result(dst);
        if width == 2 {
            if biased(dst) {
                self.emit(mov(W, result, host(dst)));
            }
            self.emit(bfi(W, result, X6, 0, 16));
        } else {
            self.emit(mov(W, result, X6));
        }
        self.commit(dst);
        self.asm.bind(done);
    }

    /// `rcl` or `rcr` by 1 to 31: the 33-bit rotate of CF and the register.
    fn through(&mut self, left: bool, dst: u8, count: u32) {
        let source = host(dst);
        self.convert(FlagSet::CF);
        self.clobber(Operand::Guest(dst));
        // CF is set when C is clear.
        self.emit(cset(W, X17, Cond::Lo));
        let (carry, overflow) = if left {
            self.emit_all(&[
                lsl_imm(W, X6, source, count),
                orr(W, X6, X6, X17, Lsl, count - 1),
            ]);
            if count > 1 {
                self.emit(orr(W, X6, X6, source, Lsr, 33 - count));
            }
            // OF is the new top bit XOR CF: the old top two bits.
            self.emit(eor(W, X15, source, source, Lsl, 1));
            ((source, 32 - count), (X15, 31))
        } else {
            self.emit_all(&[
                lsr_imm(W, X6, source, count),
                orr(W, X6, X6, X17, Lsl, 32 - count),
            ]);
            if count > 1 {
                self.emit(orr(W, X6, X6, source, Lsl, 33 - count));
            }
            // OF is the XOR of the new top two bits.
            self.emit(eor(W, X15, X6, X6, Lsl, 1));
            ((source, count - 1), (X15, 31))
        };
        let overflow = (count == 1 && self.out.any(FlagSet::OF)).then_some(overflow);
        self.insert_flags(Some(carry), overflow);
        self.emit(mov(W, source, X6));
        self.state = State {
            nz: None,
            overflow: super::Overflow::Host,
            sense: Sense::Addition,
            carry: Carry::Host,
            derivable: None,
            recipe: self.state.recipe,
        };
    }

    /// The flags of the sum in w6 of `left` and `right`, after every access.
    fn sum_flags(&mut self, left: Reg, right: Reg) {
        if self.out.any(FlagSet::NZV.or(FlagSet::CF)) {
            self.emit(cmn(W, left, right, Lsl, 0));
        }
        if self.out.any(FlagSet::PF.or(FlagSet::AF)) {
            self.emit_all(&[
                mov(W, X10, X6),
                eor(W, X11, left, right, Lsl, 0),
                eor(W, X11, X11, X6, Lsl, 0),
            ]);
        }
    }

    fn xadd(&mut self, dst: Src, src: u8) {
        let right = host(src);
        match dst {
            Src::Reg(index) => {
                let left = host(index);
                self.emit(add(W, X6, left, right, Lsl, 0));
                self.sum_flags(left, right);
                self.clobber(Operand::Guest(index));
                self.clobber(Operand::Guest(src));
                self.emit_all(&[mov(W, right, left), mov(W, left, X6)]);
            }
            Src::Mem(mem) => {
                let t = self.take_temp();
                let addr = self.address(mem);
                let left = temp(t);
                self.load(addr, left);
                self.forget(Operand::Temp(t));
                self.emit(add(W, X6, left, right, Lsl, 0));
                self.store(addr, X6);
                self.sum_flags(left, right);
                self.clobber(Operand::Guest(src));
                self.emit(mov(W, right, left));
            }
            Src::Imm(_) => unreachable!(),
        }
        self.state = State {
            nz: None,
            overflow: super::Overflow::Host,
            sense: Sense::Addition,
            carry: Carry::Host,
            derivable: None,
            recipe: Recipe::Canonical,
        };
    }

    fn xchg(&mut self, dst: Src, src: u8) {
        let right = host(src);
        match dst {
            Src::Reg(index) if index == src => {}
            Src::Reg(index) => {
                let left = host(index);
                self.clobber(Operand::Guest(index));
                self.clobber(Operand::Guest(src));
                self.emit_all(&[mov(W, X16, left), mov(W, left, right), mov(W, right, X16)]);
            }
            Src::Mem(mem) => {
                let addr = self.address(mem);
                self.load(addr, X6);
                self.store(addr, right);
                self.clobber(Operand::Guest(src));
                self.emit(mov(W, right, X6));
            }
            Src::Imm(_) => unreachable!(),
        }
    }

    /// CF and OF report a product that does not fit EAX; SF, ZF, AF and PF
    /// stay, as in the interpreter.
    fn multiply(&mut self, src: Src, signed: bool) {
        let value = self.source(src);
        self.clobber(Operand::Guest(EAX));
        self.clobber(Operand::Guest(EDX));
        let (eax, edx) = (host(EAX), host(EDX));
        self.emit(if signed {
            smull(X16, eax, value)
        } else {
            umull(X16, eax, value)
        });
        let flags = self.out.any(FlagSet::CF.or(FlagSet::OF));
        if flags {
            // What the low half cannot hold; bit 6 of its leading zeros,
            // flipped, is whether there is any.
            self.emit_all(&[
                if signed {
                    sub_ext(X, X17, X16, X16, Extend::Sxtw, 0)
                } else {
                    lsr_imm(X, X17, X16, 32)
                },
                clz(X, X17, X17),
                eor_imm(W, X17, X17, 64),
            ]);
        }
        self.emit_all(&[mov(W, eax, X16), lsr_imm(X, edx, X16, 32)]);
        if flags {
            self.insert_flags(Some((X17, 6)), Some((X17, 6)));
            self.state.overflow = super::Overflow::Host;
            self.state.sense = Sense::Addition;
            self.state.carry = Carry::Host;
            self.state.derivable = None;
        }
    }

    fn divide(&mut self, src: Src, signed: bool, helper: u8) {
        self.flush_simd();
        self.canonicalize();
        let value = self.source(src);
        let (eax, edx) = (host(EAX), host(EDX));
        let (slow, done) = (self.asm.label(), self.asm.label());
        self.asm.cbz(W, false, value, slow);
        // x16 is EDX:EAX; x17 the quotient and x15 the remainder. Only the
        // low halves of the guest registers are defined.
        self.emit_all(&[
            mov(W, X16, eax),
            mov(W, X15, edx),
            orr(X, X16, X16, X15, Lsl, 32),
        ]);
        if signed {
            self.emit_all(&[
                sbfx(X, X14, value, 0, 32),
                sdiv(X, X17, X16, X14),
                sub_ext(X, X15, X17, X17, Extend::Sxtw, 0),
            ]);
            self.asm.cbz(X, true, X15, slow);
            self.emit(msub(X, X15, X17, X14, X16));
        } else {
            self.emit_all(&[
                mov(W, X14, value),
                udiv(X, X17, X16, X14),
                lsr_imm(X, X15, X17, 32),
            ]);
            self.asm.cbz(X, true, X15, slow);
            self.emit(msub(X, X15, X17, X14, X16));
        }
        self.clobber(Operand::Guest(EAX));
        self.clobber(Operand::Guest(EDX));
        self.emit_all(&[mov(W, eax, X17), mov(W, edx, X15)]);
        self.asm.b(done);
        self.asm.bind(slow);
        self.helper(helper);
        self.asm.bind(done);
        self.state = State::CANONICAL;
    }

    /// SF, ZF, AF, PF and CF from AH, in canonical form; OF stays in V.
    fn sahf(&mut self) {
        let eax = host(EAX);
        self.emit_all(&[
            mrs_nzcv(X13),
            ubfx(W, X14, eax, 14, 2),
            bfi(X, X13, X14, 30, 2),
            ubfx(W, X14, eax, 8, 1),
            eor_imm(W, X14, X14, 1),
            bfi(X, X13, X14, 29, 1),
            msr_nzcv(X13),
            // AF is bit 4 of AH, and an odd low byte of x10 clears PF.
            ubfx(W, X11, eax, 8, 8),
            ubfx(W, X10, eax, 10, 1),
            eor_imm(W, X10, X10, 1),
        ]);
        self.state = State::CANONICAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Instruction {
        iced_x86::Decoder::with_ip(32, bytes, 0x1000, iced_x86::DecoderOptions::NONE).decode()
    }

    // spec: A64-TR-1
    #[test]
    fn plans_preserve_form_boundaries() {
        // rcl ebx, 1; lock xadd [ecx], edx; div dword [esi].
        assert_eq!(
            plan(&decode(&[0xd1, 0xd3]), 0),
            Some(Integer::Through(true, 3, 1))
        );
        assert!(matches!(
            plan(&decode(&[0xf0, 0x0f, 0xc1, 0x11]), 0),
            Some(Integer::Xadd(Src::Mem(_), 2))
        ));
        assert!(matches!(
            plan(&decode(&[0xf7, 0x36]), 5),
            Some(Integer::Divide(Src::Mem(_), false, 5))
        ));
        // Narrow CL shifts and byte divide still decline.
        assert_eq!(plan(&decode(&[0x66, 0xd3, 0xe0]), 0), None);
        assert_eq!(plan(&decode(&[0xf6, 0x36]), 0), None);
    }
}
