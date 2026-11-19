//! MMX and SSE instructions in NEON (A64-REG-5, A64F-FP-4, A64F-FP-5).
//!
//! A unit caches MMX registers in d8-d15 and XMM registers in v16-v23.
//! A register is loaded from `CpuState` when the unit first reads it.
//! Writes mark registers dirty. Exits and host calls publish dirty SIMD,
//! and x87 and EMMS publish the MMX aliases they observe. Faults recover the
//! dirty subset from the captured register state before replay. A merge
//! holds the registers and facts its arrivals agree on; incoming edges
//! publish or reload the differences. Tier-1 entries check MXCSR through
//! their version chain. MMX loads refresh the x87 alias as needed.
//!
//! Value calls keep unaffected cache registers live. Checked accesses
//! preserve v0-v7, v16-v23 and the x87 pair registers; the host ABI
//! preserves the MMX halves.
//!
//! An MMX instruction without the readiness fact checks for pending #MF,
//! then clears TOP and tags as part of completing the instruction. An
//! unaligned `movaps`-class access raises #GP before touching memory.
//! The CPU pointer stays in x5 across a run of SIMD instructions.
//! Checked SIMD accesses share scalar window/alias permission checks and
//! complete soft and crossing accesses through the owner without publishing
//! or dropping the cache, including memory `pinsrw` and `stmxcsr`.
//!
//! Packed float-to-integer conversions handle every MXCSR rounding mode,
//! DAZ, sticky exceptions and integer indefinite before publishing their result.
//! Other SSE arithmetic and conversions normally require default MXCSR:
//! masked exceptions, round to nearest, no DAZ or FTZ. Binary32 adds,
//! multiplies and packed integer-to-single conversions also accept directed
//! rounding, with FPCR restored before continuing. The A64
//! result is the x86 one whenever the operation raises nothing but
//! inexact: a quiet NaN operand propagates as the first source, as on x86.
//! Translated code never touches FPSR on the fast path, since its accesses
//! serialize: an instruction runs there once PE is set (PE is sticky, so
//! inexact needs no tracking), or when it cannot be inexact, with no
//! denormal operand, and its values rule out every other exception: no
//! infinite or NaN result, no zero or subnormal product or quotient that
//! zero operands do not explain, no product, quotient or narrowing in the
//! min normal binade, which rounding up may have reached from a tiny value,
//! no NaN compared or widened, no saturated integer. Anything else takes a
//! slow path that redoes the instruction between a cleared and a read FPSR:
//! invalid, divide by zero, overflow and underflow use core value semantics
//! for x86's default NaN, rounding and exceptions. Inexact sets PE,
//! and a denormal operand, which A64 does not report without FZ, sets DE
//! from a check of the bits. A false alarm only costs the slow path. Scalar
//! forms ignore upper lanes; NaN signaling follows each x86 predicate.
//! EFLAGS comparisons use the value entry on a NaN.

use super::fp_class;

#[cfg(test)]
use super::front::plans::simd as plan;
#[cfg(test)]
use iced_x86::Instruction;

use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit};

use super::decode::Mem;
use super::flags::Operand;
use super::residency::Entry;
use crate::asm::Label;
use crate::jit_map::Part;

use super::{Cold, RUNTIME, Translator, X5, X6, X13, X14, X16, X17, host};

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
    MoveHalf(bool),
    MaskMove,
    PackedToInt(bool, bool),
    SignMask(bool),
    ShuffleWords(bool),
    Shufpd,
    MulDwords,
    SadBytes,
    /// A selected 16-bit lane, zero-extended into a GPR.
    ExtractWord,
    /// One sign bit per byte of an MMX or XMM source.
    ByteMask,
    InsertWord,
    ShiftBytes(bool),
    StoreControl,
    LoadControl,
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
    Narrow(bool),
    Approximate(bool, bool),
    /// COMIS/UCOMIS, using core value semantics for NaNs: double precision,
    /// and whether a quiet NaN also raises invalid (COMIS).
    FlagCompare(bool, bool),
    /// CMPPS/CMPSS/CMPPD/CMPSD: scalar and double-precision selectors.
    Compare(bool, bool),
    /// `maxps` (true) or `minps`, or their scalar forms (`.1`).
    MinMax(bool, bool, bool),
    Sqrt(bool, bool),
    /// `cvtps2pi`, or `cvttps2pi` (true).
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
        self.dst == Loc::Mem || self.src == Loc::Mem || self.kind == Kind::MaskMove
    }

    /// Whether it is an MMX instruction, with the x87 effects of one.
    pub fn mmx(&self) -> bool {
        matches!(self.dst, Loc::Mmx(_))
            || matches!(self.src, Loc::Mmx(_))
            || self.kind == Kind::Emms
    }

    /// Whether it can require a value call.
    pub fn floating(&self) -> bool {
        matches!(
            self.kind,
            Kind::Float(..)
                | Kind::Double(..)
                | Kind::ScalarFromInt(_)
                | Kind::ScalarToInt(..)
                | Kind::FromInt(_)
                | Kind::Widen(_)
                | Kind::Narrow(..)
                | Kind::FlagCompare(..)
                | Kind::Compare(..)
                | Kind::MinMax(..)
                | Kind::Sqrt(..)
        )
    }

    pub(super) fn control_helper(self, key: crate::facts::Key) -> bool {
        self.floating()
            && !key.mxcsr().is_some_and(|class| {
                crate::facts::default_mxcsr(class)
                    || self.directed_rounding() && crate::facts::masked_mxcsr(class)
            })
    }

    /// Whether a `floating` instruction can be inexact.
    fn inexact(&self) -> bool {
        matches!(
            self.kind,
            Kind::Sqrt(..)
                | Kind::Float(..)
                | Kind::Double(..)
                | Kind::ScalarFromInt(false)
                | Kind::ScalarToInt(..)
                | Kind::FromInt(false)
                | Kind::Narrow(..)
        )
    }

    /// Single-instruction NEON operations that can use MXCSR rounding.
    fn directed_rounding(&self) -> bool {
        matches!(self.kind, Kind::Float(Float::Fadd | Float::Fmul, _))
            || self.kind == Kind::FromInt(false) && !self.mmx()
    }

    /// Whether a `floating` instruction reads binary64 operands.
    fn double(&self) -> bool {
        matches!(
            self.kind,
            Kind::Sqrt(_, true)
                | Kind::MinMax(_, _, true)
                | Kind::Compare(_, true)
                | Kind::Double(..)
                | Kind::FlagCompare(true, _)
                | Kind::Narrow(..)
                | Kind::ScalarToInt(true, _)
        )
    }

    /// Whether a `floating` instruction reads only the low lane.
    fn scalar(&self) -> bool {
        match self.kind {
            Kind::Sqrt(scalar, _)
            | Kind::Float(_, scalar)
            | Kind::Double(_, scalar)
            | Kind::Compare(scalar, _)
            | Kind::MinMax(_, scalar, _)
            | Kind::Widen(scalar) => scalar,
            Kind::FlagCompare(..) | Kind::Narrow(true) | Kind::ScalarToInt(..) => true,
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
        if self.kind == Kind::LoadControl {
            after.fp_masked = false;
            after.fp_default = false;
            after.fp_pe = false;
        }
        if self.mmx() {
            after.ready = self.kind != Kind::Emms;
        }
        if self.floating() {
            after.fp_masked = true;
            after.fp_default |= !self.directed_rounding();
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
            Kind::Emms
            | Kind::Move
            | Kind::ExtractWord
            | Kind::ByteMask
            | Kind::StoreControl
            | Kind::PackedToInt(..)
            | Kind::SignMask(_)
            | Kind::ShuffleWords(_) => false,
            Kind::ShiftBytes(_) => self.imm < 16,
            Kind::ShiftImm(op, log2) => {
                let bits = Arr::new(u32::from(log2), self.full()).bits();
                u32::from(self.imm) < bits || op == ShiftImm::Sshr
            }
            Kind::FlagCompare(..) => true,
            _ => !self.floating(),
        };
        let writes = match self.kind {
            Kind::FlagCompare(..) | Kind::MaskMove => 0,
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
    rounded: Option<Label>,
    value: Option<ValueContinuation>,
}

struct ValueContinuation {
    after: Label,
    state: super::State,
    vector: Vector,
    x87: Option<super::x87::Leave>,
    pairs: Option<u8>,
    first_pe: bool,
}

enum ValueOperand {
    Address(Reg),
    Loaded(VReg),
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
    /// MXCSR masks all exceptions and does not enable DAZ or FTZ.
    pub fp_masked: bool,
    /// MXCSR has default control bits, including nearest rounding.
    pub fp_default: bool,
    /// MXCSR PE is set on every path continuing inside this unit.
    pub fp_pe: bool,
}

impl Guards {
    pub const ALL: Self = Self {
        ready: true,
        fp_masked: true,
        fp_default: true,
        fp_pe: true,
    };

    /// The guards both hold.
    pub fn meet(self, other: Self) -> Self {
        Self {
            ready: self.ready && other.ready,
            fp_masked: self.fp_masked && other.fp_masked,
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
            dirty: entry.dirty,
            guards: entry.guards,
            ..Self::default()
        }
    }

    pub(super) fn entry(self) -> Entry {
        Entry {
            regs: u16::from(self.mmx) | u16::from(self.xmm) << 8,
            dirty: self.dirty,
            guards: self.guards,
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

fn value_simd_offset(loc: Loc) -> (VMem, u32) {
    match loc {
        Loc::Xmm(n) => (VMem::Q, runtime::VALUE_XMM + 16 * u32::from(n)),
        Loc::Mmx(n) => (VMem::D, runtime::VALUE_MMX + 8 * u32::from(n)),
        _ => unreachable!("a SIMD value operand"),
    }
}

impl Translator<'_> {
    pub(super) fn simd(&mut self, simd: Simd) {
        // A register it also writes is only published once written.
        let (reads, writes) = simd.footprint();
        let loads = reads as u8 & !self.vector.mmx;
        self.aliased |= loads & writes as u8;
        for n in 0..8 {
            if loads & !writes as u8 & 1 << n != 0 {
                self.mmx_aliased(n);
            }
        }
        let mmx = simd.mmx();
        let first_pe = mmx && simd.floating() && simd.inexact() && !self.vector.guards.fp_pe;
        let ready = self.vector.guards.ready;
        if mmx && !ready {
            self.as_part(Part::SimdGuard, |t| {
                t.cpu();
                if t.tier1 && (t.index != 0 || t.facts.mmx() != Some(0)) {
                    crate::facts::materialize_mmx(t.asm);
                }
                let bail = t.leave_for_event(runtime::Event::PendingX87);
                t.emit(ldrh(X16, X5, runtime::X87_STATUS));
                t.asm.tbz(true, X16, 7, bail);
            });
        }
        let address = if simd.memory() {
            let address = self.effective(simd.mem);
            if simd.aligned {
                // NZCV holds guest flags.
                let bail = self.architectural_exception(13);
                self.emit(and_imm(W, X17, address, 15));
                self.asm.cbz(W, true, X17, bail);
            }
            address
        } else {
            Reg::ZR
        };
        if self.tier1 && simd.control_helper(self.facts) {
            self.simd_value(
                simd,
                ValueOperand::Address(address),
                self.x87_leaving(),
                self.x87_live_pairs(),
            );
        } else {
            self.simd_body(simd, address);
        }
        if mmx && simd.kind != Kind::Emms && !ready {
            self.as_part(Part::SimdGuard, |t| {
                t.cpu();
                t.emit_all(&[
                    ldrh(X17, X5, runtime::X87_STATUS),
                    and_imm(W, X17, X17, 0xffff_c7ff),
                    strh(X17, X5, runtime::X87_STATUS),
                    strh(Reg::ZR, X5, runtime::X87_TAG),
                ]);
            });
            self.vector.guards.ready = true;
        }
        if mmx {
            self.pe_ready(first_pe, self.x87_leaving());
        }
    }

    /// Evaluate an SSE form using the actual MXCSR and the operands that
    /// this unit holds. The selected destination is written only on success.
    fn simd_value(
        &mut self,
        simd: Simd,
        operand: ValueOperand,
        x87: Option<super::x87::Leave>,
        pairs: Option<u8>,
    ) {
        let flags = matches!(simd.kind, Kind::FlagCompare(..));
        let reads_dst = matches!(
            simd.kind,
            Kind::Float(..)
                | Kind::Double(..)
                | Kind::MinMax(..)
                | Kind::Compare(..)
                | Kind::FlagCompare(..)
                | Kind::Sqrt(true, _)
                | Kind::ScalarFromInt(_)
                | Kind::Widen(true)
                | Kind::Narrow(true)
        ) || simd.kind == Kind::FromInt(false) && simd.width == 8;
        match simd.src {
            Loc::Mem => {
                let size = vmem(simd.width);
                let value = match operand {
                    ValueOperand::Address(address) => {
                        self.simd_access(size, V1, address, false);
                        V1
                    }
                    ValueOperand::Loaded(value) => value,
                };
                self.emit(vstr(size, value, RUNTIME, runtime::VALUE_OPERAND));
            }
            Loc::Gpr(n) => self.emit(str(
                W,
                host(n),
                RUNTIME,
                runtime::VALUE_REGS + u32::from(n) * 4,
            )),
            loc => self.publish_value_simd(loc),
        }
        if reads_dst && simd.dst != simd.src {
            self.publish_value_simd(simd.dst);
        }
        self.cpu();
        self.emit_all(&[
            ldr(W, X16, X5, runtime::MXCSR),
            str(W, X16, RUNTIME, runtime::VALUE_MXCSR),
        ]);
        let bail = self.asm.label();
        let inst = self.trace.insts[self.index];
        self.push_cold(Cold::Leave {
            label: bail,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund: self.trace.owed(self.index),
            exit: Exit::Event(runtime::Event::Exception(19)),
            eip: inst.pc,
            link: false,
            x87,
        });
        self.call_prepared_value(simd.helper, runtime::ValueTarget::Float(None), pairs);
        self.emit_all(&[
            ldr(W, X16, RUNTIME, runtime::VALUE_MXCSR),
            str(W, X16, X5, runtime::MXCSR),
        ]);
        self.asm.cbz(W, true, super::X0, bail);
        if flags {
            // COMIS/UCOMIS replace the arithmetic flags and preserve DF.
            use super::{X1, X2, X3, X10, X11};
            self.emit_all(&[
                ldr(W, X1, RUNTIME, runtime::VALUE_EFLAGS),
                and_imm(W, X2, X1, 0x40),
                ubfx(W, X3, X1, 0, 1),
                eor_imm(W, X3, X3, 1),
                orr(W, X2, X2, X3, Shift::Lsl, 5),
                lsl_imm(W, X2, X2, 24),
                msr_nzcv(X2),
                ubfx(W, X10, X1, 2, 1),
                eor_imm(W, X10, X10, 1),
                movz(W, X11, 0, 0),
            ]);
            self.state = super::State::CANONICAL;
        } else if let Loc::Gpr(n) = simd.dst {
            self.clobber(Operand::Guest(n));
            self.emit(ldr(
                W,
                super::ops::result(n),
                RUNTIME,
                runtime::VALUE_REGS + u32::from(n) * 4,
            ));
            self.commit(n);
        } else {
            let (size, offset) = value_simd_offset(simd.dst);
            let destination = self.destination(simd.dst, false);
            self.emit(vldr(size, destination, RUNTIME, offset));
            self.written(simd.dst);
        }
    }

    fn publish_value_simd(&mut self, loc: Loc) {
        let (size, offset) = value_simd_offset(loc);
        let value = self.register(loc);
        self.emit(vstr(size, value, RUNTIME, offset));
    }

    fn simd_body(&mut self, simd: Simd, address: Reg) {
        let full = simd.full();
        match simd.kind {
            Kind::MaskMove => {
                self.mask_move(simd, address);
                return;
            }
            Kind::Approximate(root, scalar) => {
                self.approximate(simd, address, root, scalar);
                return;
            }
            Kind::SignMask(double) => {
                let s = self.register(simd.src);
                let Loc::Gpr(index) = simd.dst else {
                    unreachable!()
                };
                self.clobber(Operand::Guest(index));
                self.emit_all(&[
                    vshift(
                        ShiftImm::Ushr,
                        if double { Arr::D2 } else { Arr::S4 },
                        V0,
                        s,
                        if double { 63 } else { 31 },
                    ),
                    vumov(3, X16, V0, 0),
                    vumov(3, X17, V0, 1),
                ]);
                if double {
                    self.emit(orr(W, X13, X16, X17, Shift::Lsl, 1));
                } else {
                    self.emit_all(&[
                        orr(X, X13, X16, X16, Shift::Lsr, 31),
                        orr(X, X17, X17, X17, Shift::Lsr, 31),
                        and_imm(W, X13, X13, 3),
                        and_imm(W, X17, X17, 3),
                        orr(W, X13, X13, X17, Shift::Lsl, 2),
                    ]);
                }
                self.emit(mov(W, super::ops::result(index), X13));
                self.commit(index);
                return;
            }
            Kind::MoveHalf(high) => {
                let s = self.register(simd.src);
                let d = self.register(simd.dst);
                self.emit(vins(3, d, u32::from(high), s, u32::from(!high)));
                return self.written(simd.dst);
            }
            Kind::PackedToInt(truncate, double) => {
                return self.packed_to_int(simd, address, truncate, double);
            }
            Kind::Emms => {
                self.flush_simd();
                self.cpu();
                self.emit_all(&[movz(W, X17, 0xffff, 0), strh(X17, X5, runtime::X87_TAG)]);
                self.vector.guards.ready = false;
                return;
            }
            Kind::Move => return self.simd_move(simd, address),
            Kind::ExtractWord => {
                let s = self.register(simd.src);
                let lane = u32::from(
                    simd.imm
                        & if matches!(simd.src, Loc::Xmm(_)) {
                            7
                        } else {
                            3
                        },
                );
                if let Loc::Gpr(index) = simd.dst {
                    self.clobber(Operand::Guest(index));
                    self.emit(vumov(1, super::ops::result(index), s, lane));
                    self.commit(index);
                } else {
                    self.emit(vumov(1, X6, s, lane));
                    self.access(
                        super::access::Addr::Window(address),
                        2,
                        super::access::Dir::Store,
                        X6,
                    );
                }
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
                        self.access(
                            super::access::Addr::Window(address),
                            2,
                            super::access::Dir::Load,
                            X17,
                        );
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
            Kind::LoadControl => {
                self.access(
                    super::access::Addr::Window(address),
                    4,
                    super::access::Dir::Load,
                    X13,
                );
                let bail = self.architectural_exception(13);
                self.emit(and_imm(W, X17, X13, 0xffff0000));
                self.asm.cbz(W, true, X17, bail);
                self.cpu();
                self.emit(str(W, X13, X5, runtime::MXCSR));
                self.vector.guards.fp_masked = false;
                self.vector.guards.fp_default = false;
                self.vector.guards.fp_pe = false;
                return;
            }
            Kind::StoreControl => {
                self.cpu();
                self.emit(ldr(W, X13, X5, runtime::MXCSR));
                self.access(
                    super::access::Addr::Window(address),
                    4,
                    super::access::Dir::Store,
                    X13,
                );
                return;
            }
            Kind::Half(high) => return self.half(simd, high, address),
            Kind::Float(..)
            | Kind::Double(..)
            | Kind::ScalarFromInt(_)
            | Kind::ScalarToInt(..)
            | Kind::FromInt(_)
            | Kind::Widen(_)
            | Kind::Narrow(..)
            | Kind::FlagCompare(..)
            | Kind::Compare(..)
            | Kind::MinMax(..)
            | Kind::Sqrt(..) => {
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
                            self.emit(ldr_reg(X, X16, self.data_view(), address, false));
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
        let d = self.destination(simd.dst, !matches!(simd.kind, Kind::ShuffleWords(_)));
        match simd.kind {
            Kind::ShuffleWords(high) => {
                self.emit(vmov(true, V0, s));
                let base = if high { 4 } else { 0 };
                for lane in 0..4 {
                    self.emit(vins(
                        1,
                        V0,
                        base + lane,
                        s,
                        base + (u32::from(simd.imm) >> (lane * 2) & 3),
                    ));
                }
                self.emit(vmov(true, d, V0));
            }
            Kind::Shufpd => {
                self.emit_all(&[
                    vins(3, V0, 0, d, u32::from(simd.imm & 1)),
                    vins(3, V0, 1, s, u32::from(simd.imm >> 1 & 1)),
                    vmov(true, d, V0),
                ]);
            }
            Kind::MulDwords => {
                if full {
                    self.emit_all(&[
                        vpermute(Permute::Uzp1, Arr::S4, V0, d, d),
                        vpermute(Permute::Uzp1, Arr::S4, V2, s, s),
                        vmull(false, Arr::S2, d, V0, V2),
                    ]);
                } else {
                    self.emit_all(&[vmull(false, Arr::S2, V0, d, s), vmov(false, d, V0)]);
                }
            }
            Kind::SadBytes => {
                self.emit_all(&[
                    vsame(Same::Uabd, Arr::new(0, full), V0, d, s),
                    vaddlp(Arr::new(0, full), V0, V0),
                    vaddlp(Arr::new(1, full), V0, V0),
                    vaddlp(Arr::new(2, full), V0, V0),
                    vmov(full, d, V0),
                ]);
            }
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
            | Kind::LoadControl
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
            | Kind::Narrow(..)
            | Kind::FlagCompare(..)
            | Kind::Compare(..)
            | Kind::MinMax(..)
            | Kind::MaskMove
            | Kind::Approximate(..)
            | Kind::Sqrt(..)
            | Kind::Half(_)
            | Kind::SignMask(_)
            | Kind::MoveHalf(_)
            | Kind::PackedToInt(..) => unreachable!(),
        }
        self.written(simd.dst);
    }

    /// Packed binary32 conversions use the interpreter's rounding and sticky
    /// exception rules, including integer indefinite on every invalid lane.
    fn packed_to_int(&mut self, simd: Simd, address: Reg, truncate: bool, double: bool) {
        use super::{X0, X1, X2, X6};
        let s = if simd.src == Loc::Mem {
            self.simd_access(
                if simd.width == 8 { VMem::D } else { VMem::Q },
                V1,
                address,
                false,
            );
            V1
        } else {
            self.register(simd.src)
        };
        self.cpu();
        self.emit_all(&[ldr(W, X13, X5, runtime::MXCSR), movz(W, X0, 0, 0)]);
        let temp = VReg::new(3);
        self.emit(vzero(true, V0));
        for lane in 0..if double || matches!(simd.dst, Loc::Mmx(_)) {
            2
        } else {
            4
        } {
            let (normal, daz, converted, invalid) = (
                self.asm.label(),
                self.asm.label(),
                self.asm.label(),
                self.asm.label(),
            );
            self.emit_all(&[
                vumov(if double { 3 } else { 2 }, X16, s, lane),
                ubfx(
                    if double { X } else { W },
                    X17,
                    X16,
                    if double { 52 } else { 23 },
                    if double { 11 } else { 8 },
                ),
            ]);
            self.asm.cbz(W, true, X17, normal);
            self.emit(lsl_imm(if double { X } else { W }, X17, X16, 1));
            self.asm.cbz(if double { X } else { W }, false, X17, normal);
            self.asm.tbz(true, X13, 6, daz);
            self.emit(orr_imm(W, X0, X0, 2));
            self.asm.b(normal);
            self.asm.bind(daz);
            self.emit(and_imm(
                if double { X } else { W },
                X16,
                X16,
                1 << if double { 63 } else { 31 },
            ));
            self.asm.bind(normal);
            self.emit(msr_fpsr(Reg::ZR));
            self.emit(fmov_to_v(if double { X } else { W }, temp, X16));
            if !double {
                self.emit(unary(Unary::Widen, temp, temp));
            }
            self.emit(fmov_from_v(X, X1, temp));
            if truncate {
                self.emit(unary(Unary::Frintz, V2, temp));
            } else {
                let (down, up, zero, rounded) = (
                    self.asm.label(),
                    self.asm.label(),
                    self.asm.label(),
                    self.asm.label(),
                );
                self.emit(ubfx(W, X17, X13, 13, 2));
                self.asm.tbz(true, X17, 1, up);
                self.asm.tbz(true, X17, 0, down);
                self.emit(unary(Unary::Frintn, V2, temp));
                self.asm.b(rounded);
                self.asm.bind(down);
                self.emit(unary(Unary::Frintm, V2, temp));
                self.asm.b(rounded);
                self.asm.bind(up);
                self.asm.tbz(true, X17, 0, zero);
                self.emit(unary(Unary::Frintp, V2, temp));
                self.asm.b(rounded);
                self.asm.bind(zero);
                self.emit(unary(Unary::Frintz, V2, temp));
                self.asm.bind(rounded);
            }
            self.emit_all(&[fcvt_int(X, true, X6, V2), mrs_fpsr(X17)]);
            self.asm.tbz(true, X17, 0, invalid);
            self.emit_all(&[sbfx(X, X17, X6, 0, 32), eor(X, X17, X17, X6, Shift::Lsl, 0)]);
            self.asm.cbz(X, true, X17, invalid);
            self.emit_all(&[fmov_from_v(X, X17, V2), eor(X, X17, X17, X1, Shift::Lsl, 0)]);
            self.asm.cbz(X, false, X17, converted);
            self.emit(orr_imm(W, X0, X0, 0x20));
            self.asm.b(converted);
            self.asm.bind(invalid);
            self.emit_all(&[movz(W, X6, 0x8000, 16), orr_imm(W, X0, X0, 1)]);
            self.asm.bind(converted);
            self.emit(vins_general(2, V0, lane, X6));
        }
        // Commit sticky flags even on an unmasked exception. The result
        // remains in the temporary until the architectural check passes.
        let bail = self.architectural_exception(19);
        self.emit_all(&[
            lsr_imm(W, X1, X13, 7),
            bic(W, X2, X0, X1, Shift::Lsl, 0),
            orr(W, X13, X13, X0, Shift::Lsl, 0),
            str(W, X13, X5, runtime::MXCSR),
        ]);
        self.asm.cbz(W, true, X2, bail);
        let d = self.destination(simd.dst, false);
        self.emit(vmov(simd.full(), d, V0));
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

    fn approximate(&mut self, simd: Simd, address: Reg, root: bool, scalar: bool) {
        use super::{X0, X1};
        let source = if simd.src == Loc::Mem {
            self.simd_access(if scalar { VMem::S } else { VMem::Q }, V1, address, false);
            V1
        } else {
            self.register(simd.src)
        };
        self.canonicalize();
        // Approximations ignore MXCSR and never publish exceptions.
        self.emit_all(&[mrs_nzcv(X13), mrs_fpcr(X14), msr_fpcr(Reg::ZR)]);
        let v3 = VReg::new(3);
        for lane in 0..if scalar { 1 } else { 4 } {
            let [nan, small, negative, infinite, finish] =
                std::array::from_fn(|_| self.asm.label());
            self.emit_all(&[
                vumov(2, X16, source, lane),
                and_imm(W, X17, X16, 0x7fff_ffff),
                and_imm(W, X1, X16, 1 << 31),
                movz(W, X0, 0x7f80, 16),
                cmp(W, X17, X0, Shift::Lsl, 0),
            ]);
            self.asm.b_cond(Cond::Hi, nan);
            self.emit(cmp_imm(W, X17, 0x800, true));
            self.asm.b_cond(Cond::Lo, small);
            if root {
                self.asm.tbz(true, X16, 31, negative);
            }
            self.emit(cmp(W, X17, X0, Shift::Lsl, 0));
            self.asm.b_cond(Cond::Eq, infinite);
            self.emit(fmov_to_v(W, V2, X16));
            if root {
                self.emit(fsqrt(true, false, V2, V2));
            }
            self.emit_all(&[
                movz(W, X0, 0x3f80, 16),
                fmov_to_v(W, v3, X0),
                sfloat(Float::Fdiv, V2, v3, V2),
                fmov_from_v(W, X6, V2),
                add_imm(W, X6, X6, 0x800, false),
                and_imm(W, X6, X6, 0xffff_f000),
                and_imm(W, X17, X6, 0x7f80_0000),
            ]);
            self.asm.cbz(W, false, X17, infinite);
            self.asm.b(finish);
            self.asm.bind(nan);
            self.emit(orr_imm(W, X6, X16, 0x0040_0000));
            self.asm.b(finish);
            self.asm.bind(small);
            self.emit(orr(W, X6, X1, X0, Shift::Lsl, 0));
            self.asm.b(finish);
            self.asm.bind(negative);
            self.emit(movz(W, X6, 0xffc0, 16));
            self.asm.b(finish);
            self.asm.bind(infinite);
            self.emit(mov(W, X6, X1));
            self.asm.bind(finish);
            self.emit(vins_general(2, V0, lane, X6));
        }
        self.emit_all(&[msr_fpcr(X14), msr_nzcv(X13)]);
        let destination = self.destination(simd.dst, scalar);
        self.emit(if scalar {
            vins(2, destination, 0, V0, 0)
        } else {
            vmov(true, destination, V0)
        });
        self.written(simd.dst);
    }

    fn mask_move(&mut self, simd: Simd, address: Reg) {
        self.canonicalize();
        let data = self.register(simd.dst);
        let mask = self.register(simd.src);
        let slow = self.leave_for_event(runtime::Event::CheckedAccess);
        let guest = Reg::new(7);
        self.emit(mov(W, guest, address));
        *self.uses_permissions = true;
        // Check every selected byte before any store; unselected bytes are untouched.
        for lane in 0..u32::from(simd.width) {
            let skip = self.asm.label();
            self.emit(vumov(0, X6, mask, lane));
            self.asm.tbz(false, X6, 7, skip);
            self.emit(add_imm(X, X16, guest, lane, false));
            self.asm.tbz(true, X16, 32, slow);
            self.checked_base(X16, true, false, super::X2, slow);
            self.checked_alias(X16, true, false, slow);
            self.asm.bind(skip);
        }
        self.emit(ldr(X, super::X2, RUNTIME, runtime::ALIAS));
        for lane in 0..u32::from(simd.width) {
            let skip = self.asm.label();
            self.emit(vumov(0, X6, mask, lane));
            self.asm.tbz(false, X6, 7, skip);
            self.emit_all(&[
                vumov(0, X6, data, lane),
                add_imm(W, X16, guest, lane, false),
            ]);
            self.site();
            self.emit(str_narrow(Narrow::Byte, X6, super::X2, X16));
            self.asm.bind(skip);
        }
    }

    /// Arithmetic, conversions and binary32/64 compares. The fast path
    /// never touches FPSR: it runs once PE is set, or
    /// for operations that cannot be inexact, with no denormal operand, and
    /// sends values that could raise anything else to the slow path, which
    /// retries between a cleared and a read FPSR and uses a value call on
    /// any exception but inexact. Uses x13, x14 and
    /// v0-v6.
    fn floating(&mut self, simd: Simd, address: Reg) {
        self.cpu();
        let bail = if self.tier1 {
            self.asm.label()
        } else {
            self.bail()
        };
        let (slow, resume) = (self.asm.label(), self.asm.label());
        let after = self.tier1.then(|| self.asm.label());
        let inexact = simd.inexact();
        let first_pe = inexact && !self.vector.guards.fp_pe;
        let guards = self.vector.guards;
        let directed = !guards.fp_default && simd.directed_rounding();
        let admission = if self.tier1 && !guards.fp_default {
            let inst = self.trace.insts[self.index];
            self.fact_transfer(inst.pc, self.trace.owed(self.index), self.x87_leaving())
        } else {
            bail
        };
        let rounded = directed.then(|| self.asm.label());
        self.as_part(Part::SimdGuard, |t| {
            if inexact && !guards.fp_pe || !guards.fp_default {
                t.emit(ldr(W, X13, X5, runtime::MXCSR));
            }
            // Masked exceptions, no DAZ or FTZ; other forms require nearest.
            if !guards.fp_default {
                if !guards.fp_masked {
                    t.emit_all(&[and_imm(W, X17, X13, 0xffc0), eor_imm(W, X17, X17, 0x1f80)]);
                    if directed {
                        t.emit(and_imm(W, X17, X17, u64::from(!0x6000u32)));
                    }
                    t.asm.cbz(W, true, X17, admission);
                } else if !directed {
                    t.emit(and_imm(W, X17, X13, 0x6000));
                    t.asm.cbz(W, true, X17, admission);
                }
            }
        });
        self.vector.guards.fp_masked = true;
        self.vector.guards.fp_default = !directed;
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
            | Kind::FlagCompare(..) => [Some(self.register(simd.dst)), Some(s)],
            _ => [None, Some(s)],
        };
        let operands = Operands { s, inputs };
        if inexact && !self.vector.guards.fp_pe {
            self.as_part(Part::SimdGuard, |t| t.asm.tbz(false, X13, 5, slow));
            self.vector.guards.fp_pe = true;
        }
        self.denormals(simd, operands, Some(slow));
        if let Some(rounded) = rounded {
            self.as_part(Part::SimdGuard, |t| {
                t.emit(and_imm(W, X17, X13, 0x6000));
                t.asm.cbz(W, true, X17, rounded);
            });
        }
        let tiny = self.floating_fast(simd, operands, slow, false);
        self.asm.bind(resume);
        self.push_cold(Cold::Floating(Slow {
            simd,
            operands,
            slow,
            tiny,
            resume,
            bail,
            rounded,
            value: after.map(|after| ValueContinuation {
                after,
                state: self.state,
                vector: self.vector,
                x87: self.x87_leaving(),
                pairs: self.x87_live_pairs(),
                first_pe: first_pe && !simd.mmx(),
            }),
        }));
        if matches!(simd.kind, Kind::ScalarToInt(..)) {
            let Loc::Gpr(index) = simd.dst else {
                unreachable!()
            };
            self.clobber(Operand::Guest(index));
            self.emit(fmov_from_v(W, super::ops::result(index), V0));
            self.commit(index);
            self.pe_ready(first_pe && !simd.mmx(), self.x87_leaving());
            if let Some(after) = after {
                self.asm.bind(after);
            }
            return;
        }
        if matches!(simd.kind, Kind::FlagCompare(..)) {
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
            if let Some(after) = after {
                self.asm.bind(after);
            }
            return;
        }
        let d = self.destination(
            simd.dst,
            matches!(
                simd.kind,
                Kind::Sqrt(true, _)
                    | Kind::Widen(true)
                    | Kind::Narrow(true)
                    | Kind::ScalarFromInt(_)
            ),
        );
        if simd.kind == Kind::FromInt(false) && simd.width == 8 {
            let d = self.register(simd.dst);
            self.emit(vins(3, d, 0, V0, 0));
            self.written(simd.dst);
            self.pe_ready(first_pe && !simd.mmx(), self.x87_leaving());
            if let Some(after) = after {
                self.asm.bind(after);
            }
            return;
        }
        self.emit(match simd.kind {
            Kind::Compare(true, double) => vins(if double { 3 } else { 2 }, d, 0, V0, 0),
            Kind::Sqrt(true, false)
            | Kind::ScalarFromInt(false)
            | Kind::Float(_, true)
            | Kind::MinMax(_, true, false)
            | Kind::Narrow(true) => vins(2, d, 0, V0, 0),
            Kind::Sqrt(true, true)
            | Kind::MinMax(_, true, true)
            | Kind::ScalarFromInt(true)
            | Kind::Double(_, true)
            | Kind::Widen(true) => vins(3, d, 0, V0, 0),
            _ => vmov(true, d, V0),
        });
        self.written(simd.dst);
        self.pe_ready(first_pe && !simd.mmx(), self.x87_leaving());
        if let Some(after) = after {
            self.asm.bind(after);
        }
    }

    /// An exact slow operation can leave PE clear; finish it, then leave
    /// before entering code that assumes the sticky bit has been established.
    fn pe_ready(&mut self, first: bool, x87: Option<super::x87::Leave>) {
        if !first {
            return;
        }
        let leave = self.asm.label();
        self.emit(ldr(W, X13, X5, runtime::MXCSR));
        if x87.is_some() {
            self.x87_require_bit(X13, 5, leave);
        } else {
            self.asm.tbz(false, X13, 5, leave);
        }
        self.push_cold(Cold::Leave {
            label: leave,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund: self.trace.unused(self.index),
            exit: crate::runtime::Exit::Dispatch,
            eip: self.trace.insts[self.index].next_pc,
            link: true,
            x87,
        });
    }

    /// The fast path of `floating` into v0 (and v2), with the checks around
    /// it; the label of the cold check of a zero or subnormal result that
    /// zero operands may make exact.
    fn floating_fast(
        &mut self,
        simd: Simd,
        operands: Operands,
        slow: Label,
        rounded: bool,
    ) -> Option<Label> {
        let (v3, v4, v5, v6) = (VReg::new(3), VReg::new(4), VReg::new(5), VReg::new(6));
        let double = simd.double();
        let scalar = simd.scalar();
        let [a, _] = operands.inputs;
        let s = operands.s;
        match simd.kind {
            Kind::Narrow(false) => {
                self.asm.b(slow);
                return None;
            }
            // Any NaN may raise IE: the slow path tells.
            Kind::MinMax(..)
            | Kind::Compare(..)
            | Kind::FlagCompare(..)
            | Kind::Widen(_)
            | Kind::ScalarToInt(..) => {
                let inputs: Vec<VReg> = operands.inputs.into_iter().flatten().collect();
                if simd.kind == Kind::Widen(false) {
                    fp_class::nan_low_pair(self.asm, s, v3, X16, slow);
                } else {
                    fp_class::nan_operands(self.asm, scalar, double, &inputs, X16, [v3, v4], slow);
                }
            }
            _ => {}
        }
        if let Kind::Sqrt(scalar, double) = simd.kind {
            self.floating_op(simd, operands);
            fp_class::nan_result(self.asm, scalar, double, V0, v3, X16, slow);
            return None;
        }
        if rounded {
            self.rounded_op(simd, operands);
        } else {
            self.floating_op(simd, operands);
        }
        match simd.kind {
            Kind::Float(op, _) | Kind::Double(op, _) if scalar => {
                let sum = matches!(op, Float::Fadd | Float::Fsub);
                fp_class::result_bits(self.asm, double, V0, X16, X17, (!sum).then_some(X14));
                if rounded {
                    // Overflow can saturate to max finite.
                    self.emit(add_imm(W, X16, X17, 1, false));
                    self.asm.tbz(true, X16, 8, slow);
                }
                if !sum {
                    // A product or quotient rounded up to min normal can
                    // be tiny after rounding and raise underflow.
                    self.emit(eor_imm(W, X16, X17, 2));
                    self.asm.cbz(W, false, X16, slow);
                }
                if sum {
                    // A tiny sum is exact: only infinities and NaNs.
                    if !rounded {
                        self.asm.tbz(true, X17, if double { 11 } else { 8 }, slow);
                    }
                    return None;
                }
                let label = self.asm.label();
                self.asm.cbz(W, false, X14, label);
                Some(label)
            }
            Kind::Float(op, _) | Kind::Double(op, _) => {
                fp_class::packed_arithmetic_mask(
                    self.asm,
                    double,
                    op,
                    rounded,
                    [V0, a.unwrap(), s],
                    X16,
                    [V2, v3, v4, v5, v6],
                );
                fp_class::any_lane(self.asm, v6, X16, slow);
                None
            }
            Kind::Narrow(..) => {
                fp_class::result_bits(self.asm, false, V0, X16, X17, Some(X14));
                // Rounded up to min normal, as for products.
                self.emit(eor_imm(W, X16, X17, 2));
                self.asm.cbz(W, false, X16, slow);
                let label = self.asm.label();
                self.asm.cbz(W, false, X14, label);
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
        if simd.scalar() {
            for input in inputs.into_iter().flatten() {
                fp_class::scalar_denormal(self.asm, double, input, X16, X17, to, X13);
            }
            return;
        }
        let arr = match simd.kind {
            Kind::Widen(false) => Arr::S2,
            _ if double => Arr::D2,
            _ => Arr::S4,
        };
        fp_class::packed_denormals(self.asm, arr, double, inputs, X16, [v4, v5, v6, V0]);
        fp_class::finish_denormals(self.asm, double, v6, [X14, X17, X13], to);
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
            Kind::Sqrt(scalar, double) => {
                self.emit(fsqrt(scalar, double, V0, s));
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
            Kind::Narrow(scalar) => {
                self.emit(unary(Unary::Narrow, V0, s));
                if !scalar {
                    self.emit_all(&[
                        vins(3, V2, 0, s, 1),
                        unary(Unary::Narrow, V2, V2),
                        vins(2, V0, 1, V2, 0),
                    ]);
                }
            }
            Kind::FlagCompare(double, _) => {
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
            Kind::MinMax(max, scalar, double) => {
                let d = d.unwrap();
                // The first source where it compares above (max) or
                // below (min) the second, else the second; any NaN is
                // invalid for the compare.
                let (a, b) = if max { (d, s) } else { (s, d) };
                self.emit_all(&[
                    vcompare(Float::Fcmgt, scalar, double, V0, a, b),
                    vlogic(Logic::Bsl, true, V0, d, s),
                ]);
            }
            _ => unreachable!(),
        }
    }

    /// Run a single operation under MXCSR rounding, restoring host FPCR.
    fn rounded_op(&mut self, simd: Simd, operands: Operands) {
        // x86 and ARM reverse the encodings for round up and round down.
        self.emit_all(&[
            and_imm(W, X17, X13, 0x6000),
            rbit(W, X17, X17),
            lsl_imm(W, X17, X17, 5),
            mrs_fpcr(X14),
            msr_fpcr(X17),
        ]);
        self.floating_op(simd, operands);
        self.emit(msr_fpcr(X14));
    }

    fn floating_tiny(&mut self, simd: Simd, operands: Operands, slow: Label, resume: Label) {
        let double = simd.double();
        self.asm.tbz(true, X17, if double { 11 } else { 8 }, slow);
        let zeros = match simd.kind {
            Kind::Float(Float::Fmul, _) | Kind::Double(Float::Fmul, _) => operands.inputs,
            Kind::Narrow(..) => [Some(operands.s), None],
            _ => [operands.inputs[0], None],
        };
        for zero in zeros.into_iter().flatten() {
            let size = if double { X } else { W };
            self.emit_all(&[fmov_from_v(size, X16, zero), lsl_imm(size, X16, X16, 1)]);
            self.asm.cbz(size, false, X16, resume);
        }
    }

    /// The out-of-line paths of a `floating` instruction.
    pub(super) fn floating_cold(&mut self, cold: Slow) {
        let Slow {
            simd,
            operands,
            slow,
            tiny,
            resume,
            bail,
            rounded,
            value,
        } = cold;
        if let Some(rounded) = rounded {
            self.asm.bind(rounded);
            self.remark(Part::Body);
            let tiny = self.floating_fast(simd, operands, slow, true);
            self.asm.b(resume);
            if let Some(tiny) = tiny {
                self.asm.bind(tiny);
                self.floating_tiny(simd, operands, slow, resume);
                self.asm.b(slow);
            }
        }
        if let Some(tiny) = tiny {
            self.asm.bind(tiny);
            self.floating_tiny(simd, operands, slow, resume);
        }
        self.asm.bind(slow);
        self.remark(crate::jit_map::Part::FpStatus);
        self.emit(ldr(W, X13, X5, runtime::MXCSR));
        self.denormals(simd, operands, None);
        self.emit(msr_fpsr(Reg::ZR));
        if rounded.is_some() {
            self.rounded_op(simd, operands);
        } else {
            self.floating_op(simd, operands);
        }
        // Anything but inexact leaves; inexact sets PE.
        self.emit_all(&[mrs_fpsr(X17), and_imm(X, X14, X17, 0xf)]);
        self.asm.cbz(X, true, X14, bail);
        self.emit_all(&[
            ubfx(W, X17, X17, 4, 1),
            orr(W, X13, X13, X17, Shift::Lsl, 5),
            str(W, X13, X5, runtime::MXCSR),
        ]);
        self.asm.b(resume);
        if let Some(value) = value {
            self.asm.bind(bail);
            self.state = value.state;
            self.vector = value.vector;
            self.simd_value(
                simd,
                ValueOperand::Loaded(operands.s),
                value.x87,
                value.pairs,
            );
            self.pe_ready(value.first_pe, value.x87);
            self.asm.b(value.after);
        }
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
            if let Loc::Mmx(n) = loc {
                assert!(self.aliased & 1 << n != 0, "MMX {n} loads unchecked");
                self.aliased &= !(1 << n);
            }
            self.at_cpu(true, size, v, offset);
        }
        v
    }

    /// Loads `loc` at a merge's arrival.
    fn arrive_load(&mut self, loc: Loc) {
        if let Loc::Mmx(n) = loc
            && self.vector.mmx & 1 << n == 0
        {
            self.mmx_aliased(n);
        }
        self.register(loc);
    }

    /// Before the cache loads MMX register `n`, while x16 and x17 are free:
    /// refreshes the x87 alias for Tier-1 and refuses inadmissible worker
    /// state. A merge may count a
    /// loaded register dirty, and publishing sets that exponent.
    fn mmx_aliased(&mut self, n: u8) {
        self.aliased |= 1 << n;
        self.cpu();
        if self.tier1 {
            crate::facts::refresh_mmx_slot(self.asm, n);
            return;
        }
        let leave = match self.unloadable {
            Some(leave) => leave,
            None => self.bail(),
        };
        let ok = self.asm.label();
        let exponent = runtime::X87_REGS + 10 * u32::from(n) + 8;
        self.emit_all(&[ldrh(X17, X5, exponent), add_imm(W, X17, X17, 1, false)]);
        self.asm.tbz(true, X17, 16, ok);
        // `CpuState::commit_mmx` will write it.
        self.emit(ldr(W, X16, X5, runtime::MMX_MODE));
        self.asm.tbz(false, X16, 0, leave);
        self.asm.tbz(false, X16, 2, ok);
        self.asm.tbz(false, X16, 8 + u32::from(n), leave);
        self.asm.bind(ok);
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

    /// Arrives at a merge whose entry is `entry`: publishes what it holds
    /// dirty outside the entry's dirty set, loads what the cache lacks of
    /// it, and forgets the rest.
    pub(super) fn coerce(&mut self, entry: Entry) {
        // Guards cannot be re-established here; the analysis must agree.
        assert!(
            entry.guards.within(self.vector.guards),
            "an arrival at {:#x} lacks guards",
            self.trace.insts[self.index].pc
        );
        self.publish(!entry.dirty);
        self.as_part(Part::SimdLoad, |t| {
            for n in 0..8 {
                if entry.regs & (1 << n) != 0 {
                    t.arrive_load(Loc::Mmx(n));
                }
                if entry.regs & (1 << (8 + n)) != 0 {
                    t.arrive_load(Loc::Xmm(n));
                }
            }
        });
        self.vector = Vector::at(entry);
    }

    /// Establishes a region head's entry for arrivals from outside the unit:
    /// checks the worker's MXCSR guards and loads its registers.
    /// A failed worker guard returns to the owner. The head's countdown
    /// check has charged `refund`.
    pub(super) fn enter_simd(&mut self, entry: Entry, refund: u32) {
        let guards = entry.guards;
        if !self.tier1 && (guards.fp_masked || guards.fp_default) {
            let leave = self.asm.label();
            self.push_cold(Cold::Leave {
                label: leave,
                state: self.state,
                simd_dirty: 0,
                refund,
                // A job returns to its owner, which runs the head.
                exit: if self.computation {
                    Exit::Recall
                } else {
                    Exit::Dispatch
                },
                eip: self.trace.insts[self.index].pc,
                link: false,
                x87: None,
            });
            // NZCV holds guest flags; exceptions masked, no DAZ or FTZ, and
            // nearest rounding where the body needs it.
            self.as_part(Part::SimdGuard, |t| {
                t.cpu();
                t.emit_all(&[
                    ldr(W, X13, X5, runtime::MXCSR),
                    and_imm(W, X17, X13, 0xffc0),
                    eor_imm(W, X17, X17, 0x1f80),
                ]);
                if !guards.fp_default {
                    t.emit(and_imm(W, X17, X17, u64::from(!0x6000u32)));
                }
                t.asm.cbz(W, true, X17, leave);
            });
        }
        self.as_part(Part::SimdLoad, |t| {
            for n in 0..8 {
                if entry.regs & (1 << n) != 0 {
                    t.arrive_load(Loc::Mmx(n));
                }
                if entry.regs & (1 << (8 + n)) != 0 {
                    t.arrive_load(Loc::Xmm(n));
                }
            }
        });
        self.vector = Vector::at(entry);
    }

    pub(super) fn body_cache(&mut self) {
        // Clean values remain in CpuState; only unpublished values are live
        // across a body that may fault before its return.
        self.vector.xmm &= (self.vector.dirty >> 8) as u8;
    }

    /// Only the XMM cache is caller-saved; AAPCS64 preserves MMX in d8-d15.
    pub(super) fn body_simd(&mut self, restore: bool) {
        let count = self.vector.xmm.count_ones();
        if count != 0 {
            let sp = Reg::new(31);
            if !restore {
                self.emit(sub_imm(X, sp, sp, count * 16, false));
            }
            let mut offset = 0;
            for n in 0..8 {
                if self.vector.xmm & (1 << n) != 0 {
                    let v = VReg::new(16 + n);
                    self.emit(if restore {
                        vldr(VMem::Q, v, sp, offset)
                    } else {
                        vstr(VMem::Q, v, sp, offset)
                    });
                    offset += 16;
                }
            }
            if restore {
                self.emit(add_imm(X, sp, sp, count * 16, false));
            }
        }
        self.vector.cpu = false;
    }

    /// Publishes the SIMD prefix without changing flags, addresses, x5 or SIMD
    /// temporaries. x0/x3/x4 are free at control and observer boundaries.
    pub(super) fn invalidate_mmx(&mut self) {
        self.vector.mmx = 0;
        self.vector.guards.ready = false;
        self.aliased = 0;
    }

    pub(super) fn call_prepared_value(
        &mut self,
        helper: u8,
        target: runtime::ValueTarget,
        pairs: Option<u8>,
    ) {
        let index = self.helpers + u32::from(helper);
        runtime::emit_value_call(self.asm, index, self.vector.xmm, pairs, target);
    }

    pub(super) fn publish_fx_image(&mut self) {
        for n in 0..8 {
            self.publish_value_simd(Loc::Xmm(n));
        }
        self.cpu();
        self.emit_all(&[
            ldr(W, X16, X5, runtime::MXCSR),
            str(W, X16, RUNTIME, runtime::VALUE_MXCSR),
        ]);
    }

    pub(super) fn reload_fx_image(&mut self) {
        for n in 0..8 {
            if self.vector.xmm & (1 << n) != 0 {
                let (size, offset) = value_simd_offset(Loc::Xmm(n));
                self.emit(vldr(size, VReg::new(16 + n), RUNTIME, offset));
            }
        }
        self.vector.dirty = 0;
        self.vector.guards = Guards::default();
    }

    pub(super) fn flush_simd(&mut self) {
        self.publish(self.vector.dirty);
    }

    /// Publishes the dirty registers of `mask`, as `flush_simd` does.
    pub(super) fn publish(&mut self, mask: u16) {
        let dirty = self.vector.dirty & mask;
        if dirty == 0 {
            return;
        }
        self.as_part(Part::SimdPublish, |t| t.emit_publish(dirty));
    }

    fn emit_publish(&mut self, dirty: u16) {
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
        self.vector.dirty &= !dirty;
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

    /// A label that returns an inadmissible form to the owner.
    fn bail(&mut self) -> crate::asm::Label {
        let label = self.asm.label();
        let inst = self.trace.insts[self.index];
        self.push_cold(Cold::Leave {
            label,
            state: self.state,
            simd_dirty: self.vector.dirty,
            refund: self.trace.owed(self.index),
            exit: if self.computation {
                Exit::Recall
            } else {
                Exit::Dispatch
            },
            eip: inst.pc,
            link: false,
            x87: self.x87_leaving(),
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
                fault: 0,
                opt_exit: 0,
            },
            &|_, _| None,
            &|_| None,
            &|_, _| Vec::new(),
            0x037f,
            false,
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
                .all(|i| !matches!(i.op, super::super::decode::Op::Value(_)))
        );
        // Samples can tell the residency code from the body.
        let parts: Vec<_> = work.marks().iter().map(|mark| mark.part).collect();
        for part in [Part::SimdGuard, Part::SimdPublish] {
            assert!(parts.contains(&part), "{part:?}");
        }
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
        let movq2dq = plan(&decode(|a| a.movq2dq(xmm1, mm2).unwrap()), 0).unwrap();
        assert_eq!(
            (movq2dq.kind, movq2dq.dst, movq2dq.src, movq2dq.width),
            (Kind::Move, Loc::Xmm(1), Loc::Mmx(2), 8)
        );
        let movdq2q = plan(&decode(|a| a.movdq2q(mm1, xmm2).unwrap()), 0).unwrap();
        assert_eq!(
            (movdq2q.kind, movdq2q.dst, movdq2q.src, movdq2q.width),
            (Kind::Move, Loc::Mmx(1), Loc::Xmm(2), 8)
        );
        let add = plan(&decode(|a| a.addss(xmm1, dword_ptr(esi)).unwrap()), 0).unwrap();
        assert_eq!((add.kind, add.width), (Kind::Float(Float::Fadd, true), 4));
        assert!(add.floating());
        let convert = plan(&decode(|a| a.cvttps2pi(mm1, xmm2).unwrap()), 0).unwrap();
        assert_eq!(
            (convert.kind, convert.dst, convert.src),
            (Kind::PackedToInt(true, false), Loc::Mmx(1), Loc::Xmm(2))
        );
        assert!(convert.mmx() && !convert.floating());
        let sqrt = plan(&decode(|a| a.sqrtps(xmm1, xmm2).unwrap()), 0).unwrap();
        assert_eq!(
            (sqrt.kind, sqrt.dst, sqrt.src),
            (Kind::Sqrt(false, false), Loc::Xmm(1), Loc::Xmm(2))
        );
        assert!(sqrt.floating());
        assert!(plan(&decode(|a| a.emms().unwrap()), 0).unwrap().mmx());
    }
}
