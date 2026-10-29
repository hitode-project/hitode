//! Operation helpers (A64-TR-4): integer instructions without native forms
//! run the interpreter's own semantics on operands in host registers. They
//! cover what `adv_cpu_core::integer::decode` exports, plus double shifts:
//! a mnemonic or condition, a destination and up to one source. The
//! unit reads the operands, memory through its usual access paths, calls
//! `run` through the `apply` stub, which keeps guest registers, temporaries
//! and SIMD caches, then writes the result and, after that last access, the
//! flags (A64-FLAG-4).

use adv_cpu_core::integer::{self, Operand, Operation};
use iced_x86::{ConditionCode, Instruction, Mnemonic as M, OpKind, Register};

use crate::encode::Size::{W, X};
use crate::encode::*;

use super::decode::{Mem, Part, memory};
use super::{State, Translator, X0, X1, X2, X3, X6, X10, X11, X13, X14, host};

const ECX: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Semantic {
    /// The shift, binary, unary or extension operation of this mnemonic.
    Op(M),
    Set(ConditionCode),
    Cmov(ConditionCode),
}

impl Semantic {
    /// The tag in bits 0-7 and the mnemonic or condition from bit 16.
    fn pack(self) -> u32 {
        match self {
            Self::Op(kind) => (kind as u32) << 16,
            Self::Set(condition) => 1 | (condition as u32) << 16,
            Self::Cmov(condition) => 2 | (condition as u32) << 16,
        }
    }

    fn unpack(op: u32) -> Self {
        let value = (op >> 16) as usize;
        let condition = || ConditionCode::try_from(value).unwrap();
        match op & 0xff {
            0 => Self::Op(M::try_from(value).unwrap()),
            1 => Self::Set(condition()),
            _ => Self::Cmov(condition()),
        }
    }

    /// Whether `run` reads the destination's value.
    fn reads(self) -> bool {
        !matches!(self, Self::Set(_) | Self::Op(M::Movsx | M::Movzx))
    }

    fn writes(self) -> bool {
        !matches!(self, Self::Op(M::Cmp | M::Test))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// 1 to 31.
    Imm(u8),
    Cl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Apply {
    pub semantic: Semantic,
    /// 1, 2 or 4 bytes.
    pub width: u8,
    pub dst: Part,
    /// The second operand and its width: a binary, CMOV or extension
    /// source, or the register a double shift fills from.
    pub source: Option<(Part, u8)>,
    /// A shift's count.
    pub count: Option<Count>,
}

impl Apply {
    /// The operand in memory, if one is.
    pub fn memory(&self) -> Option<Mem> {
        [Some(self.dst), self.source.map(|(part, _)| part)]
            .into_iter()
            .find_map(|part| match part {
                Some(Part::Mem(mem)) => Some(mem),
                _ => None,
            })
    }
}

/// The operation helper running `instruction`, if one does.
pub fn plan(instruction: &Instruction) -> Option<Apply> {
    if matches!(instruction.mnemonic(), M::Shld | M::Shrd) {
        return double_shift(instruction);
    }
    // Byte registers numbered as `Narrow8or16::Byte` numbers them.
    let part = |operand: Operand, width: u8| match operand {
        Operand::Register(r) => Some((Part::Reg(r.index + r.shift / 2), r.width)),
        Operand::Memory(_, width) => Some((Part::Mem(memory(instruction)?), width)),
        Operand::Immediate(value) => Some((Part::Imm(value), width)),
    };
    let (semantic, dst, source, count) = match integer::decode(instruction)? {
        Operation::Shift(kind, dst, count) => {
            let count = match count {
                // A zero count changes nothing but still checks the write:
                // the interpreter keeps that.
                Operand::Immediate(count) => match count & 31 {
                    0 => return None,
                    count => Count::Imm(count as u8),
                },
                _ => Count::Cl,
            };
            (Semantic::Op(kind), dst, None, Some(count))
        }
        Operation::Binary(kind, dst, source) => (Semantic::Op(kind), dst, Some(source), None),
        Operation::Unary(kind, dst) => (Semantic::Op(kind), dst, None, None),
        Operation::Mov(dst, source, signed) => {
            let kind = if signed { M::Movsx } else { M::Movzx };
            (Semantic::Op(kind), dst, Some(source), None)
        }
        Operation::Set(dst, condition) => (Semantic::Set(condition), dst, None, None),
        Operation::Cmov(dst, source, condition) => {
            (Semantic::Cmov(condition), dst, Some(source), None)
        }
        _ => return None,
    };
    let (dst, width) = part(dst, 0)?;
    let source = match source {
        Some(source) => Some(part(source, width)?),
        None => None,
    };
    matches!(width, 1 | 2 | 4).then_some(Apply {
        semantic,
        width,
        dst,
        source,
        count,
    })
}

/// SHLD or SHRD, which the shared integer decoder leaves out.
fn double_shift(instruction: &Instruction) -> Option<Apply> {
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
            Part::Mem(memory(instruction)?),
            instruction.memory_size().size() as u8,
        ),
        _ => return None,
    };
    let (source, size) = gpr(instruction.op1_register())?;
    if instruction.op1_kind() != OpKind::Register || size != width {
        return None;
    }
    let count = match instruction.op2_kind() {
        OpKind::Immediate8 => match instruction.immediate8() & 31 {
            0 => return None,
            count => Count::Imm(count),
        },
        OpKind::Register if instruction.op2_register() == Register::CL => Count::Cl,
        _ => return None,
    };
    matches!(width, 2 | 4).then_some(Apply {
        semantic: Semantic::Op(instruction.mnemonic()),
        width,
        dst,
        source: Some((Part::Reg(source), width)),
        count: Some(count),
    })
}

impl Translator<'_> {
    pub(super) fn apply(&mut self, op: Apply) {
        self.canonicalize();
        let address = match op.memory() {
            Some(mem) => self.effective(mem),
            None => Reg::ZR,
        };
        if op.semantic.reads() {
            self.narrow_read(op.dst, op.width, address, X6);
        }
        let mut width = 0;
        if let Some((source, size)) = op.source {
            self.narrow_read(source, size, address, X2);
            width = size;
        }
        match op.count {
            Some(Count::Imm(count)) => self.mov_imm(X3, u32::from(count)),
            Some(Count::Cl) => self.emit(and_imm(W, X3, host(ECX), 31)),
            None => {}
        }
        self.emit(mov(W, X1, X6));
        let packed = op.semantic.pack() | u32::from(op.width) << 8 | u32::from(width) << 12;
        self.mov_imm(X0, packed);
        self.far_call(self.stubs.apply);
        self.emit_all(&[mov(W, X6, X0), lsr_imm(X, X14, X0, 32)]);
        if op.semantic.writes() {
            self.narrow_write(op.dst, op.width, address, X6);
        }
        // The packed flags: NZCV in bits 0-3, AF in bit 4, x10 in bit 5.
        self.emit_all(&[
            lsl_imm(W, X13, X14, 28),
            msr_nzcv(X13),
            ubfx(W, X10, X14, 5, 1),
            and_imm(W, X11, X14, 0x10),
        ]);
        self.state = State::CANONICAL;
    }
}

/// Called by the `apply` stub: operation `op` (`Semantic::pack`, the width
/// in bits 8-11 and the source's in bits 12-15) of `value`, `source` and
/// `count`, with the canonical flags NZCV (subtraction sense), x10 and x11.
/// Returns the result in the low half and the flags packed as
/// `Translator::apply` unpacks them.
pub(crate) extern "C" fn run(
    op: u32,
    value: u32,
    source: u32,
    count: u32,
    nzcv: u64,
    pf: u64,
    af: u64,
) -> u64 {
    let n = |bit: u32| nzcv >> bit & 1 != 0;
    let mut flags = (u32::from(!n(29)) * integer::CF)
        | (u32::from((pf as u8).count_ones().is_multiple_of(2)) * integer::PF)
        | (af as u32 & integer::AF)
        | (u32::from(n(30)) * integer::ZF)
        | (u32::from(n(31)) * integer::SF)
        | (u32::from(n(28)) * integer::OF);
    let (width, source_width) = ((op >> 8 & 15) as u8, (op >> 12 & 15) as u8);
    // A zero CL count leaves the value and flags as they were.
    let count = count & 31;
    let result = match Semantic::unpack(op) {
        Semantic::Set(condition) => u32::from(integer::condition_true(condition, flags)),
        Semantic::Cmov(condition) if integer::condition_true(condition, flags) => source,
        Semantic::Cmov(_) => value,
        Semantic::Op(M::Movsx) => integer::signed(source, source_width) as u32,
        Semantic::Op(M::Movzx) => source,
        Semantic::Op(kind @ (M::Inc | M::Dec | M::Not | M::Neg)) => {
            integer::unary(&mut flags, kind, value, width)
        }
        Semantic::Op(M::Shl | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr)
        | Semantic::Op(M::Shld | M::Shrd)
            if count == 0 =>
        {
            value
        }
        Semantic::Op(kind @ (M::Shld | M::Shrd)) => {
            integer::double_shift(&mut flags, kind == M::Shld, value, source, count, width)
        }
        Semantic::Op(kind @ (M::Shl | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr)) => {
            integer::shift(&mut flags, kind, value, count, width)
        }
        Semantic::Op(kind) => integer::binary(&mut flags, kind, value, source, width),
    };
    let has = |bit: u32| u64::from(flags & bit != 0);
    let packed = has(integer::OF)
        | (1 - has(integer::CF)) << 1
        | has(integer::ZF) << 2
        | has(integer::SF) << 3
        | has(integer::AF) << 4
        | (1 - has(integer::PF)) << 5;
    u64::from(result) | packed << 32
}

#[cfg(test)]
mod tests {
    use super::super::decode::{Fetcher, Op};
    use iced_x86::code_asm::*;

    /// The ops the forms below decode to, one per instruction.
    fn decode(build: impl Fn(&mut CodeAssembler)) -> Vec<Op> {
        let mut a = CodeAssembler::new(32).unwrap();
        build(&mut a);
        let code = a.assemble(0x10000).unwrap();
        let mut memory = adv_cpu_core::Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, 7).unwrap();
        memory.write(0x10000, &code).unwrap();
        let mut fetcher = Fetcher::new(&memory);
        fetcher.helpers = Some(Vec::new());
        let (mut pc, mut ops) = (0x10000, Vec::new());
        while pc < 0x10000 + code.len() as u32 {
            let (op, len) = fetcher.fetch(pc).unwrap();
            ops.push(op);
            pc += len;
        }
        ops
    }

    // spec: A64-TR-4
    #[test]
    fn shifts_without_native_forms_run_as_operation_helpers() {
        let ops = decode(|a| {
            a.shr(dword_ptr(ebx + 8), 1).unwrap();
            a.sar(dword_ptr(esi), 5).unwrap();
            a.rol(word_ptr(ebx), 3).unwrap();
            a.rcl(byte_ptr(edi), cl).unwrap();
            a.ror(dh, cl).unwrap();
            a.rcr(cx, 1).unwrap();
            a.shrd(ebx, edx, cl).unwrap();
            a.shld(eax, esi, 7).unwrap();
            a.shrd(word_ptr(edi), ax, 3).unwrap();
            a.shld(dword_ptr(ebx), ecx, cl).unwrap();
        });
        assert_eq!(ops.len(), 10);
        for op in ops {
            assert!(matches!(op, Op::Apply(_)), "{op:?}");
        }
        // A zero count still checks the write, which the interpreter does.
        let zero = decode(|a| a.shl(dword_ptr(ebx), 32).unwrap());
        assert!(matches!(zero[..], [Op::Helper(_)]), "{zero:?}");
    }

    // spec: A64-TR-4
    #[test]
    fn integer_forms_without_native_forms_run_as_operation_helpers() {
        let ops = decode(|a| {
            a.adc(dword_ptr(ebx + 8), eax).unwrap();
            a.sbb(cx, word_ptr(esi)).unwrap();
            a.adc(bh, 3).unwrap();
        });
        for op in ops {
            assert!(matches!(op, Op::Apply(_)), "{op:?}");
        }
    }
}
