//! Integer instructions outside the lazy flag forms, including partial
//! registers, bit strings, widening arithmetic and exchanges.
//!
//! A divide whose divisor is zero or whose quotient does not fit leaves
//! the work, and the #DE, to the `interpret` helper.
//! Registers change only after every access, so a fault leaves the
//! instruction undone, as in the interpreter.

use iced_x86::{Instruction, Mnemonic as M, OpKind};

use crate::encode::Shift::{Lsl, Lsr};
use crate::encode::Size::{W, X};
use crate::encode::*;

use super::decode::{Mem, Part, ShiftKind, Src, memory, memory_sized};
use super::flags::FlagSet;
use super::{
    Carry, Operand, Recipe, Sense, State, Translator, X6, X10, X11, X13, X14, X15, X16, X17,
    biased, host, temp,
};

// Partial-width forms canonicalize before borrowing the recipe temporaries.
const X7: Reg = Reg::new(7);
const X8: Reg = Reg::new(8);

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
    Bit(M, Part, Part, u8),
    Product(Part, Option<(Part, Option<u32>)>, bool, u8),
    Quotient(Part, bool, u8, u8),
    Exchange(M, Part, u8, u8),
    Cwd,
    MemoryNeg(Part, u8),
    Set(iced_x86::ConditionCode, Part),
    Cmov(iced_x86::ConditionCode, Part, Part, u8),
    Extend(Part, Part, u8, u8, bool),
    Complement(Part, u8),
    ClearWord(u8),
    Touch(Mem),
    Xlat(Mem),
    CarryFlag(M),
}

impl Integer {
    /// Flags read, flags written, and whether it has a fault site.
    pub fn effects(self) -> (FlagSet, FlagSet, bool) {
        use FlagSet as F;
        let mem = |src: Src| matches!(src, Src::Mem(_));
        match self {
            Integer::Bit(_, dst, _, _) => (F::NONE, F::CF, matches!(dst, Part::Mem(_))),
            Integer::Product(src, other, _, _) => (
                F::NONE,
                F::CF.or(F::OF),
                matches!(src, Part::Mem(_)) || matches!(other, Some((Part::Mem(_), _))),
            ),
            Integer::Quotient(..) => (F::NONE, F::NONE, true),
            Integer::Exchange(kind, dst, _, _) => (
                F::NONE,
                if kind == M::Xchg { F::NONE } else { F::ALL },
                matches!(dst, Part::Mem(_)),
            ),
            Integer::MemoryNeg(_, _) => (F::NONE, F::ALL, true),
            Integer::Set(cc, dst) => (F::of(cc), F::NONE, matches!(dst, Part::Mem(_))),
            Integer::Cmov(cc, _, src, _) => (F::of(cc), F::NONE, matches!(src, Part::Mem(_))),
            Integer::Extend(_, src, _, _, _) => (F::NONE, F::NONE, matches!(src, Part::Mem(_))),
            Integer::Cwd | Integer::ClearWord(_) => (F::NONE, F::NONE, false),
            Integer::Complement(dst, _) => (F::NONE, F::NONE, matches!(dst, Part::Mem(_))),
            Integer::Touch(_) | Integer::Xlat(_) => (F::NONE, F::NONE, true),
            Integer::CarryFlag(kind) => {
                (if kind == M::Cmc { F::CF } else { F::NONE }, F::CF, false)
            }
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
        && !(matches!(
            mnemonic,
            M::Xadd
                | M::Xchg
                | M::Cmpxchg
                | M::Cmpxchg8b
                | M::Bts
                | M::Btr
                | M::Btc
                | M::Not
                | M::Neg
        ) && instruction.op_kind(0) == OpKind::Memory)
    {
        return None;
    }
    if mnemonic == M::Cmpxchg8b && instruction.op0_kind() == OpKind::Memory {
        return Some(Integer::CompareExchange8(memory(instruction)?, helper));
    }
    let width = match instruction.op0_kind() {
        OpKind::Register => instruction.op0_register().size() as u8,
        OpKind::Memory => instruction.memory_size().size() as u8,
        _ => 0,
    };
    let part = |n| {
        if instruction.op_kind(n) == OpKind::Memory {
            memory_sized(instruction).map(Part::Mem)
        } else {
            super::decode::part(instruction, n, width)
        }
    };
    if mnemonic == M::Neg && instruction.op0_kind() == OpKind::Memory {
        return Some(Integer::MemoryNeg(part(0)?, width));
    }
    let cc = instruction.condition_code();
    if cc != iced_x86::ConditionCode::None {
        return if instruction.op_count() == 1 {
            Some(Integer::Set(cc, part(0)?))
        } else {
            Some(Integer::Cmov(cc, part(0)?, part(1)?, width))
        };
    }
    if matches!(mnemonic, M::Movsx | M::Movzx)
        && (width == 2
            || instruction.op1_kind() == OpKind::Memory
                && memory_sized(instruction).is_some_and(|m| m.address_bits == 16))
    {
        let source_width = if instruction.op1_kind() == OpKind::Memory {
            instruction.memory_size().size() as u8
        } else {
            instruction.op1_register().size() as u8
        };
        return Some(Integer::Extend(
            part(0)?,
            if instruction.op1_kind() == OpKind::Memory {
                Part::Mem(memory_sized(instruction)?)
            } else {
                super::decode::part(instruction, 1, source_width)?
            },
            source_width,
            width,
            mnemonic == M::Movsx,
        ));
    }
    if mnemonic == M::Not && (width != 4 || instruction.op0_kind() == OpKind::Memory) {
        return Some(Integer::Complement(part(0)?, width));
    }
    if mnemonic == M::Bswap && width == 2 {
        let Part::Reg(index) = part(0)? else {
            return None;
        };
        return Some(Integer::ClearWord(index));
    }
    if mnemonic == M::Clflush {
        return Some(Integer::Touch(memory_sized(instruction)?));
    }
    if mnemonic == M::Xlatb {
        let mut address = *instruction;
        address.set_memory_index(iced_x86::Register::None);
        return Some(Integer::Xlat(memory_sized(&address)?));
    }
    if matches!(mnemonic, M::Bt | M::Bts | M::Btr | M::Btc)
        && matches!(width, 2 | 4)
        && !(mnemonic == M::Bt
            && width == 4
            && instruction.op1_kind() == OpKind::Immediate8
            && operand(0).is_some())
    {
        return Some(Integer::Bit(mnemonic, part(0)?, part(1)?, width));
    }
    if matches!(width, 1 | 2)
        || width == 4
            && (operand(0).is_none()
                || matches!(mnemonic, M::Xchg | M::Xadd | M::Cmpxchg) && register(1).is_none()
                || mnemonic == M::Imul
                    && instruction.op_count() > 1
                    && instruction.op1_kind() == OpKind::Memory
                    && memory_sized(instruction).is_some_and(|m| m.address_bits == 16))
    {
        if matches!(mnemonic, M::Mul | M::Imul) {
            let other = if instruction.op_count() == 1 {
                None
            } else {
                Some((
                    part(1)?,
                    (instruction.op_count() == 3).then(|| instruction.immediate(2) as u32),
                ))
            };
            return Some(Integer::Product(
                part(0)?,
                other,
                mnemonic == M::Imul,
                width,
            ));
        }
        if matches!(mnemonic, M::Div | M::Idiv) {
            return Some(Integer::Quotient(
                part(0)?,
                mnemonic == M::Idiv,
                width,
                helper,
            ));
        }
        if matches!(mnemonic, M::Xchg | M::Xadd | M::Cmpxchg) {
            let Part::Reg(src) = part(1)? else {
                return None;
            };
            return Some(Integer::Exchange(mnemonic, part(0)?, src, width));
        }
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
        M::Cwd => Integer::Cwd,
        M::Clc | M::Stc | M::Cmc => Integer::CarryFlag(mnemonic),
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
        M::Bswap => Integer::Bswap(instruction.op0_register().number() as u8),
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
            Integer::Bit(kind, dst, index, width) => self.bit(kind, dst, index, width),
            Integer::Product(dst, other, signed, width) => self.product(dst, other, signed, width),
            Integer::Quotient(src, signed, width, helper) => {
                self.quotient(src, signed, width, helper)
            }
            Integer::Exchange(kind, dst, src, width) => self.exchange(kind, dst, src, width),
            Integer::MemoryNeg(dst, width) => {
                self.canonicalize();
                let address = self.part_address(dst);
                self.narrow_read(dst, width, address, X13);
                self.emit(sub(W, X6, Reg::ZR, X13, Lsl, 0));
                self.narrow_write(dst, width, address, X6);
                self.emit_all(&[
                    mov(W, X10, X6),
                    eor(W, X11, X13, X6, Lsl, 0),
                    lsl_imm(W, X13, X13, 32 - 8 * u32::from(width)),
                    subs(W, Reg::ZR, Reg::ZR, X13, Lsl, 0),
                ]);
                self.state = State::CANONICAL;
            }
            Integer::Set(cc, dst) => {
                self.canonicalize();
                self.set_value(cc);
                let address = self.part_address(dst);
                self.narrow_write(dst, 1, address, X6);
            }
            Integer::Cmov(cc, dst, src, width) => {
                self.canonicalize();
                let address = self.part_address(src);
                self.narrow_read(dst, width, Reg::ZR, X7);
                self.narrow_read(src, width, address, X8);
                if matches!(cc, iced_x86::ConditionCode::p | iced_x86::ConditionCode::np) {
                    self.set_value(cc);
                    let done = self.asm.label();
                    self.asm.cbz(W, false, X6, done);
                    self.narrow_write(dst, width, Reg::ZR, X8);
                    self.asm.bind(done);
                } else {
                    let cond = self.condition(cc);
                    self.emit(csel(W, X6, X8, X7, cond));
                    self.narrow_write(dst, width, Reg::ZR, X6);
                }
            }
            Integer::Extend(dst, src, width, destination_width, signed) => {
                let address = self.part_address(src);
                self.narrow_read(src, width, address, X6);
                if signed {
                    self.emit(sbfx(W, X6, X6, 0, 8 * u32::from(width)));
                }
                self.narrow_write(dst, destination_width, Reg::ZR, X6);
            }
            Integer::Complement(dst, width) => {
                self.canonicalize();
                let address = self.part_address(dst);
                self.narrow_read(dst, width, address, X6);
                self.emit(mvn(W, X6, X6));
                self.narrow_write(dst, width, address, X6);
            }
            Integer::ClearWord(dst) => {
                self.emit(movz(W, X6, 0, 0));
                self.narrow_write(Part::Reg(dst), 2, Reg::ZR, X6);
            }
            Integer::Xlat(mem) => {
                let address = self.flat(mem);
                self.emit_all(&[
                    and_imm(W, X13, host(EAX), 255),
                    add(W, X16, address, X13, Lsl, 0),
                ]);
                let address = self.finish_address(mem, X16);
                self.access(
                    super::access::Addr::Window(address),
                    1,
                    super::access::Dir::Load,
                    X6,
                );
                self.narrow_write(Part::Reg(EAX), 1, Reg::ZR, X6);
            }
            Integer::Touch(mem) => {
                let address = self.effective(mem);
                self.access(
                    super::access::Addr::Window(address),
                    1,
                    super::access::Dir::Load,
                    X6,
                );
            }
            Integer::Cwd => {
                self.canonicalize();
                self.emit(sbfx(W, X6, host(EAX), 15, 1));
                self.narrow_write(Part::Reg(EDX), 2, Reg::ZR, X6);
            }
            Integer::CarryFlag(kind) => {
                self.canonicalize();
                self.emit(mrs_nzcv(X13));
                self.emit(match kind {
                    M::Clc => orr_imm(X, X13, X13, 1 << 29),
                    M::Stc => and_imm(X, X13, X13, !(1 << 29)),
                    _ => eor_imm(X, X13, X13, 1 << 29),
                });
                self.emit(msr_nzcv(X13));
            }
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
                if biased(index) {
                    self.emit(rev(W, super::ops::result(index), host(index)));
                    self.commit(index);
                } else {
                    self.emit(rev(W, host(index), host(index)));
                }
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

    fn set_value(&mut self, cc: iced_x86::ConditionCode) {
        if matches!(cc, iced_x86::ConditionCode::p | iced_x86::ConditionCode::np) {
            self.emit_all(&[
                eor(W, X6, X10, X10, Lsr, 4),
                eor(W, X6, X6, X6, Lsr, 2),
                eor(W, X6, X6, X6, Lsr, 1),
                ubfx(W, X6, X6, 0, 1),
            ]);
            if cc == iced_x86::ConditionCode::p {
                self.emit(eor_imm(W, X6, X6, 1));
            }
        } else {
            let cond = self.condition(cc);
            self.emit(cset(W, X6, cond));
        }
    }

    fn part_address(&mut self, part: Part) -> Reg {
        match part {
            Part::Mem(mem) => self.effective(mem),
            _ => Reg::ZR,
        }
    }

    fn bit(&mut self, kind: M, dst: Part, index: Part, width: u8) {
        self.canonicalize();
        let bits = 8 * u32::from(width);
        self.narrow_read(index, width, Reg::ZR, X8);
        let address = self.part_address(dst);
        if matches!(dst, Part::Mem(_)) && matches!(index, Part::Reg(_)) {
            self.emit_all(&[
                sbfx(W, X14, X8, 0, bits),
                asr_imm(W, X14, X14, bits.trailing_zeros()),
                add(W, X16, address, X14, Lsl, u32::from(width).trailing_zeros()),
            ]);
        } else if address != Reg::ZR {
            self.emit(mov(W, X16, address));
        }
        self.narrow_read(dst, width, X16, X7);
        self.emit_all(&[
            and_imm(W, X8, X8, u64::from(bits - 1)),
            shiftv(W, Lsr, X14, X7, X8),
        ]);
        if kind != M::Bt {
            self.emit_all(&[movz(W, X6, 1, 0), shiftv(W, Lsl, X6, X6, X8)]);
            self.emit(match kind {
                M::Bts => orr(W, X6, X7, X6, Lsl, 0),
                M::Btr => bic(W, X6, X7, X6, Lsl, 0),
                _ => eor(W, X6, X7, X6, Lsl, 0),
            });
            self.narrow_write(dst, width, X16, X6);
        }
        self.insert_flags(Some((X14, 0)), None);
        self.state.sense = Sense::Addition;
        self.state.carry = Carry::Host;
        self.state.derivable = None;
    }

    fn product(&mut self, dst: Part, other: Option<(Part, Option<u32>)>, signed: bool, width: u8) {
        self.canonicalize();
        let source = other.map_or(dst, |(src, _)| src);
        let address = self.part_address(source);
        self.narrow_read(source, width, address, X8);
        match other {
            Some((_, Some(imm))) => self.mov_imm(X7, imm),
            Some((_, None)) => self.narrow_read(dst, width, Reg::ZR, X7),
            None => self.narrow_read(Part::Reg(EAX), width, Reg::ZR, X7),
        }
        let bits = 8 * u32::from(width);
        if signed {
            self.emit_all(&[sbfx(W, X7, X7, 0, bits), sbfx(W, X8, X8, 0, bits)]);
        }
        self.emit(if signed {
            smull(X6, X7, X8)
        } else {
            umull(X6, X7, X8)
        });
        self.emit(if signed {
            sbfx(X, X14, X6, 0, bits)
        } else {
            ubfx(X, X14, X6, 0, bits)
        });
        self.emit_all(&[
            eor(X, X14, X14, X6, Lsl, 0),
            clz(X, X14, X14),
            eor_imm(W, X14, X14, 64),
            mov(W, X8, X14),
        ]);
        if other.is_some() {
            self.narrow_write(dst, width, Reg::ZR, X6);
        } else if width == 1 {
            self.narrow_write(Part::Reg(EAX), 2, Reg::ZR, X6);
        } else {
            self.narrow_write(Part::Reg(EAX), width, Reg::ZR, X6);
            self.emit(lsr_imm(X, X6, X6, bits));
            self.narrow_write(Part::Reg(EDX), width, Reg::ZR, X6);
        }
        self.insert_flags(Some((X8, 6)), Some((X8, 6)));
        self.state.overflow = super::Overflow::Host;
        self.state.sense = Sense::Addition;
        self.state.carry = Carry::Host;
        self.state.derivable = None;
    }

    fn quotient(&mut self, src: Part, signed: bool, width: u8, helper: u8) {
        self.flush_simd();
        self.canonicalize();
        let address = self.part_address(src);
        self.narrow_read(src, width, address, X8);
        let (slow, done) = (self.asm.label(), self.asm.label());
        self.asm.cbz(W, false, X8, slow);
        let bits = 8 * u32::from(width);
        if width == 1 {
            self.emit(ubfx(W, X7, host(EAX), 0, 16));
        } else {
            self.emit_all(&[
                ubfx(W, X7, host(EAX), 0, bits),
                ubfx(W, X14, host(EDX), 0, bits),
                orr(X, X7, X7, X14, Lsl, bits),
            ]);
        }
        if signed {
            self.emit_all(&[
                sbfx(X, X7, X7, 0, bits * 2),
                sbfx(X, X8, X8, 0, bits),
                sdiv(X, X6, X7, X8),
                sbfx(X, X14, X6, 0, bits),
            ]);
        } else {
            self.emit_all(&[udiv(X, X6, X7, X8), ubfx(X, X14, X6, 0, bits)]);
        }
        self.emit(eor(X, X14, X14, X6, Lsl, 0));
        self.asm.cbz(X, true, X14, slow);
        self.emit(msub(X, X7, X6, X8, X7));
        self.narrow_write(Part::Reg(EAX), width, Reg::ZR, X6);
        self.narrow_write(
            Part::Reg(if width == 1 { 4 } else { EDX }),
            width,
            Reg::ZR,
            X7,
        );
        self.asm.b(done);
        self.asm.bind(slow);
        self.helper(helper);
        self.asm.bind(done);
        self.state = State::CANONICAL;
    }

    fn exchange(&mut self, kind: M, dst: Part, src: u8, width: u8) {
        self.canonicalize();
        let address = self.part_address(dst);
        self.narrow_read(dst, width, address, X7);
        self.narrow_read(Part::Reg(src), width, Reg::ZR, X8);
        if kind == M::Cmpxchg {
            self.narrow_read(Part::Reg(EAX), width, Reg::ZR, X14);
            self.emit(eor(W, X6, X14, X7, Lsl, 0));
            let (unequal, flags) = (self.asm.label(), self.asm.label());
            self.asm.cbz(W, true, X6, unequal);
            self.narrow_write(dst, width, address, X8);
            self.asm.b(flags);
            self.asm.bind(unequal);
            if matches!(dst, Part::Mem(_)) {
                self.narrow_write(dst, width, address, X7);
            }
            self.narrow_write(Part::Reg(EAX), width, Reg::ZR, X7);
            self.asm.bind(flags);
            self.emit_all(&[mov(W, X8, X7), mov(W, X7, X14), sub(W, X6, X7, X8, Lsl, 0)]);
        } else {
            self.emit(if kind == M::Xadd {
                add(W, X6, X7, X8, Lsl, 0)
            } else {
                mov(W, X6, X8)
            });
            // Register aliases follow the interpreter's source-then-destination order.
            if matches!(dst, Part::Mem(_)) {
                self.narrow_write(dst, width, address, X6);
                self.narrow_write(Part::Reg(src), width, Reg::ZR, X7);
            } else {
                self.narrow_write(Part::Reg(src), width, Reg::ZR, X7);
                self.narrow_write(dst, width, Reg::ZR, X6);
            }
            if kind == M::Xchg {
                return;
            }
        }
        let shift = 32 - 8 * u32::from(width);
        self.emit_all(&[
            mov(W, X10, X6),
            eor(W, X11, X7, X8, Lsl, 0),
            eor(W, X11, X11, X6, Lsl, 0),
            lsl_imm(W, X13, X7, shift),
        ]);
        self.emit(if kind == M::Xadd {
            adds(W, Reg::ZR, X13, X8, Lsl, shift)
        } else {
            subs(W, Reg::ZR, X13, X8, Lsl, shift)
        });
        self.state = State {
            sense: if kind == M::Xadd {
                Sense::Addition
            } else {
                Sense::Subtraction
            },
            ..State::CANONICAL
        };
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
                self.access_or(X17, 4, false, load, slow);
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
                self.access_or(X17, 4, true, store, slow);
            }
            Src::Imm(_) => unreachable!(),
        }
        self.asm.b(flags);
        self.asm.bind(unequal);
        if matches!(dst, Src::Mem(_)) {
            // A mismatch still writes: read-only destinations must fault before
            // changing either EAX or the comparison flags.
            let store = str_reg(W, X6, super::WINDOW, X17, false);
            self.access_or(X17, 4, true, store, slow);
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
        self.canonicalize();
        let address = self.effective(mem);
        self.emit(mov(W, X17, address));
        let slow = self.asm.label();
        self.emit(and_imm(W, X13, X17, 7));
        self.asm.cbz(W, true, X13, slow);
        self.access_or(
            X17,
            8,
            false,
            ldr_reg(X, X6, super::WINDOW, X17, false),
            slow,
        );
        self.emit_all(&[
            orr(X, X13, host(EAX), host(EDX), Lsl, 32),
            eor(X, X13, X13, X6, Lsl, 0),
        ]);
        let unequal = self.asm.label();
        let done = self.asm.label();
        self.asm.cbz(X, true, X13, unequal);
        self.emit(orr(X, X15, host(3), host(ECX), Lsl, 32));
        self.access_or(
            X17,
            8,
            true,
            str_reg(X, X15, super::WINDOW, X17, false),
            slow,
        );
        self.emit_all(&[mrs_nzcv(X14), orr_imm(X, X14, X14, 1 << 30), msr_nzcv(X14)]);
        self.asm.b(done);
        self.asm.bind(unequal);
        self.access_or(
            X17,
            8,
            true,
            str_reg(X, X6, super::WINDOW, X17, false),
            slow,
        );
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
        // Narrow CL shifts still decline.
        assert_eq!(plan(&decode(&[0x66, 0xd3, 0xe0]), 0), None);
    }
}
