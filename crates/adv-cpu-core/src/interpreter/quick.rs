//! Single-instruction decoding for the most common x86 forms.
//!
//! The first-visit interpreter and the AArch64 front end share the parser.
//! Other forms go through the full decoder.

use iced_x86::ConditionCode;

use crate::Memory;

const ESP: u8 = 4;
const PAGE: u32 = 4096;

/// Fetches from one tracked page with the possible instruction tail on the
/// next page. The returned slice is borrowed only until the instruction runs.
#[derive(Default)]
pub struct Window {
    page: Option<u32>,
    buffer: Option<Box<[u8; PAGE as usize + 15]>>,
    len: usize,
    copied: bool,
}

impl Window {
    pub fn fetchable(memory: &Memory, pc: u32) -> bool {
        let page = pc & !(PAGE - 1);
        memory.executable(page, PAGE as usize)
            && memory.code_tracked(page)
            && memory.committed(page, PAGE as usize).is_ok()
            && page
                .checked_add(PAGE)
                .is_none_or(|next| !memory.executable(next, 15) || memory.code_tracked(next))
    }

    pub fn fetch<'a>(&'a mut self, memory: &'a Memory, pc: u32) -> Result<&'a [u8], u8> {
        let page = pc & !(PAGE - 1);
        if self.page != Some(page) {
            self.page = None;
            if !memory.executable(page, PAGE as usize) {
                return Err(1);
            }
            let more = page
                .checked_add(PAGE)
                .is_some_and(|next| memory.executable(next, 15));
            let len = PAGE as usize + if more { 15 } else { 0 };
            if !Self::fetchable(memory, pc) {
                return Err(2);
            }
            self.copied = memory.read_linear(page, len).is_err();
            if self.copied {
                let buffer = self
                    .buffer
                    .get_or_insert_with(|| Box::new([0; PAGE as usize + 15]));
                memory.read_into(page, &mut buffer[..len]).map_err(|_| 2)?;
            }
            self.len = len;
            self.page = Some(page);
        }
        let bytes = if self.copied {
            &self.buffer.as_ref().unwrap()[..self.len]
        } else {
            memory.read_linear(page, self.len).map_err(|_| 3)?
        };
        Ok(&bytes[(pc - page) as usize..])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mem {
    pub base: Option<u8>,
    pub index: Option<u8>,
    pub scale: u32,
    /// Effective address width before adding the segment base.
    pub address_bits: u8,
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

/// A narrow source of `movzx`/`movsx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Narrow8or16 {
    /// A byte register: AL..BL are 0..3, AH..BH 4..7.
    Byte(u8),
    /// A word register by guest index.
    Word(u8),
    Mem(Mem, bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickOp {
    Nop,
    Pause,
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
    Jcc(ConditionCode, u32),
    Ret(u16),
    JmpIndirect(Src),
    CallIndirect(Src),
    Setcc(ConditionCode, u8),
    Cmov(ConditionCode, u8, Src),
    Shift(ShiftKind, Src, Option<u32>),
    Imul(u8, Src, Option<u32>),
    Carry(bool, u8, Src),
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
            address_bits: 32,
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

/// A move of a register to itself, such as the hot-patch prologue's
/// `mov edi, edi`, changes nothing, flags included.
fn mov(dst: Src, src: Src) -> QuickOp {
    match (dst, src) {
        (Src::Reg(dst), Src::Reg(src)) if dst == src => QuickOp::Nop,
        _ => QuickOp::Mov(dst, src),
    }
}

/// The decoded form and its byte length, for the common unprefixed forms.
pub fn quick(bytes: &[u8], pc: u32) -> Option<(QuickOp, u32)> {
    let mut b = Bytes { bytes, at: 0 };
    let op = quick_op(&mut b, pc)?;
    if matches!(
        op,
        QuickOp::Setcc(ConditionCode::p | ConditionCode::np, _)
            | QuickOp::Cmov(ConditionCode::p | ConditionCode::np, ..)
    ) {
        return None;
    }
    Some((op, b.at as u32))
}

fn quick_op(b: &mut Bytes, pc: u32) -> Option<QuickOp> {
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
            QuickOp::Alu(alu(), rm, Src::Reg(reg))
        }
        0x03 | 0x0b | 0x23 | 0x2b | 0x33 | 0x3b => {
            let (reg, rm) = b.modrm()?;
            QuickOp::Alu(alu(), Src::Reg(reg), rm)
        }
        0x05 | 0x0d | 0x25 | 0x2d | 0x35 | 0x3d => {
            QuickOp::Alu(alu(), Src::Reg(0), Src::Imm(b.u32()?))
        }
        0x11 | 0x19 => {
            let (reg, rm) = b.modrm()?;
            QuickOp::Carry(opcode == 0x19, register(rm)?, Src::Reg(reg))
        }
        0x13 | 0x1b => {
            let (reg, rm) = b.modrm()?;
            QuickOp::Carry(opcode == 0x1b, reg, rm)
        }
        0x15 | 0x1d => QuickOp::Carry(opcode == 0x1d, 0, Src::Imm(b.u32()?)),
        0x40..=0x4f => QuickOp::Step(opcode < 0x48, opcode & 7),
        0x50..=0x57 => QuickOp::Push(Src::Reg(opcode & 7)),
        0x58..=0x5f => match opcode & 7 {
            ESP => return None,
            register => QuickOp::Pop(register),
        },
        0xc9 => QuickOp::Leave,
        0x68 => QuickOp::Push(Src::Imm(b.u32()?)),
        0x6a => QuickOp::Push(Src::Imm(b.i8()?)),
        0x69 | 0x6b => {
            let (reg, rm) = b.modrm()?;
            let imm = if opcode == 0x69 { b.u32()? } else { b.i8()? };
            QuickOp::Imul(reg, rm, Some(imm))
        }
        0x70..=0x7f => {
            let rel = b.i8()?;
            QuickOp::Jcc(CONDITIONS[usize::from(opcode & 15)], near(b, rel))
        }
        0x81 | 0x83 => {
            let (reg, rm) = b.modrm()?;
            let imm = Src::Imm(if opcode == 0x81 { b.u32()? } else { b.i8()? });
            match GROUP1[usize::from(reg)] {
                Some(alu) => QuickOp::Alu(alu, rm, imm),
                None => QuickOp::Carry(reg == 3, register(rm)?, imm),
            }
        }
        0x85 => {
            let (reg, rm) = b.modrm()?;
            QuickOp::Alu(Alu::Test, rm, Src::Reg(reg))
        }
        0x89 => {
            let (reg, rm) = b.modrm()?;
            mov(rm, Src::Reg(reg))
        }
        0x8b => {
            let (reg, rm) = b.modrm()?;
            mov(Src::Reg(reg), rm)
        }
        0x8d => match b.modrm()? {
            (reg, Src::Mem(mem)) => QuickOp::Lea(reg, mem),
            _ => return None,
        },
        0x90 => QuickOp::Nop,
        0x99 => QuickOp::Cdq,
        0xa1 | 0xa3 => {
            let mem = Src::Mem(Mem {
                base: None,
                index: None,
                scale: 1,
                address_bits: 32,
                disp: b.u32()?,
                fs: false,
            });
            match opcode {
                0xa1 => QuickOp::Mov(Src::Reg(0), mem),
                _ => QuickOp::Mov(mem, Src::Reg(0)),
            }
        }
        0xb8..=0xbf => QuickOp::Mov(Src::Reg(opcode & 7), Src::Imm(b.u32()?)),
        0xc1 | 0xd1 | 0xd3 => {
            let (reg, rm) = b.modrm()?;
            let count = match opcode {
                0xc1 => Some(u32::from(b.u8()?) & 31),
                0xd1 => Some(1),
                _ => None,
            };
            let kind = match reg {
                0 => ShiftKind::Rol,
                1 => ShiftKind::Ror,
                4 | 6 => ShiftKind::Shl,
                5 => ShiftKind::Shr,
                7 => ShiftKind::Sar,
                _ => return None,
            };
            match (rm, count) {
                (Src::Reg(_), Some(0)) => QuickOp::Nop,
                (Src::Reg(_), _) | (Src::Mem(_), None) => QuickOp::Shift(kind, rm, count),
                _ => return None,
            }
        }
        0xc2 => match b.u16()? {
            bytes if bytes < 4096 => QuickOp::Ret(bytes),
            _ => return None,
        },
        0xc3 => QuickOp::Ret(0),
        // PAUSE and `rep ret`, which older compilers emit after a branch.
        0xf3 => match b.u8()? {
            0x90 => QuickOp::Pause,
            0xc3 => QuickOp::Ret(0),
            _ => return None,
        },
        // QuickOperand-size padding: `xchg ax, ax` and `nop r/m16`.
        0x66 => match b.u8()? {
            0x90 => QuickOp::Nop,
            0x0f if b.u8()? == 0x1f => match b.modrm()? {
                (0, _) => QuickOp::Nop,
                _ => return None,
            },
            _ => return None,
        },
        0xc7 => match b.modrm()? {
            (0, rm) => QuickOp::Mov(rm, Src::Imm(b.u32()?)),
            _ => return None,
        },
        0xe8 => {
            let rel = b.u32()?;
            QuickOp::Call(near(b, rel))
        }
        0xe9 => {
            let rel = b.u32()?;
            QuickOp::Jmp(near(b, rel))
        }
        0xeb => {
            let rel = b.i8()?;
            QuickOp::Jmp(near(b, rel))
        }
        0xf7 => match b.modrm()? {
            (0 | 1, rm) => QuickOp::Alu(Alu::Test, rm, Src::Imm(b.u32()?)),
            (2, rm) => QuickOp::Not(register(rm)?),
            (3, rm) => QuickOp::Neg(register(rm)?),
            _ => return None,
        },
        0xff => match b.modrm()? {
            (0, rm) => QuickOp::Step(true, register(rm)?),
            (1, rm) => QuickOp::Step(false, register(rm)?),
            (2, rm) => QuickOp::CallIndirect(rm),
            (4, rm) => QuickOp::JmpIndirect(rm),
            (6, rm) => QuickOp::Push(rm),
            _ => return None,
        },
        0x0f => {
            let second = b.u8()?;
            let cc = CONDITIONS[usize::from(second & 15)];
            match second {
                0x40..=0x4f => {
                    let (reg, rm) = b.modrm()?;
                    QuickOp::Cmov(cc, reg, rm)
                }
                0x80..=0x8f => {
                    let rel = b.u32()?;
                    QuickOp::Jcc(cc, near(b, rel))
                }
                // The reg field is ignored.
                0x90..=0x9f => match b.modrm()? {
                    (_, Src::Reg(target)) => QuickOp::Setcc(cc, target),
                    _ => return None,
                },
                0xaf => {
                    let (reg, rm) = b.modrm()?;
                    QuickOp::Imul(reg, rm, None)
                }
                0x1f => match b.modrm()? {
                    (0, _) => QuickOp::Nop,
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
                    QuickOp::Extend(reg, source, second >= 0xbe)
                }
                _ => return None,
            }
        }
        _ => return None,
    })
}
