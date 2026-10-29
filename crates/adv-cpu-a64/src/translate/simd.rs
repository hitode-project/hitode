//! MMX and SSE instructions in NEON (A64-REG-5, A64F-FP-4, A64F-FP-5).
//!
//! A unit caches MMX registers in d8-d15 and XMM registers in v16-v23.
//! A register is loaded from `CpuState` when the unit first reads it.
//! Writes mark it dirty; internal edges outside integer pixel loops,
//! exits, helpers, host calls and x87/EMMS entries publish dirty MMX to MMX
//! and the aliased x87 significand and exponent (0xffff), and dirty XMM to
//! XMM. Direct faults record the dirty mask and recover that prefix from
//! the signal context before re-executing the interrupted instruction.
//! Region heads hold nothing; a merge holds its entry set
//! (`translate::residency`), which arrivals load what they lack of.
//! A helper may change any register, so the cache is forgotten after one;
//! d8-d15 survive the Rust calls between, and the `access` and `apply`
//! stubs keep v0-v7 and v16-v23.
//!
//! An MMX instruction without the x87 guard (`Guards::ready`) leaves for
//! the interpreter if an x87 exception is pending, and once it completes
//! sets TOP and every tag to 0, as each MMX instruction does. Guards hold
//! until a helper, x87 instruction or EMMS, and survive a merge every
//! arrival of which passed them. `movaps`-style
//! accesses that are not 16-byte aligned leave for the interpreter too,
//! which raises the fault. The cpu pointer stays in x5 across a run of
//! SIMD instructions.
//! Checked SIMD accesses share scalar window/alias permission checks and
//! complete soft and crossing accesses through the owner without publishing
//! or dropping the cache. Memory `pinsrw` and `stmxcsr` use instruction helpers.
//!
//! SSE arithmetic and conversions run in NEON while MXCSR is the default:
//! masked exceptions, round to nearest, no DAZ or FTZ. There the A64
//! result is the x86 one whenever the operation raises nothing but
//! inexact: a quiet NaN operand propagates as the first source, as on x86.
//! Translated code never touches FPSR on the fast path, since its accesses
//! serialize: an instruction runs there once PE is set (PE is sticky, so
//! inexact needs no tracking), or when it cannot be inexact, with no
//! denormal operand, and its values rule out every other exception: no
//! infinite or NaN result, no zero or subnormal product or quotient that
//! zero operands do not explain, no NaN compared or widened, no saturated
//! integer. Anything else takes a slow path that redoes the instruction
//! between a cleared and a read FPSR: invalid, divide by zero, overflow and
//! underflow leave for the interpreter before the destination changes,
//! which also settles x86's negative default NaN and its tininess after
//! rounding; inexact sets PE, and a denormal operand, which A64 does not
//! report without FZ, sets DE from a check of the bits. A false alarm only
//! costs the slow path. Scalar forms ignore upper lanes; NaN signaling
//! follows each x86 predicate. EFLAGS comparisons leave for the
//! interpreter on any NaN.

use iced_x86::{Instruction, Mnemonic, OpKind};

use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit};

use super::decode::{Mem, memory};
use super::flags::Operand;
use super::residency::Entry;
use crate::asm::Label;

use super::{Cold, RUNTIME, Translator, WINDOW, X5, X13, X14, X16, X17, host};

const V0: VReg = VReg::new(0);
const V1: VReg = VReg::new(1);
const V2: VReg = VReg::new(2);

/// An operand of a SIMD instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loc {
    Mmx(u8),
    Xmm(u8),
    Gpr(u8),
    /// `Simd::mem`, of `Simd::width` bytes.
    Mem,
    /// `Simd::imm`, or no operand.
    Imm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `dst = src`, zero-extending a narrower value into a register.
    Move,
    /// Scalar register moves: only the lane of `1 << log2` bytes changes.
    MoveLow(u8),
    /// A selected 16-bit lane, zero-extended into a GPR.
    ExtractWord,
    /// One sign bit per byte of an MMX or XMM source.
    ByteMask,
    InsertWord,
    ShiftBytes(bool),
    StoreControl,
    /// Scalar integer conversion: double precision selector.
    ScalarFromInt(bool),
    /// Scalar to signed dword: double precision and truncation selectors.
    ScalarToInt(bool, bool),
    /// A lane-wise operation on lanes of `1 << log2` bytes.
    Same(Same, u8),
    Logic(Logic),
    /// `pandn`: `!dst & src`.
    AndNot,
    /// Interleaves the low (`Zip1`) or high (`Zip2`) halves.
    Unpack(Permute, u8),
    /// Saturates the lanes of dst then src to lanes of `1 << log2` bytes.
    Pack(Narrowing, u8),
    ShiftImm(ShiftImm, u8),
    /// A shift by the 64-bit count in src.
    ShiftBy(ShiftImm, u8),
    /// The high halves of signed (true) or unsigned 16-bit products.
    MulHigh(bool),
    /// `pmaddwd`.
    MulAdd,
    /// `pshufw` or `pshufd`: lanes of `1 << log2` bytes of src picked by
    /// `imm`.
    Shuffle(u8),
    Shufps,
    Emms,
    /// `addps`-style arithmetic on every lane, or on the low one (`true`).
    Float(Float, bool),
    /// Double-precision arithmetic, scalar when the selector is true.
    Double(Float, bool),
    /// Signed packed dwords to single or double precision (`true`).
    FromInt(bool),
    /// Single to double precision, packed or scalar (`true`).
    Widen(bool),
    /// Scalar double to single precision.
    Narrow,
    /// COMIS/UCOMIS, using the interpreter for NaNs.
    FlagCompare(bool),
    /// CMPPS/CMPSS/CMPPD/CMPSD: scalar and double-precision selectors.
    Compare(bool, bool),
    /// `maxps` (true) or `minps`, or their scalar forms (`.1`).
    MinMax(bool, bool),
    /// `cvtps2pi`, or `cvttps2pi` (true).
    ToInt(bool),
    /// `movlps` or `movhps` (true): a 64-bit half to or from memory.
    Half(bool),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Simd {
    pub kind: Kind,
    pub dst: Loc,
    pub src: Loc,
    pub mem: Mem,
    pub imm: u8,
    /// Bytes a move or a memory operand covers.
    pub width: u8,
    /// A 16-byte memory operand that must be aligned.
    pub aligned: bool,
    /// The helper for forms without native lowering.
    pub helper: u8,
}

impl Simd {
    pub fn memory(&self) -> bool {
        self.dst == Loc::Mem || self.src == Loc::Mem
    }

    /// Whether it is an MMX instruction, with the x87 effects of one.
    pub fn mmx(&self) -> bool {
        matches!(self.dst, Loc::Mmx(_))
            || matches!(self.src, Loc::Mmx(_))
            || self.kind == Kind::Emms
    }

    /// Whether it may leave for the interpreter on its values.
    pub fn floating(&self) -> bool {
        matches!(
            self.kind,
            Kind::Float(..)
                | Kind::Double(..)
                | Kind::ScalarFromInt(_)
                | Kind::ScalarToInt(..)
                | Kind::FromInt(_)
                | Kind::Widen(_)
                | Kind::Narrow
                | Kind::FlagCompare(_)
                | Kind::Compare(..)
                | Kind::MinMax(..)
                | Kind::ToInt(_)
        )
    }

    /// Whether a `floating` instruction can be inexact.
    fn inexact(&self) -> bool {
        matches!(
            self.kind,
            Kind::Float(..)
                | Kind::Double(..)
                | Kind::ScalarFromInt(false)
                | Kind::ScalarToInt(..)
                | Kind::FromInt(false)
                | Kind::Narrow
                | Kind::ToInt(_)
        )
    }

    /// Whether a `floating` instruction reads binary64 operands.
    fn double(&self) -> bool {
        matches!(
            self.kind,
            Kind::Compare(_, true)
                | Kind::Double(..)
                | Kind::FlagCompare(true)
                | Kind::Narrow
                | Kind::ScalarToInt(true, _)
        )
    }

    /// Whether a `floating` instruction reads only the low lane.
    fn scalar(&self) -> bool {
        match self.kind {
            Kind::Float(_, scalar)
            | Kind::Double(_, scalar)
            | Kind::Compare(scalar, _)
            | Kind::MinMax(_, scalar)
            | Kind::Widen(scalar) => scalar,
            Kind::FlagCompare(_) | Kind::Narrow | Kind::ScalarToInt(..) => true,
            _ => false,
        }
    }

    /// Whether the destination is 128 bits wide.
    fn full(&self) -> bool {
        matches!(self.dst, Loc::Xmm(_))
    }

    /// The guards that hold after it continues, given those `before`, as
    /// `Translator::simd` establishes them.
    pub fn guarding(&self, before: Guards) -> Guards {
        let mut after = before;
        if self.mmx() {
            after.ready = self.kind != Kind::Emms;
        }
        if self.floating() {
            after.fp_default = true;
            after.fp_pe |= self.inexact();
        }
        after
    }

    /// The registers it surely reads and those it may write, as
    /// `Vector::dirty` numbers them (`translate::residency`).
    pub fn footprint(&self) -> (u16, u16) {
        let bit = |loc| match loc {
            Loc::Mmx(n) => 1u16 << n,
            Loc::Xmm(n) => 1 << (8 + n),
            _ => 0,
        };
        // As the lowering below reads its destination.
        let reads_dst = match self.kind {
            Kind::Emms | Kind::Move | Kind::ExtractWord | Kind::ByteMask | Kind::StoreControl => {
                false
            }
            Kind::ShiftBytes(_) => self.imm < 16,
            Kind::ShiftImm(op, log2) => {
                let bits = Arr::new(u32::from(log2), self.full()).bits();
                u32::from(self.imm) < bits || op == ShiftImm::Sshr
            }
            Kind::FlagCompare(_) => true,
            _ => !self.floating(),
        };
        let writes = match self.kind {
            Kind::FlagCompare(_) => 0,
            _ => bit(self.dst),
        };
        (
            bit(self.src) | if reads_dst { bit(self.dst) } else { 0 },
            writes,
        )
    }
}

/// The operands of a `floating` instruction in host registers: the source
/// and the inputs it computes from.
#[derive(Clone, Copy, Debug)]
pub struct Operands {
    s: VReg,
    inputs: [Option<VReg>; 2],
}

/// The slow path of a `floating` instruction.
pub struct Slow {
    simd: Simd,
    operands: Operands,
    slow: Label,
    /// Checks a zero or subnormal result, then goes on or takes `slow`.
    tiny: Option<Label>,
    resume: Label,
    bail: Label,
}

const NO_MEM: Mem = Mem {
    base: None,
    index: None,
    scale: 1,
    disp: 0,
    fs: false,
};

/// The `Simd` for `instruction`, whose interpreter helper is `helper`.
pub fn plan(instruction: &Instruction, helper: u8) -> Option<Simd> {
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
    let mnemonic = instruction.mnemonic();
    let (dst, src) = match instruction.op_count() {
        0 => (Loc::Imm, Loc::Imm),
        1 if mnemonic == M::Stmxcsr => (loc(0)?, Loc::Imm),
        2 | 3 => (loc(0)?, loc(1)?),
        _ => return None,
    };
    let kind = match mnemonic {
        M::Emms => Kind::Emms,
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
        M::Cvtdq2ps => Kind::FromInt(false),
        M::Cvtdq2pd => Kind::FromInt(true),
        M::Cvtps2pd => Kind::Widen(false),
        M::Cvtss2sd => Kind::Widen(true),
        M::Cvtsd2ss => Kind::Narrow,
        M::Comiss | M::Ucomiss => Kind::FlagCompare(false),
        M::Comisd | M::Ucomisd => Kind::FlagCompare(true),
        M::Cmpps => Kind::Compare(false, false),
        M::Cmpss => Kind::Compare(true, false),
        M::Cmppd => Kind::Compare(false, true),
        M::Cmpsd => Kind::Compare(true, true),
        M::Maxps => Kind::MinMax(true, false),
        M::Minps => Kind::MinMax(false, false),
        M::Maxss => Kind::MinMax(true, true),
        M::Minss => Kind::MinMax(false, true),
        M::Cvtps2pi => Kind::ToInt(false),
        M::Cvttps2pi => Kind::ToInt(true),
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
        Kind::ExtractWord | Kind::ByteMask => d == 3 && matches!(s, 1 | 2),
        Kind::InsertWord => matches!(d, 1 | 2) && (s == 3 || src == Loc::Mem),
        Kind::StoreControl => dst == Loc::Mem,
        Kind::ShiftBytes(_) => d == 2 && src == Loc::Imm,
        Kind::ScalarFromInt(_) => d == 2 && (s == 3 || src == Loc::Mem),
        Kind::ScalarToInt(..) => d == 3 && (s == 2 || src == Loc::Mem),
        Kind::ToInt(_) => d == 1 && (s == 2 || src == Loc::Mem),
        Kind::Half(_) => d == 2 && src == Loc::Mem || dst == Loc::Mem && s == 2,
        Kind::Float(..)
        | Kind::Double(..)
        | Kind::FromInt(_)
        | Kind::Widen(_)
        | Kind::Narrow
        | Kind::FlagCompare(_)
        | Kind::Compare(..)
        | Kind::MinMax(..) => d == 2 && (s == 2 || src == Loc::Mem),
        Kind::Move => match (d, s) {
            (1 | 2, 0) | (0, 1 | 2) | (1 | 2, 3) | (3, 1 | 2) => true,
            _ => d == s && matches!(d, 1 | 2),
        },
        _ => matches!(d, 1 | 2) && (s == d || src == Loc::Mem || src == Loc::Imm),
    };
    let three = matches!(
        kind,
        Kind::Shuffle(_) | Kind::Shufps | Kind::Compare(..) | Kind::ExtractWord | Kind::InsertWord
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
        memory(instruction)?
    } else {
        NO_MEM
    };
    let size = instruction.memory_size().size();
    let width = if dst == Loc::Mem || src == Loc::Mem {
        size
    } else {
        match mnemonic {
            M::Movd => 4,
            M::Movq | M::Movsd => 8,
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

/// What a unit knows about its SIMD registers at the instruction being
/// emitted.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vector {
    /// MMX registers in d8-d15, by bit.
    mmx: u8,
    /// MMX (bits 0–7) and XMM (bits 8–15) needing publication.
    pub dirty: u16,
    /// XMM registers in v16-v23, by bit.
    xmm: u8,
    /// x5 holds the `CpuState` pointer.
    pub cpu: bool,
    pub guards: Guards,
}

/// Facts about `CpuState` that guards established on every path here.
/// Nothing but helpers, x87 instructions and EMMS changes them, so they
/// survive merges whose arrivals all hold them (`translate::residency`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Guards {
    /// No x87 exception is pending, and TOP and the tags are 0.
    pub ready: bool,
    /// MXCSR's control bits have passed the native SSE guard.
    pub fp_default: bool,
    /// MXCSR PE is set on every path continuing inside this unit.
    pub fp_pe: bool,
}

impl Guards {
    pub const ALL: Self = Self {
        ready: true,
        fp_default: true,
        fp_pe: true,
    };

    /// The guards both hold.
    pub fn meet(self, other: Self) -> Self {
        Self {
            ready: self.ready && other.ready,
            fp_default: self.fp_default && other.fp_default,
            fp_pe: self.fp_pe && other.fp_pe,
        }
    }

    fn within(self, other: Self) -> bool {
        self.meet(other) == self
    }
}

impl Vector {
    /// What a merge's code assumes on arrival.
    pub(super) fn at(entry: Entry) -> Self {
        Self {
            mmx: entry.regs as u8,
            xmm: (entry.regs >> 8) as u8,
            guards: entry.guards,
            ..Self::default()
        }
    }
}

fn vmem(bytes: u8) -> VMem {
    match bytes {
        4 => VMem::S,
        8 => VMem::D,
        16 => VMem::Q,
        _ => unreachable!("no {bytes}-byte SIMD access"),
    }
}

impl Translator<'_> {
    pub(super) fn simd(&mut self, simd: Simd) {
        if self.checking
            && simd.memory()
            && matches!(simd.kind, Kind::InsertWord | Kind::StoreControl)
        {
            self.helper(simd.helper);
            return;
        }
        let mmx = simd.mmx();
        let ready = self.vector.guards.ready;
        if mmx && !ready {
            self.cpu();
            let bail = self.bail();
            self.emit(ldrh(X16, X5, runtime::X87_STATUS));
            self.asm.tbz(true, X16, 7, bail);
        }
        let address = if simd.memory() {
            let address = self.effective(simd.mem);
            if simd.aligned {
                // NZCV holds guest flags.
                let bail = self.bail();
                self.emit(and_imm(W, X17, address, 15));
                self.asm.cbz(W, true, X17, bail);
            }
            address
        } else {
            Reg::ZR
        };
        self.simd_body(simd, address);
        if mmx && simd.kind != Kind::Emms && !ready {
            self.cpu();
            self.emit_all(&[
                ldrh(X17, X5, runtime::X87_STATUS),
                and_imm(W, X17, X17, 0xffff_c7ff),
                strh(X17, X5, runtime::X87_STATUS),
                strh(Reg::ZR, X5, runtime::X87_TAG),
            ]);
            self.vector.guards.ready = true;
        }
    }

    fn simd_body(&mut self, simd: Simd, address: Reg) {
        let full = simd.full();
        match simd.kind {
            Kind::Emms => {
                self.flush_simd();
                self.cpu();
                self.emit_all(&[movz(W, X17, 0xffff, 0), strh(X17, X5, runtime::X87_TAG)]);
                self.vector.guards.ready = false;
                return;
            }
            Kind::Move => return self.simd_move(simd, address),
            Kind::ExtractWord => {
                let Loc::Gpr(index) = simd.dst else {
                    unreachable!()
                };
                let s = self.register(simd.src);
                let lane = u32::from(
                    simd.imm
                        & if matches!(simd.src, Loc::Xmm(_)) {
                            7
                        } else {
                            3
                        },
                );
                self.clobber(Operand::Guest(index));
                self.emit(vumov(1, super::ops::result(index), s, lane));
                self.commit(index);
                return;
            }
            Kind::ByteMask => {
                let Loc::Gpr(index) = simd.dst else {
                    unreachable!()
                };
                let source = self.register(simd.src);
                self.clobber(Operand::Guest(index));
                self.emit_all(&[
                    movz(X, X17, 0x4081, 0),
                    movk(X, X17, 0x1020, 16),
                    movk(X, X17, 0x0408, 32),
                    movk(X, X17, 0x0002, 48),
                ]);
                for lane in 0..if matches!(simd.src, Loc::Xmm(_)) {
                    2
                } else {
                    1
                } {
                    self.emit_all(&[
                        vumov(3, X16, source, lane),
                        and_imm(X, X16, X16, 0x8080808080808080),
                        mul(X, X16, X16, X17),
                        lsr_imm(X, X16, X16, 56),
                    ]);
                    if lane == 0 {
                        self.emit(mov(W, X13, X16));
                    } else {
                        self.emit(orr(W, X13, X13, X16, Shift::Lsl, 8));
                    }
                }
                self.emit(mov(W, super::ops::result(index), X13));
                self.commit(index);
                return;
            }
            Kind::InsertWord => {
                let value = match simd.src {
                    Loc::Gpr(index) => host(index),
                    Loc::Mem => {
                        self.site();
                        self.emit(ldr_narrow(Narrow::Half, X17, WINDOW, address));
                        X17
                    }
                    _ => unreachable!(),
                };
                let d = self.register(simd.dst);
                let lane = u32::from(simd.imm) & if full { 7 } else { 3 };
                self.emit(vins_general(1, d, lane, value));
                return self.written(simd.dst);
            }
            Kind::ShiftBytes(left) => {
                let count = u32::from(simd.imm);
                let d = self.destination(simd.dst, count < 16);
                if count >= 16 {
                    self.emit(vzero(true, d));
                } else if count != 0 {
                    self.emit(vzero(true, V0));
                    self.emit(if left {
                        vext(d, V0, d, 16 - count)
                    } else {
                        vext(d, d, V0, count)
                    });
                }
                return self.written(simd.dst);
            }
            Kind::StoreControl => {
                self.cpu();
                self.emit(ldr(W, X13, X5, runtime::MXCSR));
                self.site();
                self.emit(str_reg(W, X13, WINDOW, address, false));
                return;
            }
            Kind::Half(high) => return self.half(simd, high, address),
            Kind::Float(..)
            | Kind::Double(..)
            | Kind::ScalarFromInt(_)
            | Kind::ScalarToInt(..)
            | Kind::FromInt(_)
            | Kind::Widen(_)
            | Kind::Narrow
            | Kind::FlagCompare(_)
            | Kind::Compare(..)
            | Kind::MinMax(..)
            | Kind::ToInt(_) => {
                return self.floating(simd, address);
            }
            Kind::ShiftImm(op, log2) => {
                let arr = Arr::new(u32::from(log2), full);
                let (bits, count) = (arr.bits(), u32::from(simd.imm));
                let d = self.destination(simd.dst, count < bits || op == ShiftImm::Sshr);
                // A zero count still writes: the alias gets an MMX exponent.
                match op {
                    _ if count == 0 => {}
                    ShiftImm::Sshr => self.emit(vshift(op, arr, d, d, count.min(bits))),
                    _ if count >= bits => self.emit(vzero(full, d)),
                    _ => self.emit(vshift(op, arr, d, d, count)),
                }
                return self.written(simd.dst);
            }
            Kind::ShiftBy(op, log2) => {
                let arr = Arr::new(u32::from(log2), full);
                match simd.src {
                    Loc::Mem => {
                        if self.checking {
                            self.simd_access(vmem(simd.width), V1, address, false);
                            self.emit(fmov_from_v(X, X16, V1));
                        } else {
                            self.site();
                            self.emit(ldr_reg(X, X16, WINDOW, address, false));
                        }
                    }
                    loc => {
                        let s = self.register(loc);
                        self.emit(fmov_from_v(X, X16, s));
                    }
                }
                // Counts past the lane size act as the lane size; NZCV holds
                // guest flags.
                self.emit_all(&[
                    mrs_nzcv(X14),
                    movz(W, X17, arr.bits() as u16, 0),
                    cmp(X, X16, X17, Shift::Lsl, 0),
                    csel(X, X16, X16, X17, Cond::Lo),
                    msr_nzcv(X14),
                ]);
                if op != ShiftImm::Shl {
                    self.emit(sub(X, X16, Reg::ZR, X16, Shift::Lsl, 0));
                }
                let d = self.destination(simd.dst, true);
                let same = if op == ShiftImm::Sshr {
                    Same::Sshl
                } else {
                    Same::Ushl
                };
                self.emit_all(&[vdup(arr, V1, X16), vsame(same, arr, d, d, V1)]);
                return self.written(simd.dst);
            }
            _ => {}
        }
        let s = match simd.src {
            Loc::Mem => {
                self.simd_access(vmem(simd.width), V1, address, false);
                V1
            }
            Loc::Imm => unreachable!("handled above"),
            loc => self.register(loc),
        };
        let d = self.destination(simd.dst, true);
        match simd.kind {
            Kind::MoveLow(log2) => self.emit(vins(u32::from(log2), d, 0, s, 0)),
            Kind::Same(op, log2) => self.emit(vsame(op, Arr::new(u32::from(log2), full), d, d, s)),
            Kind::Logic(op) => self.emit(vlogic(op, full, d, d, s)),
            Kind::AndNot => self.emit(vlogic(Logic::Bic, full, d, s, d)),
            Kind::Unpack(op, log2) => {
                self.emit(vpermute(op, Arr::new(u32::from(log2), full), d, d, s))
            }
            Kind::Pack(op, log2) => {
                let log2 = u32::from(log2);
                if full {
                    self.emit_all(&[
                        vnarrow(op, Arr::new(log2, false), V0, d),
                        vnarrow(op, Arr::new(log2, true), V0, s),
                        vmov(true, d, V0),
                    ]);
                } else {
                    self.emit_all(&[
                        vmov(false, V0, d),
                        vins(3, V0, 1, s, 0),
                        vnarrow(op, Arr::new(log2, false), d, V0),
                    ]);
                }
            }
            Kind::MulHigh(signed) => {
                self.emit(vmull(signed, Arr::H4, V0, d, s));
                if full {
                    self.emit_all(&[
                        vmull(signed, Arr::H8, V2, d, s),
                        vpermute(Permute::Uzp2, Arr::H8, d, V0, V2),
                    ]);
                } else {
                    self.emit(vshrn(Arr::H4, d, V0, 16));
                }
            }
            Kind::MulAdd => {
                self.emit(vmull(true, Arr::H4, V0, d, s));
                if full {
                    self.emit_all(&[
                        vmull(true, Arr::H8, V2, d, s),
                        vsame(Same::Addp, Arr::S4, d, V0, V2),
                    ]);
                } else {
                    self.emit(vsame(Same::Addp, Arr::S4, d, V0, V0));
                }
            }
            Kind::Shuffle(log2) => {
                let log2 = u32::from(log2);
                let lane = u32::from(simd.imm & 3);
                if u32::from(simd.imm) == lane * 0x55 {
                    self.emit(vdup_lane(Arr::new(log2, full), d, s, lane));
                    return self.written(simd.dst);
                }
                for lane in 0..4 {
                    let from = u32::from(simd.imm) >> (2 * lane) & 3;
                    self.emit(vins(log2, V0, lane, s, from));
                }
                self.emit(vmov(full, d, V0));
            }
            Kind::Shufps => {
                for lane in 0..4 {
                    let from = u32::from(simd.imm) >> (2 * lane) & 3;
                    self.emit(vins(2, V0, lane, if lane < 2 { d } else { s }, from));
                }
                self.emit(vmov(true, d, V0));
            }
            Kind::Move
            | Kind::InsertWord
            | Kind::ShiftBytes(_)
            | Kind::StoreControl
            | Kind::ExtractWord
            | Kind::ByteMask
            | Kind::ShiftImm(..)
            | Kind::ShiftBy(..)
            | Kind::Emms
            | Kind::Float(..)
            | Kind::Double(..)
            | Kind::ScalarFromInt(_)
            | Kind::ScalarToInt(..)
            | Kind::FromInt(_)
            | Kind::Widen(_)
            | Kind::Narrow
            | Kind::FlagCompare(_)
            | Kind::Compare(..)
            | Kind::MinMax(..)
            | Kind::ToInt(_)
            | Kind::Half(_) => unreachable!(),
        }
        self.written(simd.dst);
    }

    fn simd_move(&mut self, simd: Simd, address: Reg) {
        let size = vmem(simd.width);
        match (simd.dst, simd.src) {
            (Loc::Mem, src) => {
                let s = self.register(src);
                self.simd_access(size, s, address, true);
            }
            (Loc::Gpr(index), src) => {
                let s = self.register(src);
                self.clobber(Operand::Guest(index));
                self.emit(fmov_from_v(W, super::ops::result(index), s));
                self.commit(index);
            }
            (dst, Loc::Mem) => {
                let d = self.destination(dst, false);
                self.simd_access(size, d, address, false);
                self.written(dst);
            }
            (dst, Loc::Gpr(index)) => {
                let d = self.destination(dst, false);
                self.emit(fmov_to_v(W, d, host(index)));
                self.written(dst);
            }
            (dst, src) => {
                let s = self.register(src);
                let d = self.destination(dst, false);
                self.emit(vmov(size == VMem::Q, d, s));
                self.written(dst);
            }
        }
    }

    fn simd_access(&mut self, size: VMem, value: VReg, address: Reg, store: bool) {
        self.vector_access(size, value, address, store);
    }

    /// `movlps` or `movhps`: one 64-bit lane of an XMM register.
    fn half(&mut self, simd: Simd, high: bool, address: Reg) {
        let lane = u32::from(high);
        if simd.dst == Loc::Mem {
            let s = self.register(simd.src);
            let value = if high {
                self.emit(vins(3, V1, 0, s, 1));
                V1
            } else {
                s
            };
            self.simd_access(VMem::D, value, address, true);
        } else {
            let d = self.register(simd.dst);
            self.simd_access(VMem::D, V1, address, false);
            self.emit(vins(3, d, lane, V1, 0));
            self.written(simd.dst);
        }
    }

    /// Arithmetic, conversions and binary32/64 compares under the default
    /// MXCSR. The fast path never touches FPSR: it runs once PE is set, or
    /// for operations that cannot be inexact, with no denormal operand, and
    /// sends values that could raise anything else to the slow path, which
    /// redoes the operation between a cleared and a read FPSR and leaves
    /// for the interpreter on any exception but inexact. Uses x13, x14 and
    /// v0-v6.
    fn floating(&mut self, simd: Simd, address: Reg) {
        self.cpu();
        let bail = self.bail();
        let (slow, resume) = (self.asm.label(), self.asm.label());
        let inexact = simd.inexact();
        let first_pe = inexact && !self.vector.guards.fp_pe;
        if inexact && !self.vector.guards.fp_pe || !self.vector.guards.fp_default {
            self.emit(ldr(W, X13, X5, runtime::MXCSR));
        }
        // Masked exceptions, nearest, no DAZ or FTZ; flags may be set.
        if !self.vector.guards.fp_default {
            self.emit_all(&[and_imm(W, X17, X13, 0xffc0), eor_imm(W, X17, X17, 0x1f80)]);
            self.asm.cbz(W, true, X17, bail);
            self.vector.guards.fp_default = true;
        }
        let s = match simd.src {
            Loc::Mem => {
                self.simd_access(vmem(simd.width), V1, address, false);
                V1
            }
            Loc::Gpr(index) => {
                self.emit(fmov_to_v(W, V1, host(index)));
                V1
            }
            loc => self.register(loc),
        };
        let inputs = match simd.kind {
            Kind::Float(..)
            | Kind::Double(..)
            | Kind::Compare(..)
            | Kind::MinMax(..)
            | Kind::FlagCompare(_) => [Some(self.register(simd.dst)), Some(s)],
            _ => [None, Some(s)],
        };
        let operands = Operands { s, inputs };
        if inexact && !self.vector.guards.fp_pe {
            self.asm.tbz(false, X13, 5, slow);
            self.vector.guards.fp_pe = true;
        }
        self.denormals(simd, operands, Some(slow));
        let tiny = self.floating_fast(simd, operands, slow);
        self.asm.bind(resume);
        self.push_cold(Cold::Floating(Slow {
            simd,
            operands,
            slow,
            tiny,
            resume,
            bail,
        }));
        if matches!(simd.kind, Kind::ScalarToInt(..)) {
            let Loc::Gpr(index) = simd.dst else {
                unreachable!()
            };
            self.clobber(Operand::Guest(index));
            self.emit(fmov_from_v(W, super::ops::result(index), V0));
            self.commit(index);
            self.pe_ready(first_pe);
            return;
        }
        if matches!(simd.kind, Kind::FlagCompare(_)) {
            self.emit_all(&[
                fmov_from_v(W, X16, V0),
                fmov_from_v(W, X17, V2),
                and_imm(W, X16, X16, 1 << 30),
                mvn(W, X17, X17),
                and_imm(W, X17, X17, 1 << 29),
                orr(W, X16, X16, X17, Shift::Lsl, 0),
                msr_nzcv(X16),
                movz(W, super::X10, 1, 0),
                movz(W, super::X11, 0, 0),
            ]);
            self.state = super::State::CANONICAL;
            return;
        }
        let d = self.destination(
            simd.dst,
            matches!(
                simd.kind,
                Kind::Widen(true) | Kind::Narrow | Kind::ScalarFromInt(_)
            ),
        );
        self.emit(match simd.kind {
            Kind::Compare(true, double) => vins(if double { 3 } else { 2 }, d, 0, V0, 0),
            Kind::ScalarFromInt(false)
            | Kind::Float(_, true)
            | Kind::MinMax(_, true)
            | Kind::Narrow => vins(2, d, 0, V0, 0),
            Kind::ScalarFromInt(true) | Kind::Double(_, true) | Kind::Widen(true) => {
                vins(3, d, 0, V0, 0)
            }
            Kind::ToInt(_) => vmov(false, d, V0),
            _ => vmov(true, d, V0),
        });
        self.written(simd.dst);
        self.pe_ready(first_pe);
    }

    /// An exact slow operation can leave PE clear; finish it, then leave
    /// before entering code that assumes the sticky bit has been established.
    fn pe_ready(&mut self, first: bool) {
        if !first {
            return;
        }
        let leave = self.asm.label();
        self.emit(ldr(W, X13, X5, runtime::MXCSR));
        self.asm.tbz(false, X13, 5, leave);
        self.push_cold(Cold::Leave {
            label: leave,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund: self.trace.unused(self.index),
            exit: crate::runtime::Exit::Dispatch,
            eip: self.trace.insts[self.index].next_pc,
            link: true,
            x87: None,
        });
    }

    /// The fast path of `floating` into v0 (and v2), with the checks around
    /// it; the label of the cold check of a zero or subnormal result that
    /// zero operands may make exact.
    fn floating_fast(&mut self, simd: Simd, operands: Operands, slow: Label) -> Option<Label> {
        let (v3, v4, v5, v6) = (VReg::new(3), VReg::new(4), VReg::new(5), VReg::new(6));
        let double = simd.double();
        let scalar = simd.scalar();
        let [a, _] = operands.inputs;
        let s = operands.s;
        match simd.kind {
            // Rare: always exact through FPSR.
            Kind::ToInt(_) => {
                self.asm.b(slow);
                return None;
            }
            // Any NaN may raise IE: the slow path tells.
            Kind::MinMax(..)
            | Kind::Compare(..)
            | Kind::FlagCompare(_)
            | Kind::Widen(_)
            | Kind::ScalarToInt(..) => {
                let inputs: Vec<VReg> = operands.inputs.into_iter().flatten().collect();
                if simd.kind == Kind::Widen(false) {
                    // Two binary32 lanes in the low half.
                    self.emit_all(&[
                        vfloat(Float::Fcmeq, false, v3, s, s),
                        fmov_from_v(X, X16, v3),
                        add_imm(X, X16, X16, 1, false),
                    ]);
                    self.asm.cbz(X, true, X16, slow);
                } else {
                    for (n, &input) in inputs.iter().enumerate() {
                        let target = if n == 0 { v3 } else { v4 };
                        self.emit(vcompare(Float::Fcmeq, scalar, double, target, input, input));
                    }
                    if inputs.len() == 2 {
                        self.emit(vlogic(Logic::And, true, v3, v3, v4));
                    }
                    if scalar {
                        self.emit(fmov_from_v(X, X16, v3));
                        self.asm.cbz(W, false, X16, slow);
                    } else {
                        self.emit_all(&[
                            vnot(true, v3, v3),
                            umaxv(v3, v3),
                            fmov_from_v(W, X16, v3),
                        ]);
                        self.asm.cbz(W, true, X16, slow);
                    }
                }
            }
            _ => {}
        }
        self.floating_op(simd, operands);
        match simd.kind {
            Kind::Float(op, _) | Kind::Double(op, _) if scalar => {
                let sum = matches!(op, Float::Fadd | Float::Fsub);
                let tiny = self.result_bits(double, V0, !sum);
                if sum {
                    // A tiny sum is exact: only infinities and NaNs.
                    self.asm.tbz(true, X17, if double { 11 } else { 8 }, slow);
                    return None;
                }
                let label = self.asm.label();
                self.asm.cbz(W, false, tiny, label);
                Some(label)
            }
            Kind::Float(op, _) | Kind::Double(op, _) => {
                let arr = if double { Arr::D2 } else { Arr::S4 };
                // v6 lanes with an infinity or NaN, v5 zeros and subnormals.
                if double {
                    self.emit(movz(X, X16, 0x7ff0, 48));
                } else {
                    self.emit(movz(W, X16, 0x7f80, 16));
                }
                self.emit_all(&[
                    vdup(arr, v4, X16),
                    vlogic(Logic::And, true, v5, V0, v4),
                    vsame(Same::Cmeq, arr, v6, v5, v4),
                ]);
                if !matches!(op, Float::Fadd | Float::Fsub) {
                    // A product with a zero factor, or a zero dividend, is
                    // an exact zero.
                    self.emit_all(&[
                        vzero(true, V2),
                        vsame(Same::Cmeq, arr, v5, v5, V2),
                        vcompare(Float::Fcmeq, false, double, v3, a.unwrap(), V2),
                    ]);
                    if op == Float::Fmul {
                        self.emit_all(&[
                            vcompare(Float::Fcmeq, false, double, v4, s, V2),
                            vlogic(Logic::Orr, true, v3, v3, v4),
                        ]);
                    }
                    self.emit_all(&[
                        vlogic(Logic::Bic, true, v5, v5, v3),
                        vlogic(Logic::Orr, true, v6, v6, v5),
                    ]);
                }
                self.emit_all(&[umaxv(v6, v6), fmov_from_v(W, X16, v6)]);
                self.asm.cbz(W, true, X16, slow);
                None
            }
            Kind::Narrow => {
                let tiny = self.result_bits(false, V0, true);
                let label = self.asm.label();
                self.asm.cbz(W, false, tiny, label);
                Some(label)
            }
            Kind::ScalarToInt(..) => {
                // fcvtzs and fcvtns saturate: both limits leave, with the
                // NaNs already gone.
                self.emit_all(&[
                    eor(W, X17, X16, X16, Shift::Asr, 31),
                    add_imm(W, X17, X17, 1, false),
                ]);
                self.asm.tbz(true, X17, 31, slow);
                None
            }
            _ => None,
        }
    }

    /// The biased exponent + 1 of the scalar result in `v`, in w17, and if
    /// `tiny`, in w14 a register that is zero for a zero, subnormal,
    /// infinite or NaN result; uses x16.
    fn result_bits(&mut self, double: bool, v: VReg, tiny: bool) -> Reg {
        let (size, fraction, exponent) = if double { (X, 52, 11) } else { (W, 23, 8) };
        self.emit_all(&[
            fmov_from_v(size, X16, v),
            ubfx(size, X17, X16, fraction, exponent),
            add_imm(W, X17, X17, 1, false),
        ]);
        if tiny {
            self.emit(and_imm(W, X14, X17, (1 << exponent) - 2));
        }
        X14
    }

    /// Branches to `to`, or sets DE in w13 without one, when an operand
    /// `floating` reads is a nonzero subnormal. Scalar upper lanes are
    /// ignored. Uses x16, x17 and v0 and v4-v6.
    fn denormals(&mut self, simd: Simd, operands: Operands, to: Option<Label>) {
        if matches!(simd.kind, Kind::FromInt(_) | Kind::ScalarFromInt(_)) {
            return;
        }
        let (v4, v5, v6) = (VReg::new(4), VReg::new(5), VReg::new(6));
        let double = simd.double();
        let inputs = operands.inputs;
        // After shifting away the sign, a nonzero subnormal is below 2^24
        // (binary32) or 2^53 (binary64).
        if simd.scalar() {
            for input in inputs.into_iter().flatten() {
                let clean = self.asm.label();
                let size = if double { X } else { W };
                let fraction = if double { 52 } else { 23 };
                let exponent = if double { 11 } else { 8 };
                self.emit_all(&[
                    fmov_from_v(size, X16, input),
                    ubfx(size, X17, X16, fraction, exponent),
                ]);
                self.asm.cbz(size, true, X17, clean);
                self.emit(lsl_imm(size, X16, X16, exponent + 1));
                match to {
                    Some(to) => self.asm.cbz(size, true, X16, to),
                    None => {
                        self.asm.cbz(size, false, X16, clean);
                        self.emit(orr_imm(W, X13, X13, 2));
                    }
                }
                self.asm.bind(clean);
            }
            return;
        }
        let arr = match simd.kind {
            Kind::ToInt(_) | Kind::Widen(false) => Arr::S2,
            _ if double => Arr::D2,
            _ => Arr::S4,
        };
        self.emit_all(&[
            movz(X, X16, 1, 0),
            vdup(arr, v4, X16),
            movz(W, X16, 0xffff, 0),
        ]);
        if double {
            self.emit_all(&[
                movk(X, X16, 0xffff, 16),
                movk(X, X16, 0xffff, 32),
                movk(X, X16, 0x001f, 48),
            ]);
        } else {
            self.emit(movk(W, X16, 0x00ff, 16));
        }
        self.emit(vdup(arr, v5, X16));
        for (n, input) in inputs.into_iter().flatten().enumerate() {
            let target = if n == 0 { v6 } else { V0 };
            self.emit_all(&[
                vshift(ShiftImm::Shl, arr, target, input, 1),
                vsame(Same::Sub, arr, target, target, v4),
                vsame(Same::Uqsub, arr, target, v5, target),
            ]);
            if n != 0 {
                self.emit(vlogic(Logic::Orr, true, v6, v6, V0));
            }
        }
        // The 64-bit forms clear the high half.
        if double {
            self.emit_all(&[
                vumov(3, X14, v6, 0),
                vumov(3, X17, v6, 1),
                orr(X, X17, X17, X14, Shift::Lsl, 0),
            ]);
        } else {
            self.emit_all(&[umaxv(v6, v6), fmov_from_v(W, X17, v6)]);
        }
        match to {
            Some(to) => self.asm.cbz(X, true, X17, to),
            None => {
                let clean = self.asm.label();
                self.asm.cbz(X, false, X17, clean);
                self.emit(orr_imm(W, X13, X13, 2));
                self.asm.bind(clean);
            }
        }
    }

    /// The operation of `floating`, into v0 (v2 also for EFLAGS compares,
    /// x16 also for integer results).
    fn floating_op(&mut self, simd: Simd, operands: Operands) {
        let s = operands.s;
        let d = operands.inputs[0];
        match simd.kind {
            Kind::ScalarFromInt(double) => {
                self.emit_all(&[fmov_from_v(W, X16, s), scvtf(W, V0, X16)]);
                if !double {
                    self.emit(unary(Unary::Narrow, V0, V0));
                }
            }
            Kind::ScalarToInt(double, truncate) => {
                let source = if double {
                    s
                } else {
                    self.emit(unary(Unary::Widen, V0, s));
                    V0
                };
                self.emit_all(&[fcvt_int(W, truncate, X16, source), fmov_to_v(W, V0, X16)]);
            }
            Kind::Double(op, true) => self.emit(dfloat(op, V0, d.unwrap(), s)),
            Kind::Double(op, false) => self.emit(vfloat(op, true, V0, d.unwrap(), s) | 1 << 22),
            Kind::FromInt(false) => self.emit(vfromint(V0, s)),
            Kind::FromInt(true) => self.emit_all(&[
                vumov(2, X16, s, 0),
                scvtf(W, V0, X16),
                vumov(2, X16, s, 1),
                scvtf(W, V2, X16),
                vins(3, V0, 1, V2, 0),
            ]),
            Kind::Widen(false) => self.emit(vwiden(V0, s)),
            Kind::Widen(true) => self.emit(unary(Unary::Widen, V0, s)),
            Kind::Narrow => self.emit(unary(Unary::Narrow, V0, s)),
            Kind::FlagCompare(double) => {
                let d = d.unwrap();
                // Relational comparisons send every NaN to the original helper,
                // which distinguishes COMIS from UCOMIS exception behavior.
                self.emit_all(&[
                    vcompare(Float::Fcmeq, true, double, V0, d, s),
                    vcompare(Float::Fcmgt, true, double, V2, s, d),
                ]);
            }
            Kind::Float(op, false) => self.emit(vfloat(op, true, V0, d.unwrap(), s)),
            Kind::Float(op, true) => self.emit(sfloat(op, V0, d.unwrap(), s)),
            Kind::Compare(scalar, double) => {
                let d = d.unwrap();
                // ARM equality is quiet for QNaN; relational comparisons signal.
                // x86 negated predicates include unordered lanes, so invert the
                // corresponding ordered test rather than reverse its operands.
                match simd.imm & 7 {
                    0 | 4 => self.emit(vcompare(Float::Fcmeq, scalar, double, V0, d, s)),
                    1 | 5 => self.emit(vcompare(Float::Fcmgt, scalar, double, V0, s, d)),
                    2 | 6 => self.emit(vcompare(Float::Fcmge, scalar, double, V0, s, d)),
                    3 | 7 => self.emit_all(&[
                        vcompare(Float::Fcmeq, scalar, double, V0, d, d),
                        vcompare(Float::Fcmeq, scalar, double, V2, s, s),
                        vlogic(Logic::And, true, V0, V0, V2),
                    ]),
                    _ => unreachable!(),
                }
                if matches!(simd.imm & 7, 3..=6) {
                    self.emit(vnot(true, V0, V0));
                }
            }
            Kind::MinMax(max, scalar) => {
                let d = d.unwrap();
                // The first source where it compares above (max) or
                // below (min) the second, else the second; any NaN is
                // invalid for the compare.
                let (a, b) = if max { (d, s) } else { (s, d) };
                self.emit_all(&[
                    if scalar {
                        sfcmgt(V0, a, b)
                    } else {
                        vfloat(Float::Fcmgt, true, V0, a, b)
                    },
                    vlogic(Logic::Bsl, true, V0, d, s),
                ]);
            }
            Kind::ToInt(truncate) => self.emit(vtoint(truncate, false, V0, s)),
            _ => unreachable!(),
        }
    }

    /// The slow path of a `floating` instruction, and its tiny-result check.
    pub(super) fn floating_cold(&mut self, cold: Slow) {
        let Slow {
            simd,
            operands,
            slow,
            tiny,
            resume,
            bail,
        } = cold;
        if let Some(tiny) = tiny {
            self.asm.bind(tiny);
            let double = simd.double();
            self.asm.tbz(true, X17, if double { 11 } else { 8 }, slow);
            let zeros = match simd.kind {
                Kind::Float(Float::Fmul, _) | Kind::Double(Float::Fmul, _) => operands.inputs,
                Kind::Narrow => [Some(operands.s), None],
                _ => [operands.inputs[0], None],
            };
            for zero in zeros.into_iter().flatten() {
                let size = if double { X } else { W };
                self.emit_all(&[fmov_from_v(size, X16, zero), lsl_imm(size, X16, X16, 1)]);
                self.asm.cbz(size, false, X16, resume);
            }
        }
        self.asm.bind(slow);
        self.remark(crate::jit_map::Part::FpStatus);
        self.emit(ldr(W, X13, X5, runtime::MXCSR));
        self.denormals(simd, operands, None);
        self.emit(msr_fpsr(Reg::ZR));
        self.floating_op(simd, operands);
        // Anything but inexact leaves; inexact sets PE.
        self.emit_all(&[mrs_fpsr(X17), and_imm(X, X14, X17, 0xf)]);
        self.asm.cbz(X, true, X14, bail);
        self.emit_all(&[
            ubfx(W, X17, X17, 4, 1),
            orr(W, X13, X13, X17, Shift::Lsl, 5),
            str(W, X13, X5, runtime::MXCSR),
        ]);
        self.asm.b(resume);
    }

    /// The host register of an MMX or XMM operand, loaded if the unit has
    /// not yet.
    fn register(&mut self, loc: Loc) -> VReg {
        let (bit, v, size, offset) = match loc {
            Loc::Mmx(n) => (
                &mut self.vector.mmx,
                VReg::new(8 + n),
                VMem::D,
                runtime::MMX + 8 * u32::from(n),
            ),
            Loc::Xmm(n) => (
                &mut self.vector.xmm,
                VReg::new(16 + n),
                VMem::Q,
                runtime::XMM + 16 * u32::from(n),
            ),
            _ => unreachable!("not a SIMD register"),
        };
        let mask = 1 << (v.number() & 7);
        if *bit & mask == 0 {
            *bit |= mask;
            self.cpu();
            self.at_cpu(true, size, v, offset);
        }
        v
    }

    /// The host register of a destination register, holding its value if
    /// `read`.
    fn destination(&mut self, loc: Loc, read: bool) -> VReg {
        if read {
            return self.register(loc);
        }
        match loc {
            Loc::Mmx(n) => VReg::new(8 + n),
            Loc::Xmm(n) => VReg::new(16 + n),
            _ => unreachable!("not a SIMD register"),
        }
    }

    /// Defers the write until an edge or observer publishes it.
    fn written(&mut self, loc: Loc) {
        match loc {
            Loc::Mmx(n) => {
                self.vector.mmx |= 1 << n;
                self.vector.dirty |= 1 << n;
            }
            Loc::Xmm(n) => {
                self.vector.xmm |= 1 << n;
                self.vector.dirty |= 1 << (8 + n);
            }
            _ => unreachable!("not a SIMD register"),
        }
    }

    /// Whether the cache holds every register and guard of `entry`, a
    /// merge's entry (`translate::residency`).
    pub(super) fn holds(&self, entry: Entry) -> bool {
        let held = u16::from(self.vector.mmx) | u16::from(self.vector.xmm) << 8;
        entry.regs & !held == 0 && entry.guards.within(self.vector.guards)
    }

    /// Arrives at a merge whose entry is `entry`: publishes, loads what
    /// the cache lacks of it, and forgets the rest.
    pub(super) fn coerce(&mut self, entry: Entry) {
        // Guards cannot be re-established here; the analysis must agree.
        assert!(
            entry.guards.within(self.vector.guards),
            "an arrival at {:#x} lacks guards",
            self.trace.insts[self.index].pc
        );
        self.flush_simd();
        for n in 0..8 {
            if entry.regs & (1 << n) != 0 {
                self.register(Loc::Mmx(n));
            }
            if entry.regs & (1 << (8 + n)) != 0 {
                self.register(Loc::Xmm(n));
            }
        }
        self.vector = Vector::at(entry);
    }

    /// Publishes the SIMD prefix without changing flags, addresses, x5 or SIMD
    /// temporaries. x0/x3/x4 are free at control and observer boundaries.
    pub(super) fn flush_simd(&mut self) {
        let dirty = self.vector.dirty;
        if dirty == 0 {
            return;
        }
        self.emit_all(&[
            ldr(X, super::X3, RUNTIME, runtime::CPU_STATE),
            movz(W, super::X0, 0xffff, 0),
        ]);
        for n in 0..8 {
            if dirty & (1 << n) == 0 {
                continue;
            }
            let v = VReg::new(8 + n);
            let x87 = runtime::X87_REGS + 10 * u32::from(n);
            self.emit(vstr(VMem::D, v, super::X3, runtime::MMX + 8 * u32::from(n)));
            self.emit(add_imm(X, super::X4, super::X3, x87, false));
            self.emit(vstr(VMem::D, v, super::X4, 0));
            self.emit(strh(super::X0, super::X4, 8));
        }
        if dirty >> 8 != 0 {
            self.emit(add_imm(X, super::X4, super::X3, runtime::XMM, false));
        }
        for n in 0..8 {
            if dirty & (1 << (8 + n)) != 0 {
                self.emit(vstr(
                    VMem::Q,
                    VReg::new(16 + n),
                    super::X4,
                    16 * u32::from(n),
                ));
            }
        }
        self.vector.dirty = 0;
    }

    /// Loads the `CpuState` pointer into x5 unless it is there.
    pub(super) fn cpu(&mut self) {
        if !self.vector.cpu {
            self.emit(ldr(X, X5, RUNTIME, runtime::CPU_STATE));
            self.vector.cpu = true;
        }
    }

    /// Loads or stores `v` at `offset` into `CpuState`; uses x17.
    fn at_cpu(&mut self, load: bool, size: VMem, v: VReg, offset: u32) {
        let bytes = size.bytes();
        let word = if offset.is_multiple_of(bytes) && offset / bytes < 4096 {
            vldr(size, v, X5, offset)
        } else if offset < 256 {
            vldur(size, v, X5, offset as i32)
        } else {
            self.emit(add_imm(X, X17, X5, offset, false));
            vldr(size, v, X17, 0)
        };
        self.emit(if load { word } else { word & !(1 << 22) });
    }

    /// A label that leaves for the interpreter to run the instruction.
    fn bail(&mut self) -> crate::asm::Label {
        let label = self.asm.label();
        let inst = self.trace.insts[self.index];
        self.push_cold(Cold::Leave {
            label,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund: self.trace.owed(self.index),
            exit: Exit::Interpret,
            eip: inst.pc,
            link: false,
            x87: None,
        });
        label
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

    // spec: A64F-FP-4
    #[test]
    fn mixed_mmx_scaling_loop_instruction_count() {
        use crate::translate::{Features, Stubs, Workspace, translate};
        let mut a = CodeAssembler::new(32).unwrap();
        let mut repeat = a.create_label();
        a.set_label(&mut repeat).unwrap();
        a.pxor(mm6, mm6).unwrap();
        a.movq(mm7, mm5).unwrap();
        for _ in 0..4 {
            a.movd(mm0, dword_ptr(esi)).unwrap();
            a.movd(mm1, dword_ptr(esi + 4)).unwrap();
            a.punpcklbw(mm0, mm6).unwrap();
            a.punpcklbw(mm1, mm6).unwrap();
            a.pmullw(mm0, mm4).unwrap();
            a.paddw(mm0, mm1).unwrap();
            a.psrlw(mm0, 8).unwrap();
            a.packuswb(mm0, mm7).unwrap();
            a.movd(dword_ptr(edi), mm0).unwrap();
            a.add(esi, 4).unwrap();
            a.add(edi, 4).unwrap();
            a.add(eax, edx).unwrap();
            a.shr(eax, 1).unwrap();
        }
        a.dec(ecx).unwrap();
        a.jnz(repeat).unwrap();
        let code = a.assemble(0x10000).unwrap();
        let mut memory = adv_cpu_core::Memory::new(0x10000, 4096).unwrap();
        memory.protect(0x10000, 4096, 7).unwrap();
        memory.write(0x10000, &code).unwrap();
        let mut work = Workspace::default();
        translate(
            &mut work,
            &mut memory,
            0x10000,
            &[0x10000 + code.len() as u32],
            &|_| false,
            0,
            Features { flagm: false },
            0,
            Stubs {
                call: 0,
                exit: 0,
                dispatch: 0,
                lookup: 0,
                lookup_slow: 0,
                ret_miss: 0,
                access: 0,
                interpret: 0,
                apply: 0,
                fault: 0,
            },
            &|_, _| None,
            &|_| None,
            &|_, _| Vec::new(),
            0x037f,
        );
        println!(
            "mixed MMX loop: {} guest instructions, {} A64 instructions",
            work.unit.trace.len(),
            work.words().len()
        );
        assert!(
            work.unit
                .trace
                .insts
                .iter()
                .all(|i| !matches!(i.op, super::super::decode::Op::Helper(_)))
        );
    }

    #[test]
    fn plans_follow_the_operand_classes() {
        let movd = plan(&decode(|a| a.movd(mm1, eax).unwrap()), 0).unwrap();
        assert_eq!(
            (movd.kind, movd.dst, movd.src),
            (Kind::Move, Loc::Mmx(1), Loc::Gpr(0))
        );
        let load = plan(
            &decode(|a| a.movaps(xmm2, xmmword_ptr(esi + 16)).unwrap()),
            3,
        )
        .unwrap();
        assert!(load.aligned && load.width == 16 && load.helper == 3);
        let unaligned = plan(&decode(|a| a.movups(xmm2, xmmword_ptr(esi)).unwrap()), 0).unwrap();
        assert!(!unaligned.aligned);
        let shift = plan(&decode(|a| a.psrlw(mm3, 4).unwrap()), 0).unwrap();
        assert_eq!(
            (shift.kind, shift.imm),
            (Kind::ShiftImm(ShiftImm::Ushr, 1), 4)
        );
        let by = plan(&decode(|a| a.psraw(xmm1, xmm2).unwrap()), 0).unwrap();
        assert_eq!(by.kind, Kind::ShiftBy(ShiftImm::Sshr, 1));
        let low = plan(&decode(|a| a.movss(xmm1, xmm2).unwrap()), 0).unwrap();
        assert_eq!(low.kind, Kind::MoveLow(2));
        assert!(plan(&decode(|a| a.movq2dq(xmm1, mm2).unwrap()), 0).is_none());
        let add = plan(&decode(|a| a.addss(xmm1, dword_ptr(esi)).unwrap()), 0).unwrap();
        assert_eq!((add.kind, add.width), (Kind::Float(Float::Fadd, true), 4));
        assert!(add.floating());
        let convert = plan(&decode(|a| a.cvttps2pi(mm1, xmm2).unwrap()), 0).unwrap();
        assert!(convert.mmx() && convert.floating());
        assert!(plan(&decode(|a| a.sqrtps(xmm1, xmm2).unwrap()), 0).is_none());
        assert!(plan(&decode(|a| a.emms().unwrap()), 0).unwrap().mmx());
    }
}
