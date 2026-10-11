//! MMX and SSE integer and move instructions in NEON (workbook §4, §8).
//!
//! A unit caches MMX registers in d8-d15 and XMM registers in v16-v23.
//! A register is loaded from `CpuState` when the unit first reads it and
//! stored back whenever an instruction writes it, so `CpuState` is always
//! current: fault sites, exits and helpers need no spill. An MMX write
//! also stores the x87 register it aliases, with the exponent of an MMX
//! value. The cache is forgotten where control can arrive from elsewhere
//! (region heads and merges) and after a helper, which may change any
//! register; d8-d15 survive the Rust calls between, and the `access`
//! stub keeps v16-v23.
//!
//! The first MMX instruction after the cache is forgotten leaves for the
//! interpreter if an x87 exception is pending, and once it completes sets
//! TOP and every tag to 0, as each MMX instruction does. `movaps`-style
//! accesses that are not 16-byte aligned leave for the interpreter too,
//! which raises the fault. The cpu pointer stays in x5 across a run of
//! SIMD instructions.

use iced_x86::{Instruction, Mnemonic, OpKind};

use crate::encode::Size::{W, X};
use crate::encode::*;
use crate::runtime::{self, Exit};

use super::decode::{Mem, memory};
use super::flags::Operand;
use super::{Cold, RUNTIME, Translator, WINDOW, X5, X14, X16, X17, host};

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
    /// `movss` between registers: the low lane only.
    MoveLow,
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
    /// The helper that runs the instruction when it must be checked.
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

    /// Whether the destination is 128 bits wide.
    fn full(&self) -> bool {
        matches!(self.dst, Loc::Xmm(_))
    }
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
        M::Movss if matches!((dst, src), (Loc::Xmm(_), Loc::Xmm(_))) => Kind::MoveLow,
        M::Movss => Kind::Move,
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
        Kind::Move => match (d, s) {
            (1 | 2, 0) | (0, 1 | 2) | (1 | 2, 3) | (3, 1 | 2) => true,
            _ => d == s && matches!(d, 1 | 2),
        },
        _ => matches!(d, 1 | 2) && (s == d || src == Loc::Mem || src == Loc::Imm),
    };
    let three = matches!(kind, Kind::Shuffle(_) | Kind::Shufps);
    let imm_ok = match kind {
        Kind::ShiftImm(..) => instruction.op_count() == 2,
        _ if three => instruction.op_count() == 3 && instruction.op_kind(2) == OpKind::Immediate8,
        _ => src != Loc::Imm || kind == Kind::Emms,
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
            M::Movq => 8,
            _ => 16,
        }
    };
    let aligned = (dst == Loc::Mem || src == Loc::Mem)
        && size == 16
        && !matches!(mnemonic, M::Movups | M::Movupd | M::Movdqu);
    let imm = match kind {
        Kind::ShiftImm(..) => instruction.immediate8(),
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
    /// XMM registers in v16-v23, by bit.
    xmm: u8,
    /// No x87 exception is pending, and TOP and the tags are 0.
    ready: bool,
    /// x5 holds the `CpuState` pointer.
    pub cpu: bool,
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
        if self.checking && simd.memory() {
            self.helper(simd.helper);
            return;
        }
        let mmx = simd.mmx();
        let ready = self.vector.ready;
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
            self.vector.ready = true;
        }
    }

    fn simd_body(&mut self, simd: Simd, address: Reg) {
        let full = simd.full();
        match simd.kind {
            Kind::Emms => {
                self.cpu();
                self.emit_all(&[movz(W, X17, 0xffff, 0), strh(X17, X5, runtime::X87_TAG)]);
                self.vector.ready = false;
                return;
            }
            Kind::Move => return self.simd_move(simd, address),
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
                        self.site();
                        self.emit(ldr_reg(X, X16, WINDOW, address, false));
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
                self.site();
                self.emit(vldr_reg(vmem(simd.width), V1, WINDOW, address));
                V1
            }
            Loc::Imm => unreachable!("handled above"),
            loc => self.register(loc),
        };
        let d = self.destination(simd.dst, true);
        match simd.kind {
            Kind::MoveLow => self.emit(vins(2, d, 0, s, 0)),
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
            Kind::Move | Kind::ShiftImm(..) | Kind::ShiftBy(..) | Kind::Emms => unreachable!(),
        }
        self.written(simd.dst);
    }

    fn simd_move(&mut self, simd: Simd, address: Reg) {
        let size = vmem(simd.width);
        match (simd.dst, simd.src) {
            (Loc::Mem, src) => {
                let s = self.register(src);
                self.site();
                self.emit(vstr_reg(size, s, WINDOW, address));
            }
            (Loc::Gpr(index), src) => {
                let s = self.register(src);
                self.clobber(Operand::Guest(index));
                self.emit(fmov_from_v(W, super::ops::result(index), s));
                self.commit(index);
            }
            (dst, Loc::Mem) => {
                let d = self.destination(dst, false);
                self.site();
                self.emit(vldr_reg(size, d, WINDOW, address));
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

    /// Stores a destination register written in its host register.
    fn written(&mut self, loc: Loc) {
        self.cpu();
        match loc {
            Loc::Mmx(n) => {
                self.vector.mmx |= 1 << n;
                let v = VReg::new(8 + n);
                let x87 = runtime::X87_REGS + 10 * u32::from(n);
                self.at_cpu(false, VMem::D, v, runtime::MMX + 8 * u32::from(n));
                self.at_cpu(false, VMem::D, v, x87);
                self.emit(movz(W, X17, 0xffff, 0));
                self.half_at_cpu(X17, x87 + 8);
            }
            Loc::Xmm(n) => {
                self.vector.xmm |= 1 << n;
                let offset = runtime::XMM + 16 * u32::from(n);
                self.at_cpu(false, VMem::Q, VReg::new(16 + n), offset);
            }
            _ => unreachable!("not a SIMD register"),
        }
    }

    /// Loads the `CpuState` pointer into x5 unless it is there.
    fn cpu(&mut self) {
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

    /// Stores the low half of `rt` at `offset` into `CpuState`; uses x16.
    fn half_at_cpu(&mut self, rt: Reg, offset: u32) {
        if offset.is_multiple_of(2) {
            self.emit(strh(rt, X5, offset));
        } else if offset < 256 {
            self.emit(sturh(rt, X5, offset as i32));
        } else {
            self.emit_all(&[add_imm(X, X16, X5, offset, false), strh(rt, X16, 0)]);
        }
    }

    /// A label that leaves for the interpreter to run the instruction.
    fn bail(&mut self) -> crate::asm::Label {
        let label = self.asm.label();
        let inst = self.trace.insts[self.index];
        self.cold.push(Cold::Leave {
            label,
            state: self.state,
            refund: self.trace.owed(self.index),
            exit: Exit::Interpret,
            eip: inst.pc,
            link: false,
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
        assert_eq!(low.kind, Kind::MoveLow);
        assert!(plan(&decode(|a| a.movq2dq(xmm1, mm2).unwrap()), 0).is_none());
        assert!(plan(&decode(|a| a.addps(xmm1, xmm2).unwrap()), 0).is_none());
        assert!(plan(&decode(|a| a.emms().unwrap()), 0).unwrap().mmx());
    }
}
