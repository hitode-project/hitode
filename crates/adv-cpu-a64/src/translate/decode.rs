//! Guest instructions the translator handles.
//!
//! `quick` decodes the forms compilers emit most straight to an `Op`, in a
//! few dozen cycles; anything else, prefixes included, goes through iced-x86
//! and `plan`. Whatever `quick` accepts, iced and `plan` decode to the same
//! `Op` and length (`quick_agrees_with_iced`).

use adv_cpu_core::interpreter::Prepared;
use adv_cpu_core::memory::Memory;
use iced_x86::{
    Code, ConditionCode, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind,
    Register,
};

use super::flags::FlagSet;

pub const ESP: u8 = 4;
pub const EBP: u8 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mem {
    pub base: Option<u8>,
    pub index: Option<u8>,
    pub scale: u32,
    pub disp: u32,
    /// Relative to the FS base rather than flat.
    pub fs: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    Reg(u8),
    Imm(u32),
    Mem(Mem),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    Add,
    Sub,
    And,
    Or,
    Xor,
    Cmp,
    Test,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShiftKind {
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
}

/// An operand of an 8- or 16-bit instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// A byte register as `Narrow8or16::Byte` numbers it, or a word
    /// register by guest index.
    Reg(u8),
    Imm(u32),
    Mem(Mem),
}

/// A narrow source of `movzx`/`movsx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Narrow8or16 {
    /// A byte register: AL..BL are 0..3, AH..BH 4..7.
    Byte(u8),
    /// A word register by guest index.
    Word(u8),
    Mem(Mem, bool),
}

/// What a conditional branch tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cc {
    Flags(ConditionCode),
    /// `jecxz`.
    EcxZero,
    /// `jcxz`.
    CxZero,
    /// `loop`, `loope` (`Some(true)`) or `loopne`: ECX counts down first.
    Loop(Option<bool>),
}

/// One guest instruction the translator handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Nop,
    Mov(Src, Src),
    Lea(u8, Mem),
    Alu(Alu, Src, Src),
    Step(bool, u8),
    Neg(u8),
    Not(u8),
    Extend(u8, Narrow8or16, bool),
    Cdq,
    Push(Src),
    Pop(u8),
    Leave,
    Jmp(u32),
    Call(u32),
    Jcc(Cc, u32),
    Ret(u16),
    JmpIndirect(Src),
    CallIndirect(Src),
    Setcc(ConditionCode, u8),
    Cmov(ConditionCode, u8, Src),
    /// A register shifted by a count of 1 to 31.
    Shift(ShiftKind, u8, u32),
    /// `imul dst, source` or `imul dst, source, imm`.
    Imul(u8, Src, Option<u32>),
    /// `adc` (false) or `sbb` (true) into a register.
    Carry(bool, u8, Src),
    /// `mov` (`None`) or an ALU operation of 1 or 2 bytes.
    Narrow(Option<Alu>, u8, Part, Part),
    /// `inc` (true) or `dec` of a 32-bit memory operand, which keep CF.
    StepMemory(bool, Mem),
    /// `inc` or `dec` of a byte or word, preserving CF.
    NarrowStep(bool, u8, Part),
    /// `cld` (false) or `std`.
    Direction(bool),
    /// An MMX or SSE instruction.
    Simd(super::simd::Simd),
    /// A string instruction, perhaps repeated.
    Strings(super::strings::Strings),
    Integer(super::integer::Integer),
    /// An x87 instruction `fast::classify` accepts.
    X87(super::x87::X87),
    /// Anything else the interpreter runs without transferring control:
    /// the `interpret` stub runs it, by its index in the trace's helpers.
    Helper(u8),
}

fn gpr32(register: Register) -> Option<u8> {
    register.is_gpr32().then(|| register.number() as u8)
}

pub(super) fn memory(instruction: &Instruction) -> Option<Mem> {
    // CS, DS, SS and ES are flat; FS has the thread's base, and GS is left
    // to the interpreter.
    let fs = match instruction.segment_prefix() {
        Register::FS => true,
        Register::GS => return None,
        _ => false,
    };
    let register = |register| match register {
        Register::None => Some(None),
        register => gpr32(register).map(Some),
    };
    Some(Mem {
        base: register(instruction.memory_base())?,
        index: register(instruction.memory_index())?,
        scale: instruction.memory_index_scale(),
        disp: instruction.memory_displacement32(),
        fs,
    })
}

/// The width of an instruction's first operand, if it is 8 or 16 bits.
fn narrow_width(instruction: &Instruction) -> Option<u8> {
    let size = match instruction.op_kind(0) {
        OpKind::Register => instruction.op_register(0).size(),
        OpKind::Memory => instruction.memory_size().size(),
        _ => return None,
    };
    matches!(size, 1 | 2).then_some(size as u8)
}

/// Operand `n` of an 8- or 16-bit instruction of `width` bytes.
fn part(instruction: &Instruction, n: u32, width: u8) -> Option<Part> {
    let mask = if width == 1 { 0xff } else { 0xffff };
    match instruction.op_kind(n) {
        OpKind::Register => {
            let register = instruction.op_register(n);
            let fits = if width == 1 {
                register.is_gpr8()
            } else {
                register.is_gpr16()
            };
            fits.then(|| Part::Reg(register.number() as u8))
        }
        OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate8to16 => {
            Some(Part::Imm(instruction.immediate(n) as u32 & mask))
        }
        OpKind::Memory => memory(instruction).map(Part::Mem),
        _ => None,
    }
}

fn operand(instruction: &Instruction, n: u32) -> Option<Src> {
    match instruction.op_kind(n) {
        OpKind::Register => gpr32(instruction.op_register(n)).map(Src::Reg),
        OpKind::Immediate32 | OpKind::Immediate8to32 => {
            Some(Src::Imm(instruction.immediate(n) as u32))
        }
        OpKind::Memory if instruction.memory_size().size() == 4 => {
            memory(instruction).map(Src::Mem)
        }
        _ => None,
    }
}

pub fn plan(instruction: &Instruction) -> Option<Op> {
    use Mnemonic as M;
    // Guest threads take turns on one host thread, so a locked
    // read-modify-write is an ordinary one; other lock uses are #UD.
    if instruction.has_lock_prefix()
        && !(instruction.op_kind(0) == OpKind::Memory
            && matches!(
                instruction.mnemonic(),
                M::Add | M::Sub | M::And | M::Or | M::Xor | M::Inc | M::Dec
            ))
    {
        return None;
    }
    let count = instruction.op_count();
    let two = || Some((operand(instruction, 0)?, operand(instruction, 1)?));
    let register = |n| match instruction.op_kind(n) {
        OpKind::Register => gpr32(instruction.op_register(n)),
        _ => None,
    };
    let near = || match instruction.op_kind(0) {
        OpKind::NearBranch32 => Some(instruction.near_branch32()),
        _ => None,
    };
    let alu = |alu| {
        let (dst, src) = two()?;
        match (dst, src) {
            (Src::Imm(_), _) | (Src::Mem(_), Src::Mem(_)) => None,
            _ => Some(Op::Alu(alu, dst, src)),
        }
    };
    let mnemonic = instruction.mnemonic();
    if instruction.is_jcc_short_or_near() {
        let cc = Cc::Flags(instruction.condition_code());
        return Some(Op::Jcc(cc, near()?)).filter(|op| supported(*op));
    }
    let counted = match instruction.code() {
        Code::Jecxz_rel8_32 => Some(Cc::EcxZero),
        Code::Jcxz_rel8_32 => Some(Cc::CxZero),
        Code::Loop_rel8_32_ECX => Some(Cc::Loop(None)),
        Code::Loope_rel8_32_ECX => Some(Cc::Loop(Some(true))),
        Code::Loopne_rel8_32_ECX => Some(Cc::Loop(Some(false))),
        _ => None,
    };
    if let Some(cc) = counted {
        return Some(Op::Jcc(cc, near()?));
    }
    let narrow = match mnemonic {
        M::Mov => Some(None),
        M::Add => Some(Some(Alu::Add)),
        M::Sub => Some(Some(Alu::Sub)),
        M::And => Some(Some(Alu::And)),
        M::Or => Some(Some(Alu::Or)),
        M::Xor => Some(Some(Alu::Xor)),
        M::Cmp => Some(Some(Alu::Cmp)),
        M::Test => Some(Some(Alu::Test)),
        _ => None,
    };
    if let (Some(alu), Some(width), 2) = (narrow, narrow_width(instruction), count) {
        return match (part(instruction, 0, width)?, part(instruction, 1, width)?) {
            (Part::Imm(_), _) | (Part::Mem(_), Part::Mem(_)) => None,
            (dst, src) => Some(Op::Narrow(alu, width, dst, src)),
        };
    }
    if matches!(mnemonic, M::Inc | M::Dec)
        && let Some(width) = narrow_width(instruction)
    {
        return Some(Op::NarrowStep(
            mnemonic == M::Inc,
            width,
            part(instruction, 0, width)?,
        ));
    }
    Some(match mnemonic {
        M::Nop => Op::Nop,
        // Hints that never fault; ENDBR32 marks indirect branch targets for
        // CET, which the guest never enables.
        M::Endbr32 => Op::Nop,
        M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 | M::Prefetchnta => Op::Nop,
        M::Cld => Op::Direction(false),
        M::Std => Op::Direction(true),
        M::Mov => match two()? {
            (Src::Imm(_), _) | (Src::Mem(_), Src::Mem(_)) => return None,
            (dst, src) => Op::Mov(dst, src),
        },
        // An address, with no segment base.
        M::Lea => Op::Lea(
            register(0)?,
            Mem {
                fs: false,
                ..memory(instruction)?
            },
        ),
        M::Add => alu(Alu::Add)?,
        M::Sub => alu(Alu::Sub)?,
        M::And => alu(Alu::And)?,
        M::Or => alu(Alu::Or)?,
        M::Xor => alu(Alu::Xor)?,
        M::Cmp => alu(Alu::Cmp)?,
        M::Test => alu(Alu::Test)?,
        M::Inc | M::Dec => match operand(instruction, 0)? {
            Src::Reg(index) => Op::Step(mnemonic == M::Inc, index),
            Src::Mem(mem) => Op::StepMemory(mnemonic == M::Inc, mem),
            Src::Imm(_) => return None,
        },
        M::Neg => Op::Neg(register(0)?),
        M::Not => Op::Not(register(0)?),
        M::Movzx | M::Movsx => {
            let signed = mnemonic == M::Movsx;
            let source = match instruction.op_kind(1) {
                OpKind::Register => {
                    let source = instruction.op_register(1);
                    if source.is_gpr8() {
                        Narrow8or16::Byte(source.number() as u8)
                    } else if source.is_gpr16() {
                        Narrow8or16::Word(source.number() as u8)
                    } else {
                        return None;
                    }
                }
                OpKind::Memory => match instruction.memory_size().size() {
                    1 => Narrow8or16::Mem(memory(instruction)?, false),
                    2 => Narrow8or16::Mem(memory(instruction)?, true),
                    _ => return None,
                },
                _ => return None,
            };
            Op::Extend(register(0)?, source, signed)
        }
        M::Cwde => Op::Extend(0, Narrow8or16::Word(0), true),
        M::Cdq => Op::Cdq,
        M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror => {
            let dst = register(0)?;
            if instruction.op_kind(1) != OpKind::Immediate8 {
                return None;
            }
            let kind = match mnemonic {
                M::Shl | M::Sal => ShiftKind::Shl,
                M::Shr => ShiftKind::Shr,
                M::Sar => ShiftKind::Sar,
                M::Rol => ShiftKind::Rol,
                _ => ShiftKind::Ror,
            };
            // A zero count changes nothing, flags included.
            match u32::from(instruction.immediate8()) & 31 {
                0 => Op::Nop,
                count => Op::Shift(kind, dst, count),
            }
        }
        M::Imul => {
            let dst = register(0)?;
            let source = match operand(instruction, 1)? {
                Src::Imm(_) => return None,
                source => source,
            };
            match count {
                2 => Op::Imul(dst, source, None),
                3 => Op::Imul(dst, source, Some(instruction.immediate(2) as u32)),
                _ => return None,
            }
        }
        M::Adc | M::Sbb => match two()? {
            (Src::Reg(dst), source) => Op::Carry(mnemonic == M::Sbb, dst, source),
            _ => return None,
        },
        M::Leave if instruction.code() == iced_x86::Code::Leaved => Op::Leave,
        M::Push if count == 1 => Op::Push(operand(instruction, 0)?),
        M::Pop => match register(0)? {
            ESP => return None,
            register => Op::Pop(register),
        },
        M::Jmp => match instruction.op_kind(0) {
            OpKind::NearBranch32 => Op::Jmp(instruction.near_branch32()),
            OpKind::Register | OpKind::Memory => Op::JmpIndirect(operand(instruction, 0)?),
            _ => return None,
        },
        M::Call => match instruction.op_kind(0) {
            OpKind::NearBranch32 => Op::Call(instruction.near_branch32()),
            OpKind::Register | OpKind::Memory => Op::CallIndirect(operand(instruction, 0)?),
            _ => return None,
        },
        M::Ret => match instruction.code() {
            iced_x86::Code::Retnd => Op::Ret(0),
            iced_x86::Code::Retnd_imm16 if instruction.immediate16() < 4096 => {
                Op::Ret(instruction.immediate16())
            }
            _ => return None,
        },
        M::Seto
        | M::Setno
        | M::Setb
        | M::Setae
        | M::Sete
        | M::Setne
        | M::Setbe
        | M::Seta
        | M::Sets
        | M::Setns
        | M::Setp
        | M::Setnp
        | M::Setl
        | M::Setge
        | M::Setle
        | M::Setg => {
            let target = instruction.op_register(0);
            if instruction.op_kind(0) != OpKind::Register || !target.is_gpr8() {
                return None;
            }
            Op::Setcc(instruction.condition_code(), target.number() as u8)
        }
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
        | M::Cmovg => {
            let source = operand(instruction, 1)?;
            Op::Cmov(instruction.condition_code(), register(0)?, source)
        }
        _ => return None,
    })
    .filter(|op| supported(*op))
}

fn supported(op: Op) -> bool {
    // Only a branch tests PF in a unit; other conditions on it are left to
    // the interpreter.
    let parity = |cc| {
        matches!(
            cc,
            ConditionCode::p | ConditionCode::np | ConditionCode::None
        )
    };
    match op {
        Op::Jcc(Cc::Flags(cc), _) => cc != ConditionCode::None,
        Op::Setcc(cc, _) | Op::Cmov(cc, ..) => !parity(cc),
        _ => true,
    }
}

/// Condition codes by their encoding in `jcc`, `setcc` and `cmovcc`.
const CONDITIONS: [ConditionCode; 16] = {
    use ConditionCode as C;
    [
        C::o,
        C::no,
        C::b,
        C::ae,
        C::e,
        C::ne,
        C::be,
        C::a,
        C::s,
        C::ns,
        C::p,
        C::np,
        C::l,
        C::ge,
        C::le,
        C::g,
    ]
};

/// ALU operations by the `reg` field of opcodes 81 and 83; `None` for adc
/// and sbb.
const GROUP1: [Option<Alu>; 8] = [
    Some(Alu::Add),
    Some(Alu::Or),
    None,
    None,
    Some(Alu::And),
    Some(Alu::Sub),
    Some(Alu::Xor),
    Some(Alu::Cmp),
];

/// Instruction bytes; every read past the end fails.
struct Bytes<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Bytes<'_> {
    fn u8(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.at)?;
        self.at += 1;
        Some(byte)
    }

    /// A sign-extended byte.
    fn i8(&mut self) -> Option<u32> {
        self.u8().map(|byte| byte as i8 as u32)
    }

    fn u16(&mut self) -> Option<u16> {
        let bytes = self.bytes.get(self.at..self.at + 2)?;
        self.at += 2;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes = self.bytes.get(self.at..self.at + 4)?;
        self.at += 4;
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// The `reg` field and the operand of a ModRM byte, with 32-bit
    /// addressing.
    fn modrm(&mut self) -> Option<(u8, Src)> {
        let modrm = self.u8()?;
        let (mode, reg, rm) = (modrm >> 6, modrm >> 3 & 7, modrm & 7);
        if mode == 3 {
            return Some((reg, Src::Reg(rm)));
        }
        let (mut base, mut index, mut scale) = (Some(rm), None, 1);
        if rm == 4 {
            let sib = self.u8()?;
            // iced keeps the scale without an index, which then ignores it.
            scale = 1 << (sib >> 6);
            if sib >> 3 & 7 != 4 {
                index = Some(sib >> 3 & 7);
            }
            base = Some(sib & 7);
        }
        if mode == 0 && base == Some(5) {
            base = None;
        }
        let disp = match mode {
            0 if base.is_none() => self.u32()?,
            0 => 0,
            1 => self.i8()?,
            _ => self.u32()?,
        };
        let mem = Mem {
            base,
            index,
            scale,
            disp,
            fs: false,
        };
        Some((reg, Src::Mem(mem)))
    }
}

fn register(src: Src) -> Option<u8> {
    match src {
        Src::Reg(register) => Some(register),
        _ => None,
    }
}

/// The `Op` and length of the instruction at the start of `bytes`, at
/// `pc`, for the forms it knows; `None` leaves the rest to iced and `plan`.
pub fn quick(bytes: &[u8], pc: u32) -> Option<(Op, u32)> {
    let mut b = Bytes { bytes, at: 0 };
    let op = quick_op(&mut b, pc)?;
    supported(op).then_some((op, b.at as u32))
}

fn quick_op(b: &mut Bytes, pc: u32) -> Option<Op> {
    // The target of a branch whose displacement ends the instruction.
    let near = |b: &Bytes, rel: u32| pc.wrapping_add(b.at as u32).wrapping_add(rel);
    let opcode = b.u8()?;
    let alu = || match opcode >> 3 {
        0 => Alu::Add,
        1 => Alu::Or,
        4 => Alu::And,
        5 => Alu::Sub,
        6 => Alu::Xor,
        _ => Alu::Cmp,
    };
    Some(match opcode {
        0x01 | 0x09 | 0x21 | 0x29 | 0x31 | 0x39 => {
            let (reg, rm) = b.modrm()?;
            Op::Alu(alu(), rm, Src::Reg(reg))
        }
        0x03 | 0x0b | 0x23 | 0x2b | 0x33 | 0x3b => {
            let (reg, rm) = b.modrm()?;
            Op::Alu(alu(), Src::Reg(reg), rm)
        }
        0x05 | 0x0d | 0x25 | 0x2d | 0x35 | 0x3d => Op::Alu(alu(), Src::Reg(0), Src::Imm(b.u32()?)),
        0x11 | 0x19 => {
            let (reg, rm) = b.modrm()?;
            Op::Carry(opcode == 0x19, register(rm)?, Src::Reg(reg))
        }
        0x13 | 0x1b => {
            let (reg, rm) = b.modrm()?;
            Op::Carry(opcode == 0x1b, reg, rm)
        }
        0x15 | 0x1d => Op::Carry(opcode == 0x1d, 0, Src::Imm(b.u32()?)),
        0x40..=0x4f => Op::Step(opcode < 0x48, opcode & 7),
        0x50..=0x57 => Op::Push(Src::Reg(opcode & 7)),
        0x58..=0x5f => match opcode & 7 {
            ESP => return None,
            register => Op::Pop(register),
        },
        0xc9 => Op::Leave,
        0x68 => Op::Push(Src::Imm(b.u32()?)),
        0x6a => Op::Push(Src::Imm(b.i8()?)),
        0x69 | 0x6b => {
            let (reg, rm) = b.modrm()?;
            let imm = if opcode == 0x69 { b.u32()? } else { b.i8()? };
            Op::Imul(reg, rm, Some(imm))
        }
        0x70..=0x7f => {
            let rel = b.i8()?;
            Op::Jcc(
                Cc::Flags(CONDITIONS[usize::from(opcode & 15)]),
                near(b, rel),
            )
        }
        0x81 | 0x83 => {
            let (reg, rm) = b.modrm()?;
            let imm = Src::Imm(if opcode == 0x81 { b.u32()? } else { b.i8()? });
            match GROUP1[usize::from(reg)] {
                Some(alu) => Op::Alu(alu, rm, imm),
                None => Op::Carry(reg == 3, register(rm)?, imm),
            }
        }
        0x85 => {
            let (reg, rm) = b.modrm()?;
            Op::Alu(Alu::Test, rm, Src::Reg(reg))
        }
        0x89 => {
            let (reg, rm) = b.modrm()?;
            Op::Mov(rm, Src::Reg(reg))
        }
        0x8b => {
            let (reg, rm) = b.modrm()?;
            Op::Mov(Src::Reg(reg), rm)
        }
        0x8d => match b.modrm()? {
            (reg, Src::Mem(mem)) => Op::Lea(reg, mem),
            _ => return None,
        },
        0x90 => Op::Nop,
        0x99 => Op::Cdq,
        0xa1 | 0xa3 => {
            let mem = Src::Mem(Mem {
                base: None,
                index: None,
                scale: 1,
                disp: b.u32()?,
                fs: false,
            });
            match opcode {
                0xa1 => Op::Mov(Src::Reg(0), mem),
                _ => Op::Mov(mem, Src::Reg(0)),
            }
        }
        0xb8..=0xbf => Op::Mov(Src::Reg(opcode & 7), Src::Imm(b.u32()?)),
        0xc1 | 0xd1 => {
            let (reg, rm) = b.modrm()?;
            let count = if opcode == 0xc1 { b.u8()? } else { 1 };
            let kind = match reg {
                0 => ShiftKind::Rol,
                1 => ShiftKind::Ror,
                4 | 6 => ShiftKind::Shl,
                5 => ShiftKind::Shr,
                7 => ShiftKind::Sar,
                _ => return None,
            };
            let dst = register(rm)?;
            match u32::from(count) & 31 {
                0 => Op::Nop,
                count => Op::Shift(kind, dst, count),
            }
        }
        0xc2 => match b.u16()? {
            bytes if bytes < 4096 => Op::Ret(bytes),
            _ => return None,
        },
        0xc3 => Op::Ret(0),
        // `rep ret`, which older compilers emit after a branch.
        0xf3 if b.u8()? == 0xc3 => Op::Ret(0),
        // Operand-size padding: `xchg ax, ax` and `nop r/m16`.
        0x66 => match b.u8()? {
            0x90 => Op::Nop,
            0x0f if b.u8()? == 0x1f => match b.modrm()? {
                (0, _) => Op::Nop,
                _ => return None,
            },
            _ => return None,
        },
        0xc7 => match b.modrm()? {
            (0, rm) => Op::Mov(rm, Src::Imm(b.u32()?)),
            _ => return None,
        },
        0xe8 => {
            let rel = b.u32()?;
            Op::Call(near(b, rel))
        }
        0xe9 => {
            let rel = b.u32()?;
            Op::Jmp(near(b, rel))
        }
        0xeb => {
            let rel = b.i8()?;
            Op::Jmp(near(b, rel))
        }
        0xf7 => match b.modrm()? {
            (0 | 1, rm) => Op::Alu(Alu::Test, rm, Src::Imm(b.u32()?)),
            (2, rm) => Op::Not(register(rm)?),
            (3, rm) => Op::Neg(register(rm)?),
            _ => return None,
        },
        0xff => match b.modrm()? {
            (0, rm) => Op::Step(true, register(rm)?),
            (1, rm) => Op::Step(false, register(rm)?),
            (2, rm) => Op::CallIndirect(rm),
            (4, rm) => Op::JmpIndirect(rm),
            (6, rm) => Op::Push(rm),
            _ => return None,
        },
        0x0f => {
            let second = b.u8()?;
            let cc = CONDITIONS[usize::from(second & 15)];
            match second {
                0x40..=0x4f => {
                    let (reg, rm) = b.modrm()?;
                    Op::Cmov(cc, reg, rm)
                }
                0x80..=0x8f => {
                    let rel = b.u32()?;
                    Op::Jcc(Cc::Flags(cc), near(b, rel))
                }
                // The reg field is ignored.
                0x90..=0x9f => match b.modrm()? {
                    (_, Src::Reg(target)) => Op::Setcc(cc, target),
                    _ => return None,
                },
                0xaf => {
                    let (reg, rm) = b.modrm()?;
                    Op::Imul(reg, rm, None)
                }
                0x1f => match b.modrm()? {
                    (0, _) => Op::Nop,
                    _ => return None,
                },
                0xb6 | 0xb7 | 0xbe | 0xbf => {
                    let (reg, rm) = b.modrm()?;
                    let word = second & 1 != 0;
                    let source = match rm {
                        Src::Reg(n) if word => Narrow8or16::Word(n),
                        Src::Reg(n) => Narrow8or16::Byte(n),
                        Src::Mem(mem) => Narrow8or16::Mem(mem, word),
                        Src::Imm(_) => return None,
                    };
                    Op::Extend(reg, source, second >= 0xbe)
                }
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// An instruction the interpreter runs for translated code.
pub struct Helper {
    pub pc: u32,
    pub next: u32,
    pub prepared: Prepared,
}

/// Decodes guest instructions a page at a time, over the page and, when it
/// is executable too, the start of the next one; iced only sees a page
/// when `quick` declines an instruction on it.
pub struct Fetcher<'m> {
    memory: &'m Memory,
    page: Option<u32>,
    bytes: &'m [u8],
    decoder: Option<Decoder<'m>>,
    /// Why the last fetch failed: 1 not executable, 2 unreadable, 3 not
    /// handled.
    pub failure: u8,
    /// Whether instructions `plan` declines become `Op::Helper`, kept
    /// here.
    pub helpers: Option<Vec<Helper>>,
}

impl<'m> Fetcher<'m> {
    pub fn new(memory: &'m Memory) -> Self {
        Self {
            memory,
            page: None,
            bytes: &[],
            decoder: None,
            failure: 0,
            helpers: None,
        }
    }

    /// The `Op` and length of the instruction at `pc`, if it is entirely in
    /// executable memory and handled.
    pub fn fetch(&mut self, pc: u32) -> Option<(Op, u32)> {
        const PAGE: u32 = 0x1000;
        let page = pc & !(PAGE - 1);
        if self.page != Some(page) {
            self.page = None;
            self.decoder = None;
            if !self.memory.executable(page, PAGE as usize) {
                self.failure = 1;
                return None;
            }
            // An instruction may run up to 14 bytes into the next page.
            let more = page
                .checked_add(PAGE)
                .is_some_and(|next| self.memory.executable(next, 15));
            let len = PAGE as usize + if more { 15 } else { 0 };
            self.failure = 2;
            self.bytes = self.memory.read_linear(page, len).ok()?;
            self.page = Some(page);
        }
        self.failure = 3;
        let at = (pc - page) as usize;
        if let Some(quick) = quick(&self.bytes[at..], pc) {
            return Some(quick);
        }
        if self.decoder.is_none() {
            let decoder =
                Decoder::try_with_ip(32, self.bytes, u64::from(page), DecoderOptions::NONE).ok()?;
            self.decoder = Some(decoder);
        }
        let decoder = self.decoder.as_mut()?;
        decoder.set_position(at).ok()?;
        decoder.set_ip(u64::from(pc));
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return None;
        }
        let len = instruction.len();
        if let Some(op) = plan(&instruction) {
            return Some((op, len as u32));
        }
        let helpers = self.helpers.as_mut()?;
        // A software interrupt is a host call, which mostly returns to the
        // next instruction.
        let next = instruction.flow_control() == FlowControl::Next
            || instruction.code() == iced_x86::Code::Int_imm8;
        if !next || helpers.len() > usize::from(u8::MAX) {
            return None;
        }
        helpers.push(Helper {
            pc,
            next: instruction.next_ip32(),
            prepared: Prepared::new(&instruction, self.bytes.get(at..at + len)?)?,
        });
        // SIMD instructions keep their helper for when they are checked.
        let index = (helpers.len() - 1) as u8;
        let op = if let Some(simd) = super::simd::plan(&instruction, index) {
            Op::Simd(simd)
        } else if let Some(strings) = super::strings::plan(&instruction, index) {
            Op::Strings(strings)
        } else if let Some(integer) = super::integer::plan(&instruction, index) {
            Op::Integer(integer)
        } else if let Some(x87) = super::x87::plan(
            &instruction,
            index,
            helpers
                .last()
                .and_then(|helper| helper.prepared.x87_opcode()),
        ) {
            Op::X87(x87)
        } else {
            Op::Helper(index)
        };
        Some((op, len as u32))
    }
}

/// What an instruction does to the flags, and whether it may fault.
#[derive(Clone, Copy, Debug)]
pub struct Effects {
    pub reads: FlagSet,
    pub writes: FlagSet,
    /// It has a fault site, where every flag is observed.
    pub site: bool,
}

impl Op {
    pub fn effects(self) -> Effects {
        use FlagSet as F;
        let mem = |src: Src| matches!(src, Src::Mem(_));
        let (reads, writes, site) = match self {
            Op::Nop | Op::Lea(..) | Op::Not(_) | Op::Cdq | Op::Jmp(_) | Op::Direction(_) => {
                (F::NONE, F::NONE, false)
            }
            // MMX and floating-point instructions may leave for the
            // interpreter.
            Op::Simd(simd) => (
                F::NONE,
                if matches!(simd.kind, super::simd::Kind::FlagCompare(_)) {
                    F::ALL
                } else {
                    F::NONE
                },
                simd.memory() || simd.mmx() || simd.floating(),
            ),
            // A REP compare of no elements keeps the flags.
            Op::Strings(strings) if strings.compares() => {
                let reads = if strings.rep == super::strings::Rep::None {
                    F::NONE
                } else {
                    F::ALL
                };
                (reads, F::ALL, true)
            }
            Op::Strings(_) => (F::NONE, F::NONE, true),
            // The helper may run it.
            Op::X87(_) => (F::NONE, F::NONE, true),
            Op::Integer(integer) => integer.effects(),
            Op::Mov(dst, src) => (F::NONE, F::NONE, mem(dst) || mem(src)),
            Op::Alu(_, dst, src) => (F::NONE, F::ALL, mem(dst) || mem(src)),
            Op::Step(..) => (F::NONE, F::ALL.minus(F::CF), false),
            Op::Neg(_) => (F::NONE, F::ALL, false),
            Op::Extend(_, source, _) => (F::NONE, F::NONE, matches!(source, Narrow8or16::Mem(..))),
            Op::Push(_)
            | Op::Pop(_)
            | Op::Leave
            | Op::Call(_)
            | Op::Ret(_)
            | Op::CallIndirect(_) => (F::NONE, F::NONE, true),
            Op::JmpIndirect(target) => (F::NONE, F::NONE, mem(target)),
            Op::Jcc(Cc::Flags(cc), _) | Op::Setcc(cc, _) => (F::of(cc), F::NONE, false),
            Op::Jcc(Cc::Loop(Some(_)), _) => (F::ZF, F::NONE, false),
            Op::Jcc(..) => (F::NONE, F::NONE, false),
            Op::Cmov(cc, _, source) => (F::of(cc), F::NONE, mem(source)),
            Op::Shift(kind, _, count) => {
                let overflow = if count == 1 { F::OF } else { F::NONE };
                let writes = match kind {
                    ShiftKind::Rol | ShiftKind::Ror => F::CF,
                    _ => F::CF.or(F::PF).or(F::ZF).or(F::SF),
                };
                (F::NONE, writes.or(overflow), false)
            }
            Op::Imul(_, source, _) => (F::NONE, F::CF.or(F::OF), mem(source)),
            Op::Carry(_, _, source) => (F::CF, F::ALL, mem(source)),
            Op::Narrow(alu, _, dst, src) => {
                let site = matches!(dst, Part::Mem(_)) || matches!(src, Part::Mem(_));
                let writes = if alu.is_some() { F::ALL } else { F::NONE };
                (F::NONE, writes, site)
            }
            Op::StepMemory(..) => (F::NONE, F::ALL.minus(F::CF), true),
            Op::NarrowStep(_, _, dst) => {
                (F::NONE, F::ALL.minus(F::CF), matches!(dst, Part::Mem(_)))
            }
            // The interpreter reads and writes canonical flags.
            Op::Helper(_) => (F::ALL, F::NONE, true),
        };
        Effects {
            reads,
            writes,
            site,
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub struct Rng(pub u64);

    impl Rng {
        pub fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        pub fn byte(&mut self) -> u8 {
            self.next() as u8
        }
    }

    /// Opcodes `quick` knows, and a few near them it must decline.
    pub const OPCODES: &[&[u8]] = &[
        &[0x01],
        &[0x03],
        &[0x05],
        &[0x09],
        &[0x0b],
        &[0x0d],
        &[0x11],
        &[0x13],
        &[0x15],
        &[0x19],
        &[0x1b],
        &[0x1d],
        &[0x21],
        &[0x23],
        &[0x25],
        &[0x29],
        &[0x2b],
        &[0x2d],
        &[0x31],
        &[0x33],
        &[0x35],
        &[0x39],
        &[0x3b],
        &[0x3d],
        &[0x00],
        &[0x02],
        &[0x40],
        &[0x4c],
        &[0x53],
        &[0x5c],
        &[0x5e],
        &[0x68],
        &[0x6a],
        &[0x69],
        &[0x6b],
        &[0x70],
        &[0x7a],
        &[0x7f],
        &[0x80],
        &[0x81],
        &[0x83],
        &[0x84],
        &[0x85],
        &[0x89],
        &[0x8b],
        &[0x8d],
        &[0x90],
        &[0x99],
        &[0xa1],
        &[0xa3],
        &[0xb8],
        &[0xbc],
        &[0xc1],
        &[0xd1],
        &[0xd3],
        &[0xc2],
        &[0xc3],
        &[0xc6],
        &[0xc7],
        &[0xe8],
        &[0xe9],
        &[0xeb],
        &[0xf6],
        &[0xf7],
        &[0xfe],
        &[0xff],
        &[0x0f, 0x44],
        &[0x0f, 0x4a],
        &[0x0f, 0x8c],
        &[0x0f, 0x8b],
        &[0x0f, 0x94],
        &[0x0f, 0x9b],
        &[0x0f, 0xaf],
        &[0x0f, 0xb6],
        &[0x0f, 0xb7],
        &[0x0f, 0xbe],
        &[0x0f, 0xbf],
        &[0x0f, 0x1f],
        &[0x66, 0x89],
        &[0xf3, 0xc3],
        &[0xf0, 0x01],
        &[0x64, 0x8b],
        &[0x2e, 0x8b],
        &[0x67, 0x8b],
        &[0x66, 0x90],
        &[0x66, 0x0f, 0x1f],
        &[0xf3, 0x90],
        &[0xf3, 0xc2],
        &[0x66, 0x0f, 0xaf],
    ];

    /// What iced and `plan` make of the instruction at the start of `bytes`.
    fn iced(bytes: &[u8], pc: u32) -> Option<(Op, u32)> {
        let mut decoder = Decoder::with_ip(32, bytes, u64::from(pc), DecoderOptions::NONE);
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return None;
        }
        plan(&instruction).map(|op| (op, instruction.len() as u32))
    }

    // spec: A64-TR-1
    #[test]
    fn quick_agrees_with_iced() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        let (mut planned, mut covered) = (0u32, 0u32);
        for round in 0..1_000_000 {
            let mut bytes = [0u8; 16];
            for byte in &mut bytes {
                *byte = rng.byte();
            }
            // Mostly known opcodes with random operands, some raw bytes.
            let len = if round % 8 == 0 {
                bytes.len()
            } else {
                let opcode = OPCODES[rng.next() as usize % OPCODES.len()];
                bytes[..opcode.len()].copy_from_slice(opcode);
                if rng.next().is_multiple_of(4) {
                    // ModRM with a register operand.
                    bytes[opcode.len()] |= 0xc0;
                }
                // Some instructions run off the end of the bytes.
                if rng.next().is_multiple_of(16) {
                    1 + rng.next() as usize % 8
                } else {
                    bytes.len()
                }
            };
            let bytes = &bytes[..len];
            let pc = rng.next() as u32;
            let expected = iced(bytes, pc);
            let actual = quick(bytes, pc);
            if let Some(actual) = actual {
                assert_eq!(Some(actual), expected, "{bytes:02x?} at {pc:#x}");
            }
            // Narrow and memory `inc`/`dec` forms are iced's alone.
            let quick_form = !matches!(
                expected,
                Some((Op::Narrow(..) | Op::StepMemory(..) | Op::NarrowStep(..), _))
            );
            if expected.is_some() && quick_form && round % 8 != 0 {
                planned += 1;
                covered += u32::from(actual.is_some());
            }
        }
        // Declining is always correct, but it should be rare.
        assert!(covered * 100 >= planned * 95, "{covered} of {planned}");
    }
}
