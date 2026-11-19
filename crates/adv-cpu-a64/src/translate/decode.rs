//! Guest instructions the translator handles.
//!
//! `quick` decodes the forms compilers emit most straight to an `Op`, in a
//! few dozen cycles; anything else, prefixes included, goes through iced-x86
//! and `plan`. Whatever `quick` accepts, iced and `plan` decode to the same
//! `Op` and length (`quick_agrees_with_iced`).

pub use adv_cpu_core::interpreter::quick::{Alu, Mem, Narrow8or16, ShiftKind, Src};
use adv_cpu_core::interpreter::{Prepared, quick::QuickOp};
use adv_cpu_core::memory::Memory;
use iced_x86::{
    Code, ConditionCode, Decoder, DecoderOptions, FlowControl, Instruction, Mnemonic, OpKind,
    Register,
};

use super::flags::FlagSet;

pub const ESP: u8 = 4;
pub const EBP: u8 = 5;

/// An operand of an 8-, 16- or 32-bit instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// A byte register as `Narrow8or16::Byte` numbers it. Word and dword
    /// registers use guest indices.
    Reg(u8),
    Imm(u32),
    Mem(Mem),
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
    Pause,
    Fence,
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
    PopMemory(Mem),
    PushFlags(u8),
    PopFlags(u8, u8),
    Stack(Mnemonic, Part, u8, u8),
    Lahf,
    Timestamp,
    Syscall,
    Leave,
    Jmp(u32),
    Call(u32),
    Jcc(Cc, u32),
    Ret(u16),
    JmpIndirect(Src),
    CallIndirect(Src),
    Setcc(ConditionCode, u8),
    Cmov(ConditionCode, u8, Src),
    /// A dword shifted by an immediate count or CL (`None`).
    Shift(ShiftKind, Src, Option<u32>),
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
    /// An exact signed 64-bit integer load into the extended x87 stack.
    Fild64(Mem, u8, Option<u16>),
    /// A native integer instruction over partial operands (A64-TR-4).
    Parts(super::parts::Parts),
    /// Anything else the interpreter runs without transferring control:
    /// the `interpret` stub runs it, by its index in the trace's helpers.
    Helper(u8),
    /// A direct body call, by its index in the trace's bodies (A64-CALL-2).
    Body(u8),
}

fn gpr32(register: Register) -> Option<u8> {
    register.is_gpr32().then(|| register.number() as u8)
}

pub(super) fn memory(instruction: &Instruction) -> Option<Mem> {
    // The flat execution profile gives every segment except FS a zero base.
    let fs = match instruction.segment_prefix() {
        Register::FS => true,
        Register::GS => false,
        _ => false,
    };
    let register = |register| match register {
        Register::None => Some(None),
        register if register.is_gpr16() => Some(Some(register.number() as u8)),
        register => gpr32(register).map(Some),
    };
    Some(Mem {
        base: register(instruction.memory_base())?,
        index: register(instruction.memory_index())?,
        scale: instruction.memory_index_scale(),
        address_bits: if instruction.memory_base().is_gpr16()
            || instruction.memory_index().is_gpr16()
            || instruction.memory_displ_size() == 2
        {
            16
        } else {
            32
        },
        disp: instruction.memory_displacement32(),
        fs,
    })
}

/// The same address model, also accepting word address registers. Wrapping
/// precedes the segment base, as in the interpreter's `Address::resolve_regs`.
pub(super) fn memory_sized(instruction: &Instruction) -> Option<Mem> {
    let mut normalized = *instruction;
    let word = instruction.memory_base().is_gpr16() || instruction.memory_index().is_gpr16();
    if instruction.memory_base().is_gpr16() {
        normalized.set_memory_base(instruction.memory_base().full_register32());
    }
    if instruction.memory_index().is_gpr16() {
        normalized.set_memory_index(instruction.memory_index().full_register32());
    }
    let mut mem = memory(&normalized)?;
    if word {
        mem.address_bits = 16;
    }
    Some(mem)
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

/// Operand `n` of an integer instruction of `width` bytes.
pub(super) fn part(instruction: &Instruction, n: u32, width: u8) -> Option<Part> {
    let mask = match width {
        1 => 0xff,
        2 => 0xffff,
        _ => u32::MAX,
    };
    match instruction.op_kind(n) {
        OpKind::Register => {
            let register = instruction.op_register(n);
            let fits = if width == 1 {
                register.is_gpr8()
            } else if width == 2 {
                register.is_gpr16()
            } else {
                register.is_gpr32()
            };
            fits.then(|| Part::Reg(register.number() as u8))
        }
        OpKind::Immediate8
        | OpKind::Immediate16
        | OpKind::Immediate8to16
        | OpKind::Immediate32
        | OpKind::Immediate8to32 => Some(Part::Imm(instruction.immediate(n) as u32 & mask)),
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
    if matches!(instruction.mnemonic(), M::Rol | M::Ror | M::Rcl | M::Rcr)
        && instruction.op0_kind() == OpKind::Register
        && instruction.op1_kind() == OpKind::Immediate8
        && instruction.immediate8() & 31 == 0
    {
        return Some(Op::Nop);
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
        M::Pause => Op::Pause,
        M::Lfence | M::Sfence | M::Mfence => Op::Fence,
        // Hints that never fault; ENDBR32 marks indirect branch targets for
        // CET, which the guest never enables.
        M::Endbr32 => Op::Nop,
        M::Prefetcht0 | M::Prefetcht1 | M::Prefetcht2 | M::Prefetchnta => Op::Nop,
        M::Cld => Op::Direction(false),
        M::Std => Op::Direction(true),
        M::Movnti if instruction.memory_size().size() == 4 => {
            Op::Mov(Src::Mem(memory_sized(instruction)?), Src::Reg(register(1)?))
        }
        M::Mov => match two()? {
            (Src::Imm(_), _) | (Src::Mem(_), Src::Mem(_)) => return None,
            (dst, src) => mov(dst, src),
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
            let dst = operand(instruction, 0)?;
            let count = match instruction.op_kind(1) {
                OpKind::Immediate8 => Some(u32::from(instruction.immediate8()) & 31),
                OpKind::Register if instruction.op_register(1) == Register::CL => None,
                _ => return None,
            };
            let kind = match mnemonic {
                M::Shl | M::Sal => ShiftKind::Shl,
                M::Shr => ShiftKind::Shr,
                M::Sar => ShiftKind::Sar,
                M::Rol => ShiftKind::Rol,
                _ => ShiftKind::Ror,
            };
            // A zero count changes nothing, flags included.
            match (dst, count) {
                (Src::Reg(_), Some(0)) => Op::Nop,
                (Src::Reg(_), _) | (Src::Mem(_), None) => Op::Shift(kind, dst, count),
                _ => return None,
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
        M::Pop => match operand(instruction, 0)? {
            Src::Reg(register) => Op::Pop(register),
            Src::Mem(mem) => Op::PopMemory(mem),
            Src::Imm(_) => return None,
        },
        M::Pushfd => Op::PushFlags(4),
        M::Pushf => Op::PushFlags(2),
        M::Lahf => Op::Lahf,
        M::Rdtsc => Op::Timestamp,
        M::Int if instruction.immediate8() == 0x2e => Op::Syscall,
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

fn mov(dst: Src, src: Src) -> Op {
    match (dst, src) {
        (Src::Reg(dst), Src::Reg(src)) if dst == src => Op::Nop,
        _ => Op::Mov(dst, src),
    }
}

/// The planned operation and length for a common instruction.
pub fn quick(bytes: &[u8], pc: u32) -> Option<(Op, u32)> {
    let (op, len) = adv_cpu_core::interpreter::quick::quick(bytes, pc)?;
    let op = match op {
        QuickOp::Nop => Op::Nop,
        QuickOp::Pause => Op::Pause,
        QuickOp::Mov(a, b) => Op::Mov(a, b),
        QuickOp::Lea(a, b) => Op::Lea(a, b),
        QuickOp::Alu(a, b, c) => Op::Alu(a, b, c),
        QuickOp::Step(a, b) => Op::Step(a, b),
        QuickOp::Neg(a) => Op::Neg(a),
        QuickOp::Not(a) => Op::Not(a),
        QuickOp::Extend(a, b, c) => Op::Extend(a, b, c),
        QuickOp::Cdq => Op::Cdq,
        QuickOp::Push(a) => Op::Push(a),
        QuickOp::Pop(a) => Op::Pop(a),
        QuickOp::Leave => Op::Leave,
        QuickOp::Jmp(a) => Op::Jmp(a),
        QuickOp::Call(a) => Op::Call(a),
        QuickOp::Jcc(condition, target) => Op::Jcc(Cc::Flags(condition), target),
        QuickOp::Ret(a) => Op::Ret(a),
        QuickOp::JmpIndirect(a) => Op::JmpIndirect(a),
        QuickOp::CallIndirect(a) => Op::CallIndirect(a),
        QuickOp::Setcc(condition, dst) => Op::Setcc(condition, dst),
        QuickOp::Cmov(condition, dst, src) => Op::Cmov(condition, dst, src),
        QuickOp::Shift(a, b, c) => Op::Shift(a, b, c),
        QuickOp::Imul(a, b, c) => Op::Imul(a, b, c),
        QuickOp::Carry(a, b, c) => Op::Carry(a, b, c),
    };
    Some((op, len))
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
pub struct Fetcher {
    page: Option<u32>,
    buffer: Option<Box<[u8; 0x1000 + 15]>>,
    len: usize,
    copied: bool,
    /// Why the last fetch failed: 1 not executable, 2 unreadable, 3 not
    /// handled.
    pub failure: u8,
    /// Whether instructions `plan` declines become `Op::Helper`, kept
    /// here.
    pub helpers: Option<Vec<Helper>>,
    pub locked: Option<(Mem, u8, u8)>,
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher {
    pub fn new() -> Self {
        Self {
            page: None,
            buffer: None,
            len: 0,
            copied: false,
            failure: 0,
            helpers: None,
            locked: None,
        }
    }

    /// Permission/page lookup shared with native entry selection.
    pub fn fetchable(memory: &Memory, pc: u32) -> bool {
        let page = pc & !0xfff;
        memory.executable(page, 0x1000)
            && memory.code_tracked(page)
            && memory.committed(page, 0x1000).is_ok()
            && page
                .checked_add(0x1000)
                .is_none_or(|next| !memory.executable(next, 15) || memory.code_tracked(next))
    }

    /// The `Op` and length of the instruction at `pc`, if it is entirely in
    /// executable memory and handled.
    pub fn fetch(&mut self, memory: &mut Memory, pc: u32) -> Option<(Op, u32)> {
        self.locked = None;
        const PAGE: u32 = 0x1000;
        let page = pc & !(PAGE - 1);
        if self.page != Some(page) {
            self.page = None;
            if !memory.executable(page, PAGE as usize) {
                self.failure = 1;
                return None;
            }
            // An instruction may run up to 14 bytes into the next page.
            let more = page
                .checked_add(PAGE)
                .is_some_and(|next| memory.executable(next, 15));
            let len = PAGE as usize + if more { 15 } else { 0 };
            self.failure = 2;
            if !Self::fetchable(memory, pc) {
                return None;
            }
            self.copied = memory.read_linear(page, len).is_err();
            if self.copied {
                let buffer = self
                    .buffer
                    .get_or_insert_with(|| Box::new([0; 0x1000 + 15]));
                memory.read_into(page, &mut buffer[..len]).ok()?;
            }
            self.len = len;
            self.page = Some(page);
        }
        self.failure = 3;
        let at = (pc - page) as usize;
        let bytes = if self.copied {
            &self.buffer.as_ref().unwrap()[..self.len]
        } else {
            memory.read_linear(page, self.len).ok()?
        };
        if let Some(quick) = quick(&bytes[at..], pc) {
            return Some(quick);
        }
        // The decoder borrows only for this instruction, including owned copies.
        let mut decoder =
            Decoder::try_with_ip(32, bytes, u64::from(page), DecoderOptions::NONE).ok()?;
        decoder.set_position(at).ok()?;
        decoder.set_ip(u64::from(pc));
        let instruction = decoder.decode();
        if instruction.is_invalid() {
            return None;
        }
        let len = instruction.len();
        let locked = instruction.has_lock_prefix()
            || instruction.mnemonic() == Mnemonic::Xchg && instruction.op0_kind() == OpKind::Memory;
        if !locked && let Some(op) = plan(&instruction) {
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
            prepared: Prepared::new(&instruction, bytes.get(at..at + len)?)?,
        });
        // SIMD instructions keep their helper for when they are checked.
        let index = (helpers.len() - 1) as u8;
        if locked {
            self.locked = Some((
                memory_sized(&instruction)?,
                instruction.memory_size().size() as u8,
                index,
            ));
        }
        let opcode = helpers.last().and_then(|h| h.prepared.x87_opcode());
        let op = deferred_plan(&instruction, index, opcode).unwrap_or(Op::Helper(index));
        Some((op, len as u32))
    }
}

/// Native forms selected after preparing their exceptional interpreter case.
fn deferred_plan(instruction: &Instruction, index: u8, opcode: Option<u16>) -> Option<Op> {
    let mut native = *instruction;
    native.set_has_lock_prefix(false);
    let instruction = native;
    Some(if let Some(op) = plan(&instruction) {
        op
    } else if matches!(instruction.mnemonic(), Mnemonic::Popfd | Mnemonic::Popf) {
        Op::PopFlags(
            if instruction.mnemonic() == Mnemonic::Popf {
                2
            } else {
                4
            },
            index,
        )
    } else if matches!(
        instruction.mnemonic(),
        Mnemonic::Push
            | Mnemonic::Pop
            | Mnemonic::Pushad
            | Mnemonic::Pusha
            | Mnemonic::Popad
            | Mnemonic::Popa
            | Mnemonic::Leave
    ) {
        let m = instruction.mnemonic();
        let width = if matches!(m, Mnemonic::Pushad | Mnemonic::Popad) {
            4
        } else {
            2
        };
        let part = if matches!(m, Mnemonic::Push | Mnemonic::Pop) {
            part(&instruction, 0, width)?
        } else {
            Part::Reg(5)
        };
        Op::Stack(m, part, width, index)
    } else if let Some(simd) = super::simd::plan(&instruction, index) {
        Op::Simd(simd)
    } else if let Some(strings) = super::strings::plan(&instruction, index) {
        Op::Strings(strings)
    } else if let Some(integer) = super::integer::plan(&instruction, index) {
        Op::Integer(integer)
    } else if let Some(parts) = super::parts::plan(&instruction) {
        Op::Parts(parts)
    } else if instruction.code() == iced_x86::Code::Fild_m64int {
        Op::Fild64(memory(&instruction)?, index, opcode)
    } else {
        Op::X87(super::x87::plan(&instruction, index, opcode)?)
    })
}

/// What an instruction does to the flags, and whether it may fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effects {
    pub reads: FlagSet,
    pub writes: FlagSet,
    /// It has a fault site, where every flag is observed.
    pub site: bool,
}

impl Op {
    /// A load from a shared clock field's constant address (PROC-CLOCK-7).
    pub fn reads_clock(self) -> bool {
        match self {
            Op::Mov(Src::Reg(_), Src::Mem(mem)) => {
                mem.base.is_none()
                    && mem.index.is_none()
                    && !mem.fs
                    && mem.address_bits == 32
                    && adv_cpu_core::shared_clock(mem.disp)
            }
            _ => false,
        }
    }

    pub fn effects(self) -> Effects {
        use FlagSet as F;
        let mem = |src: Src| matches!(src, Src::Mem(_));
        let (reads, writes, site) = match self {
            Op::Nop
            | Op::Pause
            | Op::Fence
            | Op::Lea(..)
            | Op::Not(_)
            | Op::Cdq
            | Op::Jmp(_)
            | Op::Direction(_) => (F::NONE, F::NONE, false),
            // MMX and floating-point instructions may leave for the
            // interpreter.
            Op::Simd(simd) => (
                F::NONE,
                if matches!(simd.kind, super::simd::Kind::FlagCompare(..)) {
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
            Op::X87(_) | Op::Fild64(..) => (F::NONE, F::NONE, true),
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
            Op::Shift(kind, dst, count) => {
                let overflow = if count.is_none_or(|count| count == 1) {
                    F::OF
                } else {
                    F::NONE
                };
                let writes = match kind {
                    ShiftKind::Rol | ShiftKind::Ror => F::CF,
                    _ => F::CF.or(F::PF).or(F::ZF).or(F::SF),
                };
                let writes = writes.or(overflow);
                let reads = if count.is_none() { writes } else { F::NONE };
                (reads, writes, mem(dst))
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
            Op::Helper(_) | Op::Syscall | Op::Timestamp => (F::ALL, F::NONE, true),
            Op::Stack(..) => (F::NONE, F::NONE, true),
            Op::PopMemory(_) => (F::NONE, F::NONE, true),
            Op::PushFlags(_) | Op::Lahf => (F::ALL, F::NONE, true),
            Op::PopFlags(..) => (F::ALL, F::ALL, true),
            // Calls leave the arithmetic flags undefined (A64-CALL-2).
            Op::Body(_) => (F::NONE, F::ALL, true),
            Op::Parts(parts) => (F::ALL, F::ALL, parts.memory().is_some()),
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

    // spec: A64-TR-1, A64-FLAG-2
    #[test]
    fn cl_shifts_use_the_shift_path_for_dword_registers_and_memory() {
        for group in [0, 1, 4, 5, 6, 7] {
            for dst in 0..8 {
                for mode in [0x00, 0xc0] {
                    let bytes = [0xd3, mode | group << 3 | dst, 0, 0, 0, 0, 0];
                    let expected = iced(&bytes, 0).unwrap();
                    assert!(matches!(expected.0, Op::Shift(_, _, None)));
                    assert_eq!(quick(&bytes, 0), Some(expected));
                }
            }
        }
        for bytes in [
            &[0xd2, 0xe0][..],
            &[0x66, 0xd3, 0xe0],
            &[0xd3, 0xd0],
            &[0x0f, 0xa5, 0xd0],
        ] {
            assert_eq!(iced(bytes, 0), None);
        }
    }

    // spec: A64-TR-1
    #[test]
    fn self_moves_decode_as_nop() {
        // mov edi, edi of the hot-patch prologue, in both encodings.
        for bytes in [&[0x8b, 0xff][..], &[0x89, 0xff]] {
            assert_eq!(quick(bytes, 0), Some((Op::Nop, 2)));
            assert_eq!(iced(bytes, 0), Some((Op::Nop, 2)));
        }
        let other = [0x8b, 0xfe];
        assert_eq!(
            quick(&other, 0),
            Some((Op::Mov(Src::Reg(7), Src::Reg(6)), 2))
        );
        assert_eq!(iced(&other, 0), quick(&other, 0));
    }

    // spec: A64-TR-1, A64-FLAG-2
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
            // Narrow forms, memory INC/DEC, POP ESP and word addresses use iced.
            // Their native plans do not expand quick's coverage contract.
            let quick_form = !bytes.starts_with(&[0x67])
                && !matches!(
                    expected,
                    Some((
                        Op::Narrow(..) | Op::StepMemory(..) | Op::NarrowStep(..) | Op::Pop(ESP),
                        _
                    ))
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

#[cfg(test)]
mod whitelist_audit {
    use super::*;
    // spec: A64-TR-4
    #[test]
    fn common_form_audit() {
        let mut missing = std::collections::BTreeSet::new();
        for prefix in [
            &[][..],
            &[0x66],
            &[0x67],
            &[0x64],
            &[0x65],
            &[0xf0],
            &[0xf2],
            &[0xf3],
        ] {
            for extended in [false, true] {
                for opcode in 0..=255u8 {
                    for modrm in 0..=255u8 {
                        let mut bytes = prefix.to_vec();
                        if extended {
                            bytes.push(0x0f);
                        }
                        bytes.extend_from_slice(&[opcode, modrm]);
                        bytes.resize(16, 0);
                        let i =
                            Decoder::with_ip(32, &bytes, 0x10000, DecoderOptions::NONE).decode();
                        if i.is_invalid()
                            || i.flow_control() != FlowControl::Next
                            || Prepared::new(&i, &bytes[..i.len()]).is_none()
                        {
                            continue;
                        }
                        let native = deferred_plan(&i, 0, None).is_some();
                        let floating = adv_cpu_core::floating::operation(&i)
                            == Some(adv_cpu_core::floating::Operation::X87);
                        let word_string = (0..i.op_count()).any(|n| {
                            matches!(
                                i.op_kind(n),
                                OpKind::MemorySegSI | OpKind::MemorySegDI | OpKind::MemoryESDI
                            )
                        });
                        let allowed = floating
                            || word_string
                            || matches!(
                                i.mnemonic(),
                                Mnemonic::Cpuid
                                    | Mnemonic::Cli
                                    | Mnemonic::Sti
                                    | Mnemonic::Sgdt
                                    | Mnemonic::Sidt
                                    | Mnemonic::Sldt
                            );
                        if !native && !allowed {
                            missing.insert(format!("{:?} prefix={prefix:x?}", i.code()));
                        }
                    }
                }
            }
        }
        assert!(missing.is_empty(), "forms outside A64-TR-4: {missing:#?}");
    }
}
