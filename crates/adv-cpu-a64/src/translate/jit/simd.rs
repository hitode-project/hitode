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

use super::{FpcrMode, X1, X2};
use crate::translate::front::flags::FlagSet;
use crate::translate::lower::fp_class;
use crate::translate::lower::fp_state;
use crate::translate::lower::stubs;

#[cfg(test)]
use crate::translate::front::plans::simd as plan;
#[cfg(test)]
use iced_x86::Instruction;

use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit};

use super::residency::Entry;
use crate::asm::Label;
use crate::jit_map::Part;
use crate::translate::front::forms::{Loc, Simd, SimdKind as Kind};
use crate::translate::lower::flags::Operand;

use super::{Cold, RUNTIME, Translator, X5, X6, X13, X14, X16, X17, host};

const V0: VReg = VReg::new(0);
const V1: VReg = VReg::new(1);
const V2: VReg = VReg::new(2);

impl Simd {
    /// Whether a same-register logical or equality instruction defines a
    /// constant (`0` or all-ones) without reading its operand.
    fn is_const_idiom(&self) -> bool {
        self.dst == self.src
            && matches!(self.dst, Loc::Mmx(_) | Loc::Xmm(_))
            && matches!(
                self.kind,
                Kind::Logic(Logic::Eor) | Kind::AndNot | Kind::Same(Same::Cmeq, _)
            )
    }

    /// ARMv8.0 (non-AFP) control-helper gate: requires default MXCSR, or
    /// masked non-DAZ/FTZ MXCSR for single-instruction directed-rounding ops.
    fn v8_control_helper(self, key: crate::facts::Key) -> bool {
        self.floating()
            && !key.mxcsr().is_some_and(|class| {
                crate::facts::default_mxcsr(class)
                    || self.directed_rounding() && crate::facts::masked_mxcsr(class)
            })
    }

    pub(super) fn control_helper(self, key: crate::facts::Key) -> bool {
        if !self.floating() {
            return false;
        }
        let Some(class) = key.mxcsr() else {
            return true;
        };
        if !fp_state::sse_exceptions_masked(class) {
            return true;
        }
        if crate::facts::default_mxcsr(class)
            || self.directed_rounding() && crate::facts::masked_mxcsr(class)
        {
            return false;
        }
        !adv_cpu_core::HostFeatures::get().contains(adv_cpu_core::HostFeatures::AFP)
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
    /// `Vector::dirty` numbers them (`translate::jit::residency`).
    pub fn footprint(&self) -> (u16, u16) {
        let bit = |loc| match loc {
            Loc::Mmx(n) => 1u16 << n,
            Loc::Xmm(n) => 1 << (8 + n),
            _ => 0,
        };
        if self.is_const_idiom() {
            return (0, bit(self.dst));
        }
        // As the lowering below reads its destination.
        let reads_dst = match self.kind {
            Kind::Emms
            | Kind::Move
            | Kind::ExtractWord
            | Kind::ByteMask
            | Kind::StoreControl
            | Kind::PackedToInt(..)
            | Kind::SignMask(_)
            | Kind::Shuffle(_)
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
    kind: SlowKind,
}

enum SlowKind {
    Legacy {
        simd: Simd,
        operands: Operands,
        slow: Label,
        /// Checks a zero or subnormal result, then goes on or takes `slow`.
        tiny: Option<Label>,
        resume: Label,
        bail: Label,
        rounded: Option<Label>,
        value: Option<ValueContinuation>,
    },
    /// Out-of-line NaN flag materialization for `ucomis*`/`comis*` without `FEAT_FlagM2`.
    UnorderedFlags {
        label: Label,
        resume: Label,
        need_nzcv: bool,
        need_pf: bool,
        need_af: bool,
    },
    /// Dynamic unmasked-exception fallback for `FEAT_AFP` paths where MXCSR
    /// control bits are not statically known to mask all six SSE exceptions.
    AfpUnmasked {
        label: Label,
        after: Label,
        simd: Simd,
        s: VReg,
        value: ValueContinuation,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum KnownFpcr {
    #[default]
    Unknown,
    /// Active hardware FPCR holds this exact `FPCR` image (with `AH = 1`).
    Const(u32),
    /// Active hardware FPCR was dynamically synced from live `MXCSR` (with `AH = 1`).
    MatchesMxcsr,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FpTrack {
    /// `index + 1` of the trace instruction that established this state (`0` = unknown).
    valid_at: u32,
    fpcr: KnownFpcr,
    fpsr_armed: bool,
    all_masked: bool,
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
    /// Straight-line `FEAT_AFP` FPCR/FPSR state within the current basic block.
    fp: FpTrack,
}

/// Facts about `CpuState` that guards established on every path here.
/// Nothing but helpers, x87 instructions and EMMS changes them, so they
/// survive merges whose arrivals all hold them (`translate::jit::residency`).
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
        let first_pe = !self.features.afp
            && mmx
            && simd.floating()
            && simd.inexact()
            && !self.vector.guards.fp_pe;
        let ready = self.vector.guards.ready;
        if mmx && !ready {
            self.as_part(Part::SimdGuard, |t| {
                t.cpu();
                if t.tier1 && (t.index != 0 || t.facts.mmx() != Some(0)) {
                    t.materialize_mmx();
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
        let use_afp = self.features.afp
            && !(matches!(simd.kind, Kind::ScalarToInt(..)) && !self.features.frintts);
        let needs_control_helper = if use_afp {
            simd.floating()
                && !self
                    .facts
                    .mxcsr()
                    .is_some_and(fp_state::sse_exceptions_masked)
        } else {
            simd.v8_control_helper(self.facts)
        };
        if self.tier1 && needs_control_helper {
            self.simd_value(
                simd,
                ValueOperand::Address(address),
                self.x87_leaving(),
                self.x87_live_pairs(),
            );
            // Keep residency guard monotonicity if a test overrides `Features`
            // below `HostFeatures::get()`.
            if !simd.control_helper(self.facts) {
                self.vector.guards = simd.guarding(self.vector.guards);
            }
        } else {
            self.simd_body(simd, address);
        }
        #[cfg(native)]
        if mmx && !ready {
            self.scope_mmx_write(1 << 8);
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
        if self.features.afp {
            // Fold any lazy SSE FPSR sticky bits before copying `CpuState::mxcsr`
            // into `VALUE_MXCSR`.
            fp_state::emit_fold_fpsr(self.asm, super::X0, super::X1, super::X2);
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
        self.call_prepared_value(simd.helper, stubs::ValueTarget::Float(None), pairs);
        self.reset_fp_track();
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
                        crate::translate::lower::access::Addr::Window(address),
                        2,
                        crate::translate::lower::access::Dir::Store,
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
                            crate::translate::lower::access::Addr::Window(address),
                            2,
                            crate::translate::lower::access::Dir::Load,
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
                    crate::translate::lower::access::Addr::Window(address),
                    4,
                    crate::translate::lower::access::Dir::Load,
                    X13,
                );
                let bail = self.architectural_exception(13);
                self.emit(and_imm(W, X17, X13, 0xffff0000));
                self.asm.cbz(W, true, X17, bail);
                self.cpu();
                self.emit(str(W, X13, X5, runtime::MXCSR));
                if self.features.afp {
                    // LDMXCSR replaces all MXCSR sticky bits: discard lazy FPSR
                    // and invalidate cached FPCR assumptions (A64F-FP-5).
                    fp_state::emit_discard_fpsr(self.asm);
                    self.reset_fp_track();
                }
                self.vector.guards.fp_masked = false;
                self.vector.guards.fp_default = false;
                self.vector.guards.fp_pe = false;
                return;
            }
            Kind::StoreControl => {
                if self.features.afp {
                    // Fold lazy SSE FPSR sticky bits before observing MXCSR.
                    // `address` may live in X16 or X17, so use X0/X1/X2.
                    fp_state::emit_fold_fpsr(self.asm, super::X0, super::X1, super::X2);
                    self.vector.fp.fpsr_armed = false;
                    self.fpsr_armed = false;
                }
                self.cpu();
                self.emit(ldr(W, X13, X5, runtime::MXCSR));
                self.access(
                    crate::translate::lower::access::Addr::Window(address),
                    4,
                    crate::translate::lower::access::Dir::Store,
                    X13,
                );
                if self.checking {
                    self.reset_fp_track();
                }
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
        if simd.is_const_idiom() {
            let d = self.destination(simd.dst, false);
            match simd.kind {
                Kind::Logic(Logic::Eor) | Kind::AndNot => self.emit(vzero(full, d)),
                Kind::Same(Same::Cmeq, _) => {
                    self.emit_all(&[vzero(full, d), vnot(full, d, d)]);
                }
                _ => unreachable!(),
            }
            return self.written(simd.dst);
        }
        let s = match simd.src {
            Loc::Mem => {
                self.simd_access(vmem(simd.width), V1, address, false);
                V1
            }
            Loc::Imm => unreachable!("handled above"),
            loc => self.register(loc),
        };
        let d = self.destination(
            simd.dst,
            !matches!(simd.kind, Kind::ShuffleWords(_) | Kind::Shuffle(_)),
        );
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
                self.emit_shuffle(u32::from(log2), full, d, s, simd.imm);
            }
            Kind::Shufps => {
                if d == s {
                    self.emit_shuffle(2, true, d, s, simd.imm);
                } else {
                    self.emit_shufps(d, s, simd.imm);
                }
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

    /// Fast NEON lowering for `PSHUFD` (`log2 == 2`, `full == true`) and
    /// `PSHUFW` (`log2 == 1`, `full == false`), using 1-instruction permutes
    /// for common immediates and 1-register `TBL` otherwise.
    fn emit_shuffle(&mut self, log2: u32, full: bool, d: VReg, s: VReg, imm: u8) {
        let arr = Arr::new(log2, full);
        match imm {
            // Identity `[0, 1, 2, 3]`.
            0xe4 => {
                if d != s {
                    self.emit(vmov(full, d, s));
                }
            }
            // Broadcast `[k, k, k, k]`.
            0x00 | 0x55 | 0xaa | 0xff => {
                self.emit(vdup_lane(arr, d, s, u32::from(imm & 3)));
            }
            // 16-byte rotations (`PSHUFD` / same-reg `SHUFPS`).
            0x4e if full => self.emit(vext(d, s, s, 8)),
            0x39 if full => self.emit(vext(d, s, s, 4)),
            0x93 if full => self.emit(vext(d, s, s, 12)),
            // Standard NEON permutes (`[0,0,1,1]`, `[2,2,3,3]`, `[0,2,0,2]`,
            // `[1,3,1,3]`, `[0,0,2,2]`, `[1,1,3,3]`).
            0x50 => self.emit(vpermute(Permute::Zip1, arr, d, s, s)),
            0xfa => self.emit(vpermute(Permute::Zip2, arr, d, s, s)),
            0x88 => self.emit(vpermute(Permute::Uzp1, arr, d, s, s)),
            0xdd => self.emit(vpermute(Permute::Uzp2, arr, d, s, s)),
            0xa0 => self.emit(vpermute(Permute::Trn1, arr, d, s, s)),
            0xf5 => self.emit(vpermute(Permute::Trn2, arr, d, s, s)),
            // Duplicate low/high half (`[0,1,0,1]` / `[2,3,2,3]`).
            0x44 => self.emit(vdup_lane(Arr::new(log2 + 1, full), d, s, 0)),
            0xee => self.emit(vdup_lane(Arr::new(log2 + 1, full), d, s, 1)),
            _ => {
                let lanes = [imm & 3, (imm >> 2) & 3, (imm >> 4) & 3, (imm >> 6) & 3];
                if log2 == 2 {
                    let lane_bytes =
                        |base: u8| u32::from_le_bytes([base, base + 1, base + 2, base + 3]);
                    let lo = u64::from(lane_bytes(lanes[0] * 4))
                        | (u64::from(lane_bytes(lanes[1] * 4)) << 32);
                    let hi = u64::from(lane_bytes(lanes[2] * 4))
                        | (u64::from(lane_bytes(lanes[3] * 4)) << 32);
                    self.mov_u64(X16, lo);
                    self.mov_u64(X17, hi);
                    self.emit_all(&[
                        fmov_to_v(X, V0, X16),
                        vins_general(3, V0, 1, X17),
                        vtbl(true, 1, d, s, V0),
                    ]);
                } else {
                    let word_bytes = |base: u8| u16::from_le_bytes([base, base + 1]);
                    let mask = u64::from(word_bytes(lanes[0] * 2))
                        | (u64::from(word_bytes(lanes[1] * 2)) << 16)
                        | (u64::from(word_bytes(lanes[2] * 2)) << 32)
                        | (u64::from(word_bytes(lanes[3] * 2)) << 48);
                    self.mov_u64(X16, mask);
                    self.emit_all(&[fmov_to_v(X, V0, X16), vtbl(false, 1, d, s, V0)]);
                }
            }
        }
    }

    /// Fast NEON lowering for two-register `SHUFPS d, s, imm` (`d != s`),
    /// using 1-instruction permutes for common immediates and 2-register `TBL`
    /// otherwise.
    fn emit_shufps(&mut self, d: VReg, s: VReg, imm: u8) {
        match imm {
            // `[d0, d1, s2, s3]`
            0xe4 => self.emit(vins(3, d, 1, s, 1)),
            // `[d0, d1, s0, s1]`
            0x44 => self.emit(vpermute(Permute::Zip1, Arr::D2, d, d, s)),
            // `[d2, d3, s2, s3]`
            0xee => self.emit(vpermute(Permute::Zip2, Arr::D2, d, d, s)),
            // `[d2, d3, s0, s1]`
            0x4e => self.emit(vext(d, d, s, 8)),
            // `[d0, d2, s0, s2]`
            0x88 => self.emit(vpermute(Permute::Uzp1, Arr::S4, d, d, s)),
            // `[d1, d3, s1, s3]`
            0xdd => self.emit(vpermute(Permute::Uzp2, Arr::S4, d, d, s)),
            _ => {
                let lanes = [imm & 3, (imm >> 2) & 3, (imm >> 4) & 3, (imm >> 6) & 3];
                let (table_base, d_off, s_off) = if s.number() == d.number() + 1 {
                    (d, 0u8, 16u8)
                } else if d.number() == s.number() + 1 {
                    (s, 16u8, 0u8)
                } else {
                    self.emit(vmov(true, V0, d));
                    if s != V1 {
                        self.emit(vmov(true, V1, s));
                    }
                    (V0, 0u8, 16u8)
                };
                let lane_bytes =
                    |base: u8| u32::from_le_bytes([base, base + 1, base + 2, base + 3]);
                let lo = u64::from(lane_bytes(d_off + lanes[0] * 4))
                    | (u64::from(lane_bytes(d_off + lanes[1] * 4)) << 32);
                let hi = u64::from(lane_bytes(s_off + lanes[2] * 4))
                    | (u64::from(lane_bytes(s_off + lanes[3] * 4)) << 32);
                self.mov_u64(X16, lo);
                self.mov_u64(X17, hi);
                self.emit_all(&[
                    fmov_to_v(X, V2, X16),
                    vins_general(3, V2, 1, X17),
                    vtbl(true, 2, d, table_base, V2),
                ]);
            }
        }
    }

    /// Packed float-to-integer conversions use the interpreter's rounding and
    /// sticky exception rules, including integer indefinite on every invalid
    /// lane and Intel SDM §11.5.3.2 pre/post priority before unmasked `#XM`.
    fn packed_to_int(&mut self, simd: Simd, address: Reg, truncate: bool, double: bool) {
        use super::{X0, X1, X2};
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
        if self.features.afp && self.features.frintts {
            self.refresh_fp_track();
            let unit_mxcsr = self.unit_mxcsr_constant();
            if let Some(class) = unit_mxcsr
                && fp_state::sse_exceptions_masked(class)
            {
                let desired = fp_state::fpcr_from_mxcsr(class);
                self.emit_sync_fpcr_const(desired);
                self.emit_arm_fpsr_if_needed();
                self.set_fp_track(KnownFpcr::Const(desired), true, true);
                let d = self.destination(simd.dst, false);
                if double {
                    self.emit_all(&[
                        vfrint32(true, true, truncate, V0, s),
                        vfcvtzs_2d(V0, V0),
                        vxtn_2s(d, V0),
                    ]);
                } else {
                    let full = !matches!(simd.dst, Loc::Mmx(_));
                    self.emit_all(&[
                        vfrint32(full, false, truncate, V0, s),
                        vtoint(true, full, d, V0),
                    ]);
                }
                return self.written(simd.dst);
            }
            if unit_mxcsr.is_none()
                && self.vector.fp.all_masked
                && self.vector.fp.fpcr == KnownFpcr::MatchesMxcsr
                && self.vector.fp.fpsr_armed
            {
                self.vector.fp.valid_at = self.index as u32 + 1;
                let d = self.destination(simd.dst, false);
                if double {
                    self.emit_all(&[
                        vfrint32(true, true, truncate, V0, s),
                        vfcvtzs_2d(V0, V0),
                        vxtn_2s(d, V0),
                    ]);
                } else {
                    let full = !matches!(simd.dst, Loc::Mmx(_));
                    self.emit_all(&[
                        vfrint32(full, false, truncate, V0, s),
                        vtoint(true, full, d, V0),
                    ]);
                }
                return self.written(simd.dst);
            }
            if unit_mxcsr.is_none() {
                let (slow, done) = (self.asm.label(), self.asm.label());
                self.cpu();
                self.emit_all(&[
                    ldr(W, X13, X5, runtime::MXCSR),
                    and_imm(W, X17, X13, u64::from(fp_state::MXCSR_ALL_MASKED)),
                    eor_imm(W, X17, X17, u64::from(fp_state::MXCSR_ALL_MASKED)),
                ]);
                self.asm.cbz(W, true, X17, slow);
                self.emit_sync_fpcr_from_mxcsr(X13);
                fp_state::emit_arm_sse_fpsr(self.asm, X17);
                if double {
                    self.emit_all(&[
                        vfrint32(true, true, truncate, V0, s),
                        vfcvtzs_2d(V0, V0),
                        vxtn_2s(V0, V0),
                    ]);
                } else {
                    let full = !matches!(simd.dst, Loc::Mmx(_));
                    self.emit_all(&[
                        vfrint32(full, false, truncate, V0, s),
                        vtoint(true, full, V0, V0),
                    ]);
                }
                self.asm.b(done);
                self.asm.bind(slow);
                fp_state::emit_fold_fpsr(self.asm, X0, X1, X2);
                fp_state::emit_restore_host_fpcr(self.asm, X0, X1);
                self.emit(ldr(W, X13, X5, runtime::MXCSR));
                self.packed_to_int_slow(simd, s, truncate, double);
                self.asm.bind(done);
                self.reset_fp_track();
                let d = self.destination(simd.dst, false);
                self.emit(vmov(simd.full(), d, V0));
                return self.written(simd.dst);
            }
            // Statically known unmasked SSE exception: keep exact pre/post path.
            fp_state::emit_fold_fpsr(self.asm, X0, X1, X2);
            fp_state::emit_restore_host_fpcr(self.asm, X0, X1);
            self.reset_fp_track();
            self.cpu();
            self.emit(ldr(W, X13, X5, runtime::MXCSR));
            self.packed_to_int_slow(simd, s, truncate, double);
            let d = self.destination(simd.dst, false);
            self.emit(vmov(simd.full(), d, V0));
            return self.written(simd.dst);
        }
        if self.features.afp {
            // `afp && !frintts`: fold lazy FPSR and restore host FPCR before
            // the non-FRINTTS path reads MXCSR or clears FPSR.
            fp_state::emit_fold_fpsr(self.asm, X0, X1, X2);
            fp_state::emit_restore_host_fpcr(self.asm, X0, X1);
            self.reset_fp_track();
        }
        self.cpu();
        self.emit(ldr(W, X13, X5, runtime::MXCSR));
        let (slow, done) = (self.asm.label(), self.asm.label());
        // Fast paths require DAZ = 0, PM = 1, and (for non-truncating forms) RC = Nearest.
        self.asm.tbz(true, X13, 6, slow);
        self.asm.tbz(false, X13, 12, slow);
        if !truncate {
            self.emit(and_imm(W, X17, X13, 0x6000));
            self.asm.cbz(W, true, X17, slow);
        }
        // Tier A (zero-FPSR): when PE is already raised and every lane is finite and
        // strictly within the non-overflowing signed 32-bit range, convert directly in
        // NEON without serializing FPSR accesses.
        let after_zero_fpsr = if self.features.frintts {
            self.asm.label()
        } else {
            slow
        };
        let pe_known = !self.features.afp && self.vector.guards.fp_pe && self.trust_v8_guards();
        if !pe_known {
            self.asm.tbz(false, X13, 5, after_zero_fpsr);
        }
        if double {
            let hi_imm = if truncate { 0x41e0 } else { 0x41df };
            self.emit_all(&[
                movz(W, X14, hi_imm, 16),
                vumov(3, X16, s, 0),
                ubfx(X, X16, X16, 32, 31),
                sub(X, X16, X16, X14, Shift::Lsl, 0),
            ]);
            self.asm.tbz(false, X16, 63, after_zero_fpsr);
            self.emit_all(&[
                vumov(3, X17, s, 1),
                ubfx(X, X17, X17, 32, 31),
                sub(X, X17, X17, X14, Shift::Lsl, 0),
            ]);
            self.asm.tbz(false, X17, 63, after_zero_fpsr);
            let cvt = if truncate {
                vfcvtzs_2d(V0, s)
            } else {
                vfcvtns_2d(V0, s)
            };
            self.emit_all(&[cvt, vxtn_2s(V0, V0)]);
        } else {
            let full = !matches!(simd.dst, Loc::Mmx(_));
            if full {
                self.emit_all(&[
                    vshift(ShiftImm::Shl, Arr::S4, V2, s, 1),
                    umaxv(V2, V2),
                    fmov_from_v(W, X17, V2),
                    movz(W, X16, 0x9e00, 16),
                    sub(X, X17, X17, X16, Shift::Lsl, 0),
                ]);
                self.asm.tbz(false, X17, 63, after_zero_fpsr);
            } else {
                // Only inspect the low two lanes; upper lanes of XMM may hold NaN/Inf.
                self.emit_all(&[
                    movz(W, X14, 0x9e00, 16),
                    vumov(2, X16, s, 0),
                    lsl_imm(W, X16, X16, 1),
                    sub(X, X16, X16, X14, Shift::Lsl, 0),
                ]);
                self.asm.tbz(false, X16, 63, after_zero_fpsr);
                self.emit_all(&[
                    vumov(2, X17, s, 1),
                    lsl_imm(W, X17, X17, 1),
                    sub(X, X17, X17, X14, Shift::Lsl, 0),
                ]);
                self.asm.tbz(false, X17, 63, after_zero_fpsr);
            }
            self.emit(vtoint(truncate, full, V0, s));
        }
        self.asm.b(done);
        if self.features.frintts {
            // Tier B (FEAT_FRINTTS without AFP): handles PE = 0 and exact -2^31
            // in one vector FRINT32Z/FRINT32X + FCVTZS pair and one FPSR read.
            self.asm.bind(after_zero_fpsr);
            self.emit(msr_fpsr(Reg::ZR));
            if double {
                self.emit_all(&[
                    vfrint32(true, true, truncate, V0, s),
                    vfcvtzs_2d(V0, V0),
                    vxtn_2s(V0, V0),
                ]);
            } else {
                let full = !matches!(simd.dst, Loc::Mmx(_));
                self.emit_all(&[
                    vfrint32(full, false, truncate, V0, s),
                    vtoint(true, full, V0, V0),
                ]);
            }
            self.emit(mrs_fpsr(X17));
            runtime::emit_inc_fp_counter(self.asm, runtime::FP_FPSR_READS, 1, X16);
            self.asm.tbz(true, X17, 0, slow);
            if !pe_known {
                // FPSR.IXC (bit 4) maps directly to MXCSR.PE (bit 5).
                self.emit_all(&[
                    and_imm(W, X17, X17, 0x10),
                    orr(W, X13, X13, X17, Shift::Lsl, 1),
                    str(W, X13, X5, runtime::MXCSR),
                ]);
                runtime::emit_inc_fp_counter(self.asm, runtime::FP_FPSR_FOLDS, 1, X16);
            }
            self.asm.b(done);
        }
        self.asm.bind(slow);
        self.packed_to_int_slow(simd, s, truncate, double);
        self.asm.bind(done);
        let d = self.destination(simd.dst, false);
        self.emit(vmov(simd.full(), d, V0));
        self.written(simd.dst);
    }

    /// Exact per-lane float-to-int slow path with Intel SDM §11.5.3.2 pre/post
    /// exception priority. Expects `X5 = CpuState` and `X13 = MXCSR`.
    fn packed_to_int_slow(&mut self, simd: Simd, s: VReg, truncate: bool, double: bool) {
        use super::{X0, X1, X2, X6};
        self.emit(movz(W, X0, 0, 0));
        let temp = VReg::new(3);
        self.emit(vzero(true, V0));
        for lane in 0..if double || matches!(simd.dst, Loc::Mmx(_)) {
            2
        } else {
            4
        } {
            let (normal, converted, invalid) =
                (self.asm.label(), self.asm.label(), self.asm.label());
            self.emit(vumov(if double { 3 } else { 2 }, X16, s, lane));
            // Float-to-int conversions never raise DE; only DAZ=1 flushes subnormals to ±0.
            self.asm.tbz(false, X13, 6, normal);
            self.emit(ubfx(
                if double { X } else { W },
                X17,
                X16,
                if double { 52 } else { 23 },
                if double { 11 } else { 8 },
            ));
            self.asm.cbz(W, true, X17, normal);
            self.emit(lsl_imm(if double { X } else { W }, X17, X16, 1));
            self.asm.cbz(if double { X } else { W }, false, X17, normal);
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
            self.emit_all(&[fcvt_int(W, true, X6, V2), mrs_fpsr(X17)]);
            runtime::emit_inc_fp_counter(self.asm, runtime::FP_FPSR_READS, 1, X16);
            self.asm.tbz(true, X17, 0, invalid);
            self.emit_all(&[fmov_from_v(X, X17, V2), eor(X, X17, X17, X1, Shift::Lsl, 0)]);
            self.asm.cbz(X, false, X17, converted);
            self.emit(orr_imm(W, X0, X0, 0x20));
            self.asm.b(converted);
            self.asm.bind(invalid);
            self.emit_all(&[movz(W, X6, 0x8000, 16), orr_imm(W, X0, X0, 1)]);
            self.asm.bind(converted);
            self.emit(vins_general(2, V0, lane, X6));
        }
        // Commit sticky flags even on an unmasked exception, respecting Intel
        // SDM §11.5.3.2 priority: if pre-computation IE (bit 0) is unmasked,
        // commit only IE before #XM rather than also committing PE from another lane.
        let bail = self.architectural_exception(19);
        let commit_flags = self.asm.label();
        self.emit_all(&[lsr_imm(W, X1, X13, 7), bic(W, X2, X0, X1, Shift::Lsl, 0)]);
        self.asm.tbz(false, X2, 0, commit_flags);
        self.emit(and_imm(W, X0, X0, 1));
        self.asm.bind(commit_flags);
        self.emit_all(&[
            orr(W, X13, X13, X0, Shift::Lsl, 0),
            str(W, X13, X5, runtime::MXCSR),
        ]);
        runtime::emit_inc_fp_counter(self.asm, runtime::FP_FPSR_FOLDS, 1, X16);
        self.asm.cbz(W, true, X2, bail);
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
        if self.checking {
            // Checked access slow path calls `stubs.access`, which restores
            // `host_fpcr` and may fold/clear `fpsr_owner`.
            self.reset_fp_track();
        }
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
        use super::{X0, X1, X2, X3};
        let source = if simd.src == Loc::Mem {
            self.simd_access(if scalar { VMem::S } else { VMem::Q }, V1, address, false);
            V1
        } else {
            self.register(simd.src)
        };
        if self.features.afp && self.features.rpres {
            // Under FPCR.AH=1 + FEAT_RPRES, `frecpe`/`frsqrte` match the exact
            // 12-bit x86 reciprocal tables across all 2^32 inputs without
            // raising any FPSR flags or depending on FIZ/FZ/RMode (A64F-FP-9).
            self.ensure_ah1_for_rpres();
            let destination = self.destination(simd.dst, scalar);
            if scalar {
                self.emit_all(&[
                    sfrecpe(root, false, V0, source),
                    vins(2, destination, 0, V0, 0),
                ]);
            } else {
                self.emit(vfrecpe(root, true, false, destination, source));
            }
            return self.written(simd.dst);
        }
        self.canonicalize();
        // Approximations ignore MXCSR and never publish exceptions; the exact
        // 12-bit integer closed form matches x86/RPRES without touching FPCR.
        self.emit_all(&[mrs_nzcv(X13), movz(W, X0, 0x7f80, 16)]);
        if root {
            // 1u64 << 39
            self.emit(movz(X, X2, 0x0080, 32));
        } else {
            // exp >= 253 threshold (0x7e80_0000) and 1u32 << 26 (0x0400_0000)
            self.emit_all(&[movz(W, X2, 0x7e80, 16), movz(W, X3, 0x0400, 16)]);
        }
        for lane in 0..if scalar { 1 } else { 4 } {
            let [nan, small, negative, zero, finish] = std::array::from_fn(|_| self.asm.label());
            self.emit_all(&[
                vumov(2, X16, source, lane),
                and_imm(W, X17, X16, 0x7fff_ffff),
                and_imm(W, X1, X16, 1 << 31),
                cmp(W, X17, X0, Shift::Lsl, 0),
            ]);
            self.asm.b_cond(Cond::Hi, nan);
            self.emit(cmp_imm(W, X17, 0x800, true));
            self.asm.b_cond(Cond::Lo, small);
            if root {
                self.asm.tbz(true, X16, 31, negative);
                self.emit(cmp(W, X17, X0, Shift::Lsl, 0));
                self.asm.b_cond(Cond::Eq, zero);
                // b = (2048 + 2*i + 1) << ((e & 1) ^ 1), q = (1 << 39) / b.
                // 3 Newton steps from 12288 + exactness fixup yield floor(sqrt(q))
                // for every b in [2049, 8190]; sig = (r + 1) >> 1.
                self.emit_all(&[
                    ubfx(X, X6, X16, 13, 10),
                    movz(X, X14, 2049, 0),
                    add(X, X6, X14, X6, Shift::Lsl, 1),
                    ubfx(W, X14, X16, 23, 1),
                    cmp_imm(W, X14, 0, false),
                    lsl_imm(X, X14, X6, 1),
                    csel(X, X6, X14, X6, Cond::Eq),
                    udiv(X, X14, X2, X6),
                    movz(X, X6, 12288, 0),
                    udiv(X, X3, X14, X6),
                    add(X, X6, X6, X3, Shift::Lsl, 0),
                    lsr_imm(X, X6, X6, 1),
                    udiv(X, X3, X14, X6),
                    add(X, X6, X6, X3, Shift::Lsl, 0),
                    lsr_imm(X, X6, X6, 1),
                    udiv(X, X3, X14, X6),
                    add(X, X6, X6, X3, Shift::Lsl, 0),
                    lsr_imm(X, X6, X6, 1),
                    mul(X, X3, X6, X6),
                    cmp(X, X3, X14, Shift::Lsl, 0),
                    sub_imm(X, X3, X6, 1, false),
                    csel(X, X6, X3, X6, Cond::Hi),
                    add_imm(W, X6, X6, 1, false),
                    lsr_imm(W, X6, X6, 1),
                    and_imm(W, X6, X6, 0xfff),
                    lsr_imm(W, X14, X17, 23),
                    movz(W, X17, 380, 0),
                    sub(W, X14, X17, X14, Shift::Lsl, 0),
                    lsr_imm(W, X14, X14, 1),
                    lsl_imm(W, X6, X6, 11),
                    orr(W, X6, X6, X14, Shift::Lsl, 23),
                ]);
            } else {
                self.emit(cmp(W, X17, X2, Shift::Lsl, 0));
                self.asm.b_cond(Cond::Hs, zero);
                // a = 4096 + 2*i + 1, sig = (((1 << 26) / a) + 1) >> 1.
                self.emit_all(&[
                    ubfx(W, X6, X16, 12, 11),
                    movz(W, X14, 4097, 0),
                    add(W, X6, X14, X6, Shift::Lsl, 1),
                    udiv(W, X6, X3, X6),
                    add_imm(W, X6, X6, 1, false),
                    lsr_imm(W, X6, X6, 1),
                    and_imm(W, X6, X6, 0xfff),
                    lsr_imm(W, X14, X17, 23),
                    movz(W, X17, 253, 0),
                    sub(W, X14, X17, X14, Shift::Lsl, 0),
                    orr(W, X6, X1, X6, Shift::Lsl, 11),
                    orr(W, X6, X6, X14, Shift::Lsl, 23),
                ]);
            }
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
            self.asm.bind(zero);
            self.emit(mov(W, X6, X1));
            self.asm.bind(finish);
            self.emit(vins_general(2, V0, lane, X6));
        }
        self.emit(msr_nzcv(X13));
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

    /// Arithmetic, conversions and binary32/64 compares.
    ///
    /// With `FEAT_AFP` and all six SSE exceptions masked, runs directly under
    /// `FPCR.AH = 1` with lazy FPSR sticky-bit accumulation (A64F-FP-5).
    /// Without `FEAT_AFP` (or for `ScalarToInt` without `FEAT_FRINTTS`), runs
    /// the ARMv8.0 fallback that never touches FPSR on the fast path and sends
    /// exceptional cases to the slow path.
    fn floating(&mut self, simd: Simd, address: Reg) {
        if self.features.afp
            && !(matches!(simd.kind, Kind::ScalarToInt(..)) && !self.features.frintts)
        {
            return self.floating_afp(simd, address);
        }
        if self.features.afp {
            // `ScalarToInt` with `afp && !frintts`: fold lazy FPSR and restore
            // host FPCR before entering the ARMv8.0 fallback.
            fp_state::emit_fold_fpsr(self.asm, super::X0, super::X1, super::X2);
            fp_state::emit_restore_host_fpcr(self.asm, super::X0, super::X1);
            self.reset_fp_track();
        }
        self.cpu();
        let bail = if self.tier1 {
            self.asm.label()
        } else {
            self.bail()
        };
        let (slow, resume) = (self.asm.label(), self.asm.label());
        let after = self.tier1.then(|| self.asm.label());
        let inexact = simd.inexact();
        let trust = !self.features.afp && self.trust_v8_guards();
        let guards = if trust {
            self.vector.guards
        } else {
            Guards {
                ready: self.vector.guards.ready,
                ..Guards::default()
            }
        };
        let first_pe = inexact && !guards.fp_pe;
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
        if inexact && !guards.fp_pe {
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
            kind: SlowKind::Legacy {
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
            },
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
            self.finish_flag_compare();
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

    /// Direct `FEAT_AFP` (`FPCR.AH = 1`) lowering for SSE arithmetic, `min`/`max`,
    /// compares, `ucomis*`/`comis*`, and conversions when all six SSE exceptions
    /// are masked (A64F-FP-5, A64-FLAG-6).
    fn floating_afp(&mut self, simd: Simd, address: Reg) {
        let s = match simd.src {
            Loc::Mem => {
                self.simd_access(vmem(simd.width), V1, address, false);
                V1
            }
            Loc::Gpr(_) => V1,
            loc => self.register(loc),
        };
        let d_in = match simd.kind {
            Kind::Float(..)
            | Kind::Double(..)
            | Kind::Compare(..)
            | Kind::MinMax(..)
            | Kind::FlagCompare(..) => Some(self.register(simd.dst)),
            _ => None,
        };
        if let Kind::ScalarToInt(..) = simd.kind {
            let Loc::Gpr(index) = simd.dst else {
                unreachable!()
            };
            self.clobber(Operand::Guest(index));
        }
        let unmasked_after = self.ensure_afp_masked(simd, s);
        match simd.kind {
            Kind::Float(op, false) => {
                let d = d_in.unwrap();
                self.emit(vfloat(op, true, d, d, s));
                self.written(simd.dst);
            }
            Kind::Float(op, true) => {
                let d = d_in.unwrap();
                self.emit_all(&[sfloat(op, V0, d, s), vins(2, d, 0, V0, 0)]);
                self.written(simd.dst);
            }
            Kind::Double(op, false) => {
                let d = d_in.unwrap();
                self.emit(vfloat_2d(op, d, d, s));
                self.written(simd.dst);
            }
            Kind::Double(op, true) => {
                let d = d_in.unwrap();
                self.emit_all(&[dfloat(op, V0, d, s), vins(3, d, 0, V0, 0)]);
                self.written(simd.dst);
            }
            Kind::Sqrt(false, double) => {
                let d = self.destination(simd.dst, false);
                self.emit(fsqrt(false, double, d, s));
                self.written(simd.dst);
            }
            Kind::Sqrt(true, double) => {
                let d = self.destination(simd.dst, true);
                self.emit_all(&[
                    fsqrt(true, double, V0, s),
                    vins(if double { 3 } else { 2 }, d, 0, V0, 0),
                ]);
                self.written(simd.dst);
            }
            Kind::MinMax(max, false, double) => {
                let d = d_in.unwrap();
                self.emit(vfminmax(max, true, double, d, d, s));
                self.written(simd.dst);
            }
            Kind::MinMax(max, true, double) => {
                let d = d_in.unwrap();
                self.emit_all(&[
                    fminmax(max, double, V0, d, s),
                    vins(if double { 3 } else { 2 }, d, 0, V0, 0),
                ]);
                self.written(simd.dst);
            }
            Kind::Compare(scalar, double) => {
                let d = d_in.unwrap();
                let out = if scalar { V0 } else { d };
                match simd.imm & 7 {
                    0 | 4 => self.emit(vcompare(Float::Fcmeq, scalar, double, out, d, s)),
                    1 | 5 => self.emit(vcompare(Float::Fcmgt, scalar, double, out, s, d)),
                    2 | 6 => self.emit(vcompare(Float::Fcmge, scalar, double, out, s, d)),
                    3 | 7 => {
                        // `fcmeq(d, s)` produces the exact x86 FPSR flags for
                        // `cmpord`/`cmpunord` (IOC on SNaN, quiet on QNaN, IDC
                        // on denormal iff neither operand is NaN). Compute the
                        // ordered mask bitwise so `(QNaN, denormal)` never
                        // triggers a spurious `IDC` from `fcmeq(s, s)`.
                        let arr = if double { Arr::D2 } else { Arr::S4 };
                        let (v3, v4, v5) = (VReg::new(3), VReg::new(4), VReg::new(5));
                        self.emit(vcompare(Float::Fcmeq, scalar, double, V0, d, s));
                        if double {
                            self.emit(movz(X, X16, 0xffe0, 48));
                        } else {
                            self.emit(movz(W, X16, 0xff00, 16));
                        }
                        self.emit_all(&[
                            vdup(arr, V2, X16),
                            vshift(ShiftImm::Shl, arr, v3, d, 1),
                            vshift(ShiftImm::Shl, arr, v4, s, 1),
                            vsame(Same::Uqsub, arr, v3, v3, V2),
                            vsame(Same::Uqsub, arr, v4, v4, V2),
                            vlogic(Logic::Orr, true, v3, v3, v4),
                            vzero(true, v5),
                            vsame(Same::Cmeq, arr, out, v3, v5),
                        ]);
                    }
                    _ => unreachable!(),
                }
                if matches!(simd.imm & 7, 3..=6) {
                    self.emit(vnot(true, out, out));
                }
                if scalar {
                    self.emit(vins(if double { 3 } else { 2 }, d, 0, V0, 0));
                }
                self.written(simd.dst);
            }
            Kind::FlagCompare(double, signaling) => {
                let d = d_in.unwrap();
                self.emit(match (double, signaling) {
                    (false, false) => fcmps(d, s),
                    (false, true) => fcmpes(d, s),
                    (true, false) => fcmp(d, s),
                    (true, true) => fcmpe(d, s),
                });
                self.finish_flag_compare();
            }
            Kind::Widen(false) => {
                let d = self.destination(simd.dst, false);
                self.emit(vwiden(d, s));
                self.written(simd.dst);
            }
            Kind::Widen(true) => {
                let d = self.destination(simd.dst, true);
                self.emit_all(&[unary(Unary::Widen, V0, s), vins(3, d, 0, V0, 0)]);
                self.written(simd.dst);
            }
            Kind::Narrow(true) => {
                let d = self.destination(simd.dst, true);
                self.emit_all(&[unary(Unary::Narrow, V0, s), vins(2, d, 0, V0, 0)]);
                self.written(simd.dst);
            }
            Kind::Narrow(false) => {
                let d = self.destination(simd.dst, false);
                self.emit_all(&[
                    unary(Unary::Narrow, V0, s),
                    vins(3, V2, 0, s, 1),
                    unary(Unary::Narrow, V2, V2),
                    vins(2, V0, 1, V2, 0),
                    vmov(true, d, V0),
                ]);
                self.written(simd.dst);
            }
            Kind::ScalarFromInt(double) => {
                let gpr = match simd.src {
                    Loc::Gpr(index) => host(index),
                    Loc::Mem => {
                        self.emit(fmov_from_v(W, X16, s));
                        X16
                    }
                    _ => unreachable!(),
                };
                let d = self.destination(simd.dst, true);
                if double {
                    self.emit_all(&[scvtf(W, V0, gpr), vins(3, d, 0, V0, 0)]);
                } else {
                    self.emit_all(&[scvtf_s(W, V0, gpr), vins(2, d, 0, V0, 0)]);
                }
                self.written(simd.dst);
            }
            Kind::FromInt(false) => {
                if simd.width == 8 {
                    let d = self.register(simd.dst);
                    self.emit_all(&[vfromint(V0, s), vins(3, d, 0, V0, 0)]);
                } else {
                    let d = self.destination(simd.dst, false);
                    self.emit(vfromint(d, s));
                }
                self.written(simd.dst);
            }
            Kind::FromInt(true) => {
                let d = self.destination(simd.dst, false);
                self.emit_all(&[
                    vumov(2, X16, s, 0),
                    scvtf(W, V0, X16),
                    vumov(2, X16, s, 1),
                    scvtf(W, V2, X16),
                    vins(3, V0, 1, V2, 0),
                    vmov(true, d, V0),
                ]);
                self.written(simd.dst);
            }
            Kind::ScalarToInt(double, truncate) => {
                debug_assert!(self.features.frintts);
                let Loc::Gpr(index) = simd.dst else {
                    unreachable!()
                };
                let dst = super::ops::result(index);
                self.emit(sfrint_ts(false, double, truncate, V0, s));
                self.emit(if double {
                    fcvt_int(W, true, dst, V0)
                } else {
                    fcvt_int_s_mode(W, 3, dst, V0)
                });
                self.commit(index);
            }
            _ => unreachable!(),
        }
        self.vector.guards = simd.guarding(self.vector.guards);
        if let Some(after) = unmasked_after {
            self.asm.bind(after);
            self.reset_fp_track();
        }
    }

    /// Converts AArch64 `fcmp`/`fcmpe` NZCV flags into `State::CANONICAL`
    /// (`N=SF=0, Z=ZF, C=!CF, V=OF=0`, `x10 = !PF`, `x11 = AF = 0`) using
    /// `axflag` when `FEAT_FlagM2` is present or an out-of-line NaN fixup
    /// otherwise (A64-FLAG-6).
    fn finish_flag_compare(&mut self) {
        let need_nzcv = self
            .out
            .any(FlagSet::CF.or(FlagSet::ZF).or(FlagSet::SF).or(FlagSet::OF));
        let need_pf = self.out.any(FlagSet::PF);
        let need_af = self.out.any(FlagSet::AF);
        if self.features.flagm2 {
            if need_pf {
                self.emit(cset(W, super::X10, Cond::Vc));
            }
            if need_af {
                self.emit(movz(W, super::X11, 0, 0));
            }
            if need_nzcv {
                self.emit(AXFLAG);
            }
        } else if need_nzcv {
            let unordered = self.asm.label();
            let resume = self.asm.label();
            self.asm.b_cond(Cond::Vs, unordered);
            if self.out.any(FlagSet::SF) {
                if self.features.flagm {
                    self.emit(rmif(Reg::ZR, 0, 0b1000));
                } else {
                    self.emit_all(&[
                        mrs_nzcv(X16),
                        and_imm(X, X16, X16, !(1u64 << 31)),
                        msr_nzcv(X16),
                    ]);
                }
            }
            if need_pf {
                self.emit(movz(W, super::X10, 1, 0));
            }
            if need_af {
                self.emit(movz(W, super::X11, 0, 0));
            }
            self.asm.bind(resume);
            self.push_cold(Cold::Floating(Slow {
                kind: SlowKind::UnorderedFlags {
                    label: unordered,
                    resume,
                    need_nzcv,
                    need_pf,
                    need_af,
                },
            }));
        } else {
            if need_pf {
                self.emit(cset(W, super::X10, Cond::Vc));
            }
            if need_af {
                self.emit(movz(W, super::X11, 0, 0));
            }
        }
        self.state = super::State::CANONICAL;
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
        if matches!(
            simd.kind,
            Kind::FromInt(_) | Kind::ScalarFromInt(_) | Kind::ScalarToInt(..)
        ) {
            return;
        }
        let (v4, v5, v6) = (VReg::new(4), VReg::new(5), VReg::new(6));
        let double = simd.double();
        let inputs = operands.inputs;
        if simd.scalar() {
            let skip = if to.is_none()
                && let [Some(a), Some(b)] = inputs
            {
                // On x86, a NaN peer suppresses DE when reporting into MXCSR.
                let skip = self.asm.label();
                fp_class::nan_operands(self.asm, true, double, &[a, b], X16, [v4, v5], skip);
                Some(skip)
            } else {
                None
            };
            for input in inputs.into_iter().flatten() {
                fp_class::scalar_denormal(self.asm, double, input, X16, X17, to, X13);
            }
            if let Some(skip) = skip {
                self.asm.bind(skip);
            }
            return;
        }
        let arr = match simd.kind {
            Kind::Widen(false) => Arr::S2,
            _ if double => Arr::D2,
            _ => Arr::S4,
        };
        fp_class::packed_denormals(self.asm, arr, double, inputs, X16, [v4, v5, v6, V0]);
        if to.is_none()
            && let [Some(a), Some(b)] = inputs
        {
            // Suppress DE on any lane where either operand is NaN.
            self.emit_all(&[
                vcompare(Float::Fcmeq, false, double, v4, a, a),
                vcompare(Float::Fcmeq, false, double, v5, b, b),
                vlogic(Logic::And, true, v4, v4, v5),
                vlogic(Logic::And, true, v6, v6, v4),
            ]);
        }
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
            Kind::FlagCompare(double, signaling) => {
                let d = d.unwrap();
                self.emit(match (double, signaling) {
                    (false, false) => fcmps(d, s),
                    (false, true) => fcmpes(d, s),
                    (true, false) => fcmp(d, s),
                    (true, true) => fcmpe(d, s),
                });
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
        runtime::emit_inc_fp_counter(self.asm, runtime::FP_FPCR_WRITES, 2, X17);
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
        match cold.kind {
            SlowKind::UnorderedFlags {
                label,
                resume,
                need_nzcv,
                need_pf,
                need_af,
            } => {
                self.asm.bind(label);
                if need_nzcv {
                    // Canonical x86 unordered flags: SF=0, ZF=1, CF=1 (C=0), OF=0.
                    self.emit_all(&[movz(W, X16, 0x4000, 16), msr_nzcv(X16)]);
                }
                if need_pf {
                    self.emit(movz(W, super::X10, 0, 0));
                }
                if need_af {
                    self.emit(movz(W, super::X11, 0, 0));
                }
                self.asm.b(resume);
            }
            SlowKind::AfpUnmasked {
                label,
                after,
                simd,
                s,
                value,
            } => {
                self.asm.bind(label);
                self.state = value.state;
                self.vector = value.vector;
                self.simd_value(simd, ValueOperand::Loaded(s), value.x87, value.pairs);
                self.asm.b(after);
            }
            SlowKind::Legacy {
                simd,
                operands,
                slow,
                tiny,
                resume,
                bail,
                rounded,
                value,
            } => {
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
                runtime::emit_inc_fp_counter(self.asm, runtime::FP_FPSR_READS, 1, X16);
                self.asm.cbz(X, true, X14, bail);
                self.emit_all(&[
                    ubfx(W, X17, X17, 4, 1),
                    orr(W, X13, X13, X17, Shift::Lsl, 5),
                    str(W, X13, X5, runtime::MXCSR),
                ]);
                runtime::emit_inc_fp_counter(self.asm, runtime::FP_FPSR_FOLDS, 1, X16);
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
        }
    }

    fn reset_fp_track(&mut self) {
        self.vector.fp = FpTrack::default();
        self.invalidate_fp_state();
    }

    fn set_fp_track(&mut self, fpcr: KnownFpcr, fpsr_armed: bool, all_masked: bool) {
        self.vector.fp = FpTrack {
            valid_at: self.index as u32 + 1,
            fpcr,
            fpsr_armed,
            all_masked,
        };
        self.fpcr_mode = match fpcr {
            KnownFpcr::Unknown => FpcrMode::Unknown,
            KnownFpcr::Const(fp_state::FPCR_X87_DEFAULT) => FpcrMode::DefaultAfp,
            KnownFpcr::Const(val) => FpcrMode::Exact(val),
            KnownFpcr::MatchesMxcsr => FpcrMode::Exact(fp_state::FPCR_AH),
        };
        self.fpsr_armed = fpsr_armed;
    }

    /// Whether legacy ARMv8.0 `Guards` can be trusted without re-checking MXCSR.
    /// False only when a Tier-1 unit is lowered with `Features::afp = false`
    /// on an AFP host under a non-default MXCSR class (where `control_helper`
    /// and `v8_control_helper` diverge).
    fn trust_v8_guards(&self) -> bool {
        !(self.tier1
            && adv_cpu_core::HostFeatures::get().contains(adv_cpu_core::HostFeatures::AFP)
            && self
                .facts
                .mxcsr()
                .is_some_and(|class| !crate::facts::default_mxcsr(class)))
    }

    /// Returns the unit's entry MXCSR control class when no instruction in the
    /// trace can alter `MXCSR[15:6]`.
    fn unit_mxcsr_constant(&self) -> Option<u32> {
        use crate::translate::front::{decode::Op, trace::Mode};
        if !self.tier1 {
            return None;
        }
        let class = self.facts.mxcsr()?;
        let invariant = self.trace.insts.iter().all(|inst| {
            inst.call.is_none()
                && inst.mode != Mode::Real
                && !matches!(
                    inst.op,
                    Op::Value(_)
                        | Op::Body(_)
                        | Op::Syscall
                        | Op::Simd(Simd {
                            kind: Kind::LoadControl,
                            ..
                        })
                )
        });
        invariant.then_some(class)
    }

    /// Validates that `self.vector.fp` still holds across all straight-line
    /// trace instructions between `valid_at - 1` and `self.index`.
    fn refresh_fp_track(&mut self) {
        use crate::translate::front::{
            decode::Op,
            trace::{Mode, Next},
        };
        if self.vector.fp.valid_at == 0 || self.fpcr_mode == FpcrMode::Unknown {
            self.vector.fp = FpTrack::default();
            return;
        }
        let prev = (self.vector.fp.valid_at - 1) as usize;
        if prev >= self.index {
            self.vector.fp = FpTrack::default();
            return;
        }
        for i in prev + 1..=self.index {
            if self.trace.head(i)
                || self.trace.merge[i]
                || self.trace.insts[i - 1].next != Next::Fall
            {
                self.vector.fp = FpTrack::default();
                return;
            }
            if i == self.index {
                break;
            }
            let inst = self.trace.insts[i];
            let disturbs = inst.call.is_some()
                || inst.locked.is_some()
                || inst.mode == Mode::Real
                || (self.checked)(inst.pc)
                || matches!(
                    inst.op,
                    Op::X87(_)
                        | Op::Fild64(..)
                        | Op::Value(_)
                        | Op::Body(_)
                        | Op::Syscall
                        | Op::Timestamp
                        | Op::PopFlags(..)
                        | Op::Stack(..)
                        | Op::Call(_)
                        | Op::CallIndirect(_)
                        | Op::JmpIndirect(_)
                        | Op::Ret(_)
                );
            if disturbs {
                self.vector.fp = FpTrack::default();
                return;
            }
        }
        if !self.fpsr_armed {
            self.vector.fp.fpsr_armed = false;
        }
    }

    fn emit_sync_fpcr_const(&mut self, desired: u32) {
        if self.vector.fp.fpcr != KnownFpcr::Const(desired) {
            self.as_part(Part::SimdGuard, |t| {
                fp_state::emit_sync_fpcr_imm(t.asm, desired, X16, X17);
            });
        }
    }

    fn emit_arm_fpsr_if_needed(&mut self) {
        if !self.vector.fp.fpsr_armed {
            self.as_part(Part::SimdGuard, |t| {
                fp_state::emit_arm_sse_fpsr(t.asm, X17);
            });
        }
    }

    /// Computes hardware `FEAT_AFP` FPCR from MXCSR in `mxcsr_reg` (`X13`) into
    /// `X16` and lazily syncs hardware FPCR (preserving NZCV and `mxcsr_reg`).
    fn emit_sync_fpcr_from_mxcsr(&mut self, mxcsr_reg: Reg) {
        debug_assert!(mxcsr_reg != X16 && mxcsr_reg != X17);
        self.as_part(Part::SimdGuard, |t| {
            t.emit_all(&[
                // AH = 1 (bit 1), FIZ = MXCSR[6] (bit 0), FZ = MXCSR[15] (bit 24).
                movz(W, X16, fp_state::FPCR_AH as u16, 0),
                ubfx(W, X17, mxcsr_reg, 6, 1),
                orr(W, X16, X16, X17, Shift::Lsl, 0),
                ubfx(W, X17, mxcsr_reg, 15, 1),
                orr(W, X16, X16, X17, Shift::Lsl, 24),
                // RMode[23:22] = bit-reversed MXCSR[14:13] (swaps RD/RU).
                and_imm(W, X17, mxcsr_reg, 0x6000),
                rbit(W, X17, X17),
                lsl_imm(W, X17, X17, 5),
                orr(W, X16, X16, X17, Shift::Lsl, 0),
            ]);
            fp_state::emit_sync_fpcr(t.asm, X16, X17);
        });
    }

    /// Ensures `FPCR.AH = 1` is active for `frecpe`/`frsqrte` (`FEAT_RPRES`),
    /// skipping FPCR sync completely when any `AH = 1` mode is already tracked.
    fn ensure_ah1_for_rpres(&mut self) {
        self.refresh_fp_track();
        match self.vector.fp.fpcr {
            KnownFpcr::Const(_) | KnownFpcr::MatchesMxcsr => {
                // `frecpe`/`frsqrte` under `AH = 1` ignore FIZ/FZ/RMode and
                // never write FPSR, so existing tracking stays valid.
                self.vector.fp.valid_at = self.index as u32 + 1;
            }
            KnownFpcr::Unknown => {
                self.emit_sync_fpcr_const(fp_state::FPCR_X87_DEFAULT);
                self.set_fp_track(KnownFpcr::Const(fp_state::FPCR_X87_DEFAULT), false, false);
            }
        }
    }

    /// Ensures all six SSE exceptions are masked, hardware FPCR matches MXCSR
    /// under `FPCR.AH = 1`, and lazy SSE FPSR accumulation is armed.
    /// Returns `Some(after)` when a dynamic unmasked fallback path was emitted.
    fn ensure_afp_masked(&mut self, simd: Simd, s: VReg) -> Option<Label> {
        self.refresh_fp_track();
        if let Some(class) = self.unit_mxcsr_constant()
            && fp_state::sse_exceptions_masked(class)
        {
            let desired = fp_state::fpcr_from_mxcsr(class);
            self.emit_sync_fpcr_const(desired);
            self.emit_arm_fpsr_if_needed();
            self.set_fp_track(KnownFpcr::Const(desired), true, true);
            return None;
        }
        if self.vector.fp.all_masked
            && self.vector.fp.fpcr == KnownFpcr::MatchesMxcsr
            && self.vector.fp.fpsr_armed
        {
            self.vector.fp.valid_at = self.index as u32 + 1;
            return None;
        }
        self.cpu();
        let (unmasked, after) = if self.tier1 {
            let (u, a) = (self.asm.label(), self.asm.label());
            self.push_cold(Cold::Floating(Slow {
                kind: SlowKind::AfpUnmasked {
                    label: u,
                    after: a,
                    simd,
                    s,
                    value: ValueContinuation {
                        after: a,
                        state: self.state,
                        vector: self.vector,
                        x87: self.x87_leaving(),
                        pairs: self.x87_live_pairs(),
                        first_pe: false,
                    },
                },
            }));
            (u, Some(a))
        } else {
            (self.bail(), None)
        };
        self.as_part(Part::SimdGuard, |t| {
            t.emit_all(&[
                ldr(W, X13, X5, runtime::MXCSR),
                and_imm(W, X17, X13, u64::from(fp_state::MXCSR_ALL_MASKED)),
                eor_imm(W, X17, X17, u64::from(fp_state::MXCSR_ALL_MASKED)),
            ]);
            t.asm.cbz(W, true, X17, unmasked);
        });
        self.emit_sync_fpcr_from_mxcsr(X13);
        self.as_part(Part::SimdGuard, |t| {
            fp_state::emit_arm_sse_fpsr(t.asm, X17);
        });
        if after.is_none() {
            self.set_fp_track(KnownFpcr::MatchesMxcsr, true, true);
        }
        after
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
        let mut owner_alias = self.tier1;
        #[cfg(native)]
        {
            owner_alias |= self
                .computation_facts
                .is_some_and(|facts| facts.scope_access.is_some() || facts.scope_mmx != 0);
        }
        if owner_alias {
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

    #[cfg(native)]
    fn scope_mmx_write(&mut self, bits: u16) {
        let mask = self.computation_facts.map_or(0, |facts| facts.scope_mmx) & bits;
        if mask != 0 {
            self.emit_all(&[
                ldr(W, X17, RUNTIME, runtime::SCOPE_WRITTEN),
                orr_imm(W, X17, X17, u64::from(mask)),
                str(W, X17, RUNTIME, runtime::SCOPE_WRITTEN),
            ]);
        }
    }

    /// Defers the write until an edge or observer publishes it.
    fn written(&mut self, loc: Loc) {
        match loc {
            Loc::Mmx(n) => {
                #[cfg(native)]
                self.scope_mmx_write(1 << n);
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
    /// merge's entry (`translate::jit::residency`).
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
        if restore {
            self.reset_fp_track();
        }
        self.vector.cpu = false;
    }

    /// Publishes the SIMD prefix without changing flags, addresses, x5 or SIMD
    /// temporaries. x0/x3/x4 are free at control and observer boundaries.
    pub(super) fn invalidate_mmx(&mut self) {
        self.vector.mmx = 0;
        self.vector.guards.ready = false;
        self.aliased = 0;
        self.reset_fp_track();
    }

    pub(super) fn call_prepared_value(
        &mut self,
        helper: u8,
        target: stubs::ValueTarget,
        pairs: Option<u8>,
    ) {
        let resets_fp = matches!(target, stubs::ValueTarget::Float(_));
        let index = self.helpers + u32::from(helper);
        if let (Some(x87), Some(_), stubs::ValueTarget::Float(access)) =
            (self.x87_stubs(), pairs, &target)
        {
            let desc = match *access {
                None => 0u32,
                Some(adv_cpu_core::floating::ValueAccess::Read(w)) => u32::from(w) | (1 << 16),
                Some(adv_cpu_core::floating::ValueAccess::Write(w)) => u32::from(w) | (2 << 16),
            };
            self.mov_imm(X1, index);
            self.mov_imm(X2, desc);
            self.far_call(x87.value_call);
            self.reset_fp_track();
            return;
        }
        stubs::emit_value_call(self.asm, index, self.vector.xmm, pairs, target);
        if resets_fp {
            self.reset_fp_track();
        }
    }

    pub(super) fn publish_fx_image(&mut self) {
        if self.features.afp {
            // Fold lazy SSE sticky bits before FXSAVE reads `CpuState::mxcsr`.
            fp_state::emit_fold_fpsr(self.asm, super::X0, super::X1, super::X2);
            self.vector.fp.fpsr_armed = false;
            self.fpsr_armed = false;
        }
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
        if self.features.afp {
            // FXRSTOR replaced MXCSR: ensure uncommitted FPSR is discarded and
            // cached FPCR assumptions are cleared.
            fp_state::emit_discard_fpsr(self.asm);
            self.reset_fp_track();
        }
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
        use crate::native::compile::{Workspace, translate};
        use crate::translate::{Features, Stubs};
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
            Features {
                flagm: false,
                ..Features::default()
            },
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
                x87: None,
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
                .all(|i| !matches!(i.op, crate::translate::front::decode::Op::Value(_)))
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
        for (insn, expected_write) in [
            (decode(|a| a.pxor(xmm0, xmm0).unwrap()), 1u16 << 8),
            (decode(|a| a.xorps(xmm1, xmm1).unwrap()), 1u16 << 9),
            (decode(|a| a.xorpd(xmm2, xmm2).unwrap()), 1u16 << 10),
            (decode(|a| a.pandn(xmm3, xmm3).unwrap()), 1u16 << 11),
            (decode(|a| a.andnps(xmm4, xmm4).unwrap()), 1u16 << 12),
            (decode(|a| a.andnpd(xmm5, xmm5).unwrap()), 1u16 << 13),
            (decode(|a| a.pcmpeqb(xmm0, xmm0).unwrap()), 1u16 << 8),
            (decode(|a| a.pcmpeqw(xmm1, xmm1).unwrap()), 1u16 << 9),
            (decode(|a| a.pcmpeqd(xmm2, xmm2).unwrap()), 1u16 << 10),
            (decode(|a| a.pxor(mm3, mm3).unwrap()), 1u16 << 3),
            (decode(|a| a.pandn(mm4, mm4).unwrap()), 1u16 << 4),
            (decode(|a| a.pcmpeqd(mm5, mm5).unwrap()), 1u16 << 5),
        ] {
            assert_eq!(plan(&insn, 0).unwrap().footprint(), (0, expected_write));
        }
    }
}
