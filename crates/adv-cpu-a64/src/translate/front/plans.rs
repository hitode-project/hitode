//! Forms that lowerings can implement, selected from decoded instructions.
use super::decode::{Mem, Part, ShiftKind, Src, memory, memory_sized};
use crate::encode::{Float, Logic, Narrowing, Permute, Same, ShiftImm};
use crate::translate::{
    biased,
    integer::Integer,
    parts::{Count, Parts},
    simd::{Kind, Loc, Simd},
    strings::{Kind as StringKind, Rep, Strings},
    x87::X87,
};
use adv_cpu_core::floating::fast::{self, Fast};
use adv_cpu_core::integer::{self, Operand, Operation};
use iced_x86::{Code, Instruction, Mnemonic as M, Mnemonic, OpKind, Register};

/// The integer instruction `instruction` is, when the translator handles
/// it; `helper` runs it when a divide faults.
pub(in crate::translate) fn integer(instruction: &Instruction, helper: u8) -> Option<Integer> {
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
    if mnemonic == M::Lea && instruction.op0_register().is_gpr16() {
        let mut mem = memory_sized(instruction)?;
        mem.fs = false;
        return Some(Integer::Lea16(
            instruction.op0_register().number() as u8,
            mem,
        ));
    }
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
        return Some(Integer::CompareExchange8(memory_sized(instruction)?));
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
    if mnemonic == M::Mov
        && instruction.op1_kind() == OpKind::Register
        && instruction.op1_register().is_segment_register()
    {
        let width = if instruction.op0_kind() == OpKind::Memory {
            2
        } else {
            instruction.op0_register().size() as u8
        };
        return Some(Integer::ReadSegment(
            super::decode::part(instruction, 0, width)?,
            width,
            instruction.op1_register().number() as u8,
        ));
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
        M::Cmpxchg => Integer::Exchange(M::Cmpxchg, part(0)?, register(1)?, width),
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

/// The native plan for `instruction`, if one does.
pub(in crate::translate) fn parts(instruction: &Instruction) -> Option<Parts> {
    if matches!(instruction.mnemonic(), M::Shld | M::Shrd) {
        return double_shift(instruction);
    }
    // Byte registers numbered as `Narrow8or16::Byte` numbers them.
    let part = |operand: Operand, width: u8| match operand {
        Operand::Register(r) => Some((Part::Reg(r.index + r.shift / 2), r.width)),
        Operand::Memory(_, width) => Some((Part::Mem(memory_sized(instruction)?), width)),
        Operand::Immediate(value) => Some((Part::Imm(value), width)),
    };
    let (kind, dst, source, count) = match integer::decode(instruction)? {
        Operation::Shift(kind, dst, count) => {
            let count = match count {
                // A zero count changes nothing but still checks the write:
                // the interpreter keeps that.
                Operand::Immediate(count) => Count::Imm((count & 31) as u8),
                _ => Count::Cl,
            };
            (kind, dst, None, Some(count))
        }
        Operation::Binary(kind, dst, source) => (kind, dst, Some(source), None),
        _ => return None,
    };
    let (dst, width) = part(dst, 0)?;
    let source = match source {
        Some(source) => Some(part(source, width)?),
        None => None,
    };
    matches!(width, 1 | 2 | 4).then_some(Parts {
        kind,
        width,
        dst,
        source,
        count,
    })
}

/// SHLD or SHRD, which the shared integer decoder leaves out.
fn double_shift(instruction: &Instruction) -> Option<Parts> {
    let gpr = |register: Register| {
        (register.is_gpr16() || register.is_gpr32())
            .then(|| (register.number() as u8, register.size() as u8))
    };
    let (dst, width) = match instruction.op0_kind() {
        OpKind::Register => {
            let (number, width) = gpr(instruction.op0_register())?;
            (Part::Reg(number), width)
        }
        OpKind::Memory => (
            Part::Mem(memory_sized(instruction)?),
            instruction.memory_size().size() as u8,
        ),
        _ => return None,
    };
    let (source, size) = gpr(instruction.op1_register())?;
    if instruction.op1_kind() != OpKind::Register || size != width {
        return None;
    }
    let count = match instruction.op2_kind() {
        OpKind::Immediate8 => Count::Imm(instruction.immediate8() & 31),
        OpKind::Register if instruction.op2_register() == Register::CL => Count::Cl,
        _ => return None,
    };
    matches!(width, 2 | 4).then_some(Parts {
        kind: instruction.mnemonic(),
        width,
        dst,
        source: Some((Part::Reg(source), width)),
        count: Some(count),
    })
}

/// The string instruction `instruction` is, when the loop handles it.
pub(in crate::translate) fn strings(instruction: &Instruction, helper: u8) -> Option<Strings> {
    use Code::*;
    let (kind, width) = match instruction.code() {
        Movsb_m8_m8 => (StringKind::Movs, 1),
        Movsw_m16_m16 => (StringKind::Movs, 2),
        Movsd_m32_m32 => (StringKind::Movs, 4),
        Stosb_m8_AL => (StringKind::Stos, 1),
        Stosw_m16_AX => (StringKind::Stos, 2),
        Stosd_m32_EAX => (StringKind::Stos, 4),
        Lodsb_AL_m8 => (StringKind::Lods, 1),
        Lodsw_AX_m16 => (StringKind::Lods, 2),
        Lodsd_EAX_m32 => (StringKind::Lods, 4),
        Scasb_AL_m8 => (StringKind::Scas, 1),
        Scasw_AX_m16 => (StringKind::Scas, 2),
        Scasd_EAX_m32 => (StringKind::Scas, 4),
        Cmpsb_m8_m8 => (StringKind::Cmps, 1),
        Cmpsw_m16_m16 => (StringKind::Cmps, 2),
        Cmpsd_m32_m32 => (StringKind::Cmps, 4),
        _ => return None,
    };
    // ESI in its default segment, and 32-bit addresses.
    use iced_x86::OpKind::{MemoryESDI, MemorySegDI, MemorySegSI};
    let short = (0..instruction.op_count()).any(|n| {
        matches!(
            instruction.op_kind(n),
            MemorySegSI | MemorySegDI | MemoryESDI
        )
    });
    if short || instruction.has_lock_prefix() {
        return None;
    }
    let rep = match (instruction.has_rep_prefix(), instruction.has_repne_prefix()) {
        (false, false) => Rep::None,
        (true, false) => Rep::Rep,
        (false, true) => Rep::Repne,
        _ => return None,
    };
    Some(Strings {
        kind,
        width,
        rep,
        helper,
        fs: matches!(kind, StringKind::Movs | StringKind::Lods | StringKind::Cmps)
            && instruction.segment_prefix() == Register::FS,
    })
}

const NO_MEM: Mem = Mem {
    base: None,
    index: None,
    scale: 1,
    address_bits: 32,
    disp: 0,
    fs: false,
};

/// The `Simd` for `instruction`, whose interpreter helper is `helper`.
pub(in crate::translate) fn simd(instruction: &Instruction, helper: u8) -> Option<Simd> {
    use Mnemonic as M;
    if instruction.has_lock_prefix() {
        return None;
    }
    let loc = |n: u32| match instruction.op_kind(n) {
        OpKind::Register => {
            let register = instruction.op_register(n);
            let number = register.number() as u8;
            if register.is_mm() {
                Some(Loc::Mmx(number))
            } else if register.is_xmm() && number < 8 {
                Some(Loc::Xmm(number))
            } else if register.is_gpr32() {
                Some(Loc::Gpr(number))
            } else {
                None
            }
        }
        OpKind::Memory => Some(Loc::Mem),
        OpKind::Immediate8 => Some(Loc::Imm),
        _ => None,
    };
    if matches!(instruction.mnemonic(), M::Maskmovq | M::Maskmovdqu) {
        return Some(Simd {
            kind: Kind::MaskMove,
            dst: loc(1)?,
            src: loc(2)?,
            mem: Mem {
                base: Some(7),
                fs: instruction.segment_prefix() == iced_x86::Register::FS,
                address_bits: if instruction.op0_kind() == OpKind::MemorySegDI {
                    16
                } else {
                    32
                },
                ..NO_MEM
            },
            imm: 0,
            width: if instruction.mnemonic() == M::Maskmovq {
                8
            } else {
                16
            },
            aligned: false,
            helper,
        });
    }
    let mnemonic = instruction.mnemonic();
    let (dst, src) = match instruction.op_count() {
        0 => (Loc::Imm, Loc::Imm),
        1 if mnemonic == M::Stmxcsr => (loc(0)?, Loc::Imm),
        1 if mnemonic == M::Ldmxcsr => (Loc::Imm, loc(0)?),
        2 | 3 => (loc(0)?, loc(1)?),
        _ => return None,
    };
    let kind = match mnemonic {
        M::Emms | M::Femms => Kind::Emms,
        M::Movmskps | M::Movmskpd => Kind::SignMask(mnemonic == M::Movmskpd),
        M::Pshufhw | M::Pshuflw => Kind::ShuffleWords(mnemonic == M::Pshufhw),
        M::Shufpd => Kind::Shufpd,
        M::Pmuludq => Kind::MulDwords,
        M::Psadbw => Kind::SadBytes,
        M::Movlhps | M::Movhlps => Kind::MoveHalf(mnemonic == M::Movlhps),
        M::Cvttps2dq
        | M::Cvtps2dq
        | M::Cvtpd2dq
        | M::Cvttpd2dq
        | M::Cvtps2pi
        | M::Cvttps2pi
        | M::Cvtpd2pi
        | M::Cvttpd2pi => Kind::PackedToInt(
            matches!(
                mnemonic,
                M::Cvttps2dq | M::Cvttpd2dq | M::Cvttps2pi | M::Cvttpd2pi
            ),
            matches!(
                mnemonic,
                M::Cvtpd2dq | M::Cvttpd2dq | M::Cvtpd2pi | M::Cvttpd2pi
            ),
        ),
        M::Movq2dq | M::Movdq2q => Kind::Move,
        M::Movd
        | M::Movq
        | M::Movaps
        | M::Movups
        | M::Movapd
        | M::Movupd
        | M::Movdqa
        | M::Movdqu
        | M::Movntq
        | M::Movntps
        | M::Movntpd
        | M::Movntdq => Kind::Move,
        M::Movss | M::Movsd if matches!((dst, src), (Loc::Xmm(_), Loc::Xmm(_))) => {
            Kind::MoveLow(if mnemonic == M::Movsd { 3 } else { 2 })
        }
        M::Movss | M::Movsd => Kind::Move,
        M::Pextrw => Kind::ExtractWord,
        M::Pmovmskb => Kind::ByteMask,
        M::Pinsrw => Kind::InsertWord,
        M::Psrldq | M::Pslldq => Kind::ShiftBytes(mnemonic == M::Pslldq),
        M::Stmxcsr => Kind::StoreControl,
        M::Ldmxcsr => Kind::LoadControl,
        M::Cvtsi2ss | M::Cvtsi2sd => Kind::ScalarFromInt(mnemonic == M::Cvtsi2sd),
        M::Cvttss2si => Kind::ScalarToInt(false, true),
        M::Cvttsd2si => Kind::ScalarToInt(true, true),
        M::Cvtss2si => Kind::ScalarToInt(false, false),
        M::Cvtsd2si => Kind::ScalarToInt(true, false),
        M::Paddb => Kind::Same(Same::Add, 0),
        M::Paddw => Kind::Same(Same::Add, 1),
        M::Paddd => Kind::Same(Same::Add, 2),
        M::Paddq => Kind::Same(Same::Add, 3),
        M::Psubb => Kind::Same(Same::Sub, 0),
        M::Psubw => Kind::Same(Same::Sub, 1),
        M::Psubd => Kind::Same(Same::Sub, 2),
        M::Psubq => Kind::Same(Same::Sub, 3),
        M::Paddsb => Kind::Same(Same::Sqadd, 0),
        M::Paddsw => Kind::Same(Same::Sqadd, 1),
        M::Paddusb => Kind::Same(Same::Uqadd, 0),
        M::Paddusw => Kind::Same(Same::Uqadd, 1),
        M::Psubsb => Kind::Same(Same::Sqsub, 0),
        M::Psubsw => Kind::Same(Same::Sqsub, 1),
        M::Psubusb => Kind::Same(Same::Uqsub, 0),
        M::Psubusw => Kind::Same(Same::Uqsub, 1),
        M::Pcmpeqb => Kind::Same(Same::Cmeq, 0),
        M::Pcmpeqw => Kind::Same(Same::Cmeq, 1),
        M::Pcmpeqd => Kind::Same(Same::Cmeq, 2),
        M::Pcmpgtb => Kind::Same(Same::Cmgt, 0),
        M::Pcmpgtw => Kind::Same(Same::Cmgt, 1),
        M::Pcmpgtd => Kind::Same(Same::Cmgt, 2),
        M::Pmullw => Kind::Same(Same::Mul, 1),
        M::Pminub => Kind::Same(Same::Umin, 0),
        M::Pmaxub => Kind::Same(Same::Umax, 0),
        M::Pminsw => Kind::Same(Same::Smin, 1),
        M::Pmaxsw => Kind::Same(Same::Smax, 1),
        M::Pavgb => Kind::Same(Same::Urhadd, 0),
        M::Pavgw => Kind::Same(Same::Urhadd, 1),
        M::Pand | M::Andps | M::Andpd => Kind::Logic(Logic::And),
        M::Por | M::Orps | M::Orpd => Kind::Logic(Logic::Orr),
        M::Pxor | M::Xorps | M::Xorpd => Kind::Logic(Logic::Eor),
        M::Pandn | M::Andnps | M::Andnpd => Kind::AndNot,
        M::Punpcklbw => Kind::Unpack(Permute::Zip1, 0),
        M::Punpcklwd => Kind::Unpack(Permute::Zip1, 1),
        M::Punpckldq | M::Unpcklps => Kind::Unpack(Permute::Zip1, 2),
        M::Punpcklqdq | M::Unpcklpd => Kind::Unpack(Permute::Zip1, 3),
        M::Punpckhbw => Kind::Unpack(Permute::Zip2, 0),
        M::Punpckhwd => Kind::Unpack(Permute::Zip2, 1),
        M::Punpckhdq | M::Unpckhps => Kind::Unpack(Permute::Zip2, 2),
        M::Punpckhqdq | M::Unpckhpd => Kind::Unpack(Permute::Zip2, 3),
        M::Packsswb => Kind::Pack(Narrowing::Sqxtn, 0),
        M::Packssdw => Kind::Pack(Narrowing::Sqxtn, 1),
        M::Packuswb => Kind::Pack(Narrowing::Sqxtun, 0),
        M::Psllw | M::Pslld | M::Psllq | M::Psrlw | M::Psrld | M::Psrlq | M::Psraw | M::Psrad => {
            let (op, log2) = match mnemonic {
                M::Psllw => (ShiftImm::Shl, 1),
                M::Pslld => (ShiftImm::Shl, 2),
                M::Psllq => (ShiftImm::Shl, 3),
                M::Psrlw => (ShiftImm::Ushr, 1),
                M::Psrld => (ShiftImm::Ushr, 2),
                M::Psrlq => (ShiftImm::Ushr, 3),
                M::Psraw => (ShiftImm::Sshr, 1),
                _ => (ShiftImm::Sshr, 2),
            };
            if src == Loc::Imm {
                Kind::ShiftImm(op, log2)
            } else {
                Kind::ShiftBy(op, log2)
            }
        }
        M::Pmulhw => Kind::MulHigh(true),
        M::Pmulhuw => Kind::MulHigh(false),
        M::Pmaddwd => Kind::MulAdd,
        M::Pshufw => Kind::Shuffle(1),
        M::Pshufd => Kind::Shuffle(2),
        M::Shufps => Kind::Shufps,
        M::Addps => Kind::Float(Float::Fadd, false),
        M::Subps => Kind::Float(Float::Fsub, false),
        M::Mulps => Kind::Float(Float::Fmul, false),
        M::Divps => Kind::Float(Float::Fdiv, false),
        M::Addss => Kind::Float(Float::Fadd, true),
        M::Subss => Kind::Float(Float::Fsub, true),
        M::Mulss => Kind::Float(Float::Fmul, true),
        M::Divss => Kind::Float(Float::Fdiv, true),
        M::Addpd => Kind::Double(Float::Fadd, false),
        M::Subpd => Kind::Double(Float::Fsub, false),
        M::Mulpd => Kind::Double(Float::Fmul, false),
        M::Addsd => Kind::Double(Float::Fadd, true),
        M::Subsd => Kind::Double(Float::Fsub, true),
        M::Mulsd => Kind::Double(Float::Fmul, true),
        M::Divsd => Kind::Double(Float::Fdiv, true),
        M::Divpd => Kind::Double(Float::Fdiv, false),
        M::Sqrtss => Kind::Sqrt(true, false),
        M::Sqrtsd => Kind::Sqrt(true, true),
        M::Sqrtps => Kind::Sqrt(false, false),
        M::Sqrtpd => Kind::Sqrt(false, true),
        M::Maxpd => Kind::MinMax(true, false, true),
        M::Minpd => Kind::MinMax(false, false, true),
        M::Maxsd => Kind::MinMax(true, true, true),
        M::Minsd => Kind::MinMax(false, true, true),
        M::Cvtdq2ps | M::Cvtpi2ps => Kind::FromInt(false),
        M::Cvtdq2pd | M::Cvtpi2pd => Kind::FromInt(true),
        M::Cvtps2pd => Kind::Widen(false),
        M::Cvtss2sd => Kind::Widen(true),
        M::Cvtsd2ss => Kind::Narrow(true),
        M::Cvtpd2ps => Kind::Narrow(false),
        M::Rcpps | M::Rcpss | M::Rsqrtps | M::Rsqrtss => Kind::Approximate(
            matches!(mnemonic, M::Rsqrtps | M::Rsqrtss),
            matches!(mnemonic, M::Rcpss | M::Rsqrtss),
        ),
        M::Comiss | M::Ucomiss => Kind::FlagCompare(false, mnemonic == M::Comiss),
        M::Comisd | M::Ucomisd => Kind::FlagCompare(true, mnemonic == M::Comisd),
        M::Cmpps => Kind::Compare(false, false),
        M::Cmpss => Kind::Compare(true, false),
        M::Cmppd => Kind::Compare(false, true),
        M::Cmpsd => Kind::Compare(true, true),
        M::Maxps => Kind::MinMax(true, false, false),
        M::Minps => Kind::MinMax(false, false, false),
        M::Maxss => Kind::MinMax(true, true, false),
        M::Minss => Kind::MinMax(false, true, false),
        M::Movlps | M::Movlpd => Kind::Half(false),
        M::Movhps | M::Movhpd => Kind::Half(true),
        _ => return None,
    };
    // Registers of one class, except moves to and from the GPRs.
    let class = |loc: Loc| match loc {
        Loc::Mmx(_) => 1,
        Loc::Xmm(_) => 2,
        Loc::Gpr(_) => 3,
        Loc::Mem | Loc::Imm => 0,
    };
    let (d, s) = (class(dst), class(src));
    let valid = match kind {
        Kind::Emms => true,
        Kind::MaskMove => unreachable!("implicit memory plan"),
        Kind::SignMask(_) => d == 3 && s == 2,
        Kind::ShuffleWords(_) | Kind::Shufpd => d == 2 && (s == 2 || src == Loc::Mem),
        Kind::ExtractWord => (d == 3 || dst == Loc::Mem) && matches!(s, 1 | 2),
        Kind::ByteMask => d == 3 && matches!(s, 1 | 2),
        Kind::InsertWord => matches!(d, 1 | 2) && (s == 3 || src == Loc::Mem),
        Kind::StoreControl => dst == Loc::Mem,
        Kind::LoadControl => src == Loc::Mem,
        Kind::MoveHalf(_) => d == 2 && s == 2,
        Kind::PackedToInt(..) => matches!(d, 1 | 2) && (s == 2 || src == Loc::Mem),
        Kind::ShiftBytes(_) => d == 2 && src == Loc::Imm,
        Kind::ScalarFromInt(_) => d == 2 && (s == 3 || src == Loc::Mem),
        Kind::Approximate(..) => d == 2 && (s == 2 || src == Loc::Mem),
        Kind::FromInt(_) => d == 2 && matches!(s, 0..=2),
        Kind::ScalarToInt(..) => d == 3 && (s == 2 || src == Loc::Mem),
        Kind::Half(_) => d == 2 && src == Loc::Mem || dst == Loc::Mem && s == 2,
        Kind::Sqrt(..)
        | Kind::Float(..)
        | Kind::Double(..)
        | Kind::Widen(_)
        | Kind::Narrow(..)
        | Kind::FlagCompare(..)
        | Kind::Compare(..)
        | Kind::MinMax(..) => d == 2 && (s == 2 || src == Loc::Mem),
        Kind::Move if matches!(mnemonic, M::Movq2dq | M::Movdq2q) => {
            matches!((d, s), (1, 2) | (2, 1))
        }
        Kind::Move => match (d, s) {
            (1 | 2, 0) | (0, 1 | 2) | (1 | 2, 3) | (3, 1 | 2) => true,
            _ => d == s && matches!(d, 1 | 2),
        },
        _ => matches!(d, 1 | 2) && (s == d || src == Loc::Mem || src == Loc::Imm),
    };
    let three = matches!(
        kind,
        Kind::Shuffle(_)
            | Kind::ShuffleWords(_)
            | Kind::Shufpd
            | Kind::Shufps
            | Kind::Compare(..)
            | Kind::ExtractWord
            | Kind::InsertWord
    );
    let imm_ok = match kind {
        Kind::ShiftImm(..) | Kind::ShiftBytes(_) => instruction.op_count() == 2,
        _ if three => instruction.op_count() == 3 && instruction.op_kind(2) == OpKind::Immediate8,
        _ => src != Loc::Imm || matches!(kind, Kind::Emms | Kind::StoreControl),
    };
    if !valid || !imm_ok {
        return None;
    }
    let mem = if dst == Loc::Mem || src == Loc::Mem {
        if matches!(
            kind,
            Kind::InsertWord
                | Kind::StoreControl
                | Kind::LoadControl
                | Kind::PackedToInt(..)
                | Kind::FromInt(false)
                | Kind::ShuffleWords(_)
                | Kind::Shufpd
                | Kind::MulDwords
                | Kind::SadBytes
        ) {
            memory_sized(instruction)?
        } else {
            memory(instruction)?
        }
    } else {
        NO_MEM
    };
    let size = instruction.memory_size().size();
    let width = if dst == Loc::Mem || src == Loc::Mem {
        size
    } else {
        match mnemonic {
            M::Cvtps2pi | M::Cvttps2pi | M::Cvtpi2ps | M::Cvtpi2pd => 8,
            M::Movd => 4,
            M::Movq | M::Movsd | M::Movq2dq | M::Movdq2q => 8,
            _ => 16,
        }
    };
    let aligned = (dst == Loc::Mem || src == Loc::Mem)
        && size == 16
        && !matches!(mnemonic, M::Movups | M::Movupd | M::Movdqu);
    let imm = match kind {
        Kind::ShiftImm(..) | Kind::ShiftBytes(_) => instruction.immediate8(),
        _ if three => instruction.immediate8(),
        _ => 0,
    };
    Some(Simd {
        kind,
        dst,
        src,
        mem,
        imm,
        width: width as u8,
        aligned,
        helper,
    })
}

/// The `X87` for `instruction`, whose interpreter helper is `helper`.
pub(in crate::translate) fn x87(
    instruction: &Instruction,
    helper: u8,
    opcode: Option<u16>,
) -> Option<X87> {
    let fast = match instruction.mnemonic() {
        iced_x86::Mnemonic::Fld if instruction.memory_size().size() == 10 => Fast::Load80,
        iced_x86::Mnemonic::Fstp if instruction.memory_size().size() == 10 => Fast::Store80,
        m @ (iced_x86::Mnemonic::Fsin
        | iced_x86::Mnemonic::Fcos
        | iced_x86::Mnemonic::Fsincos
        | iced_x86::Mnemonic::Fptan
        | iced_x86::Mnemonic::Fpatan
        | iced_x86::Mnemonic::F2xm1
        | iced_x86::Mnemonic::Fyl2x
        | iced_x86::Mnemonic::Fyl2xp1
        | iced_x86::Mnemonic::Fscale
        | iced_x86::Mnemonic::Fprem
        | iced_x86::Mnemonic::Fprem1) => Fast::Leaf(m),
        _ => fast::classify(instruction)?,
    };
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
