//! Native integer forms over partial registers and memory (A64-TR-4).
//! Operands use the ordinary access emitter. Results and flags commit only
//! after the last access, including zero-count memory shifts (A64-FLAG-4).

use adv_cpu_core::integer::{self, Operand, Operation};
use iced_x86::{Instruction, Mnemonic as M, OpKind, Register};

use crate::encode::Size::{W, X};
use crate::encode::*;

use super::decode::{Alu, Mem, Part, memory_sized};
use super::{State, Translator, X0, X1, X2, X6, X10, X11, X13, X14, host};

const ECX: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// The masked immediate count, 0 to 31.
    Imm(u8),
    Cl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parts {
    pub kind: M,
    /// 1, 2 or 4 bytes.
    pub width: u8,
    pub dst: Part,
    /// A binary source, or the register a double shift fills from, and its width.
    pub source: Option<(Part, u8)>,
    /// A shift's count.
    pub count: Option<Count>,
}

impl Parts {
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

/// The native plan for `instruction`, if one does.
pub fn plan(instruction: &Instruction) -> Option<Parts> {
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

impl Translator<'_> {
    pub(super) fn parts(&mut self, op: Parts) {
        use super::{X5, X15};
        use crate::encode::Shift::Lsl;
        let x7 = Reg::new(7);
        let x8 = Reg::new(8);
        let kind = op.kind;
        let alu = match kind {
            M::Add => Some(Alu::Add),
            M::Sub => Some(Alu::Sub),
            M::And => Some(Alu::And),
            M::Or => Some(Alu::Or),
            M::Xor => Some(Alu::Xor),
            M::Cmp => Some(Alu::Cmp),
            M::Test => Some(Alu::Test),
            _ => None,
        };
        if let Some(alu) = alu {
            self.narrow_op(Some(alu), op.width, op.dst, op.source.unwrap().0, false);
            return;
        }
        self.canonicalize();
        let address = match op.memory() {
            Some(mem) => {
                let address = self.effective(mem);
                self.emit(mov(W, X5, address));
                X5
            }
            None => Reg::ZR,
        };
        self.narrow_read(op.dst, op.width, address, x7);
        if let Some((source, width)) = op.source {
            self.narrow_read(source, width, address, x8);
        }
        self.emit_all(&[mrs_nzcv(X14), mov(X, X15, X14)]);
        let bits = 8 * u32::from(op.width);
        let (zero, done) = (self.asm.label(), self.asm.label());
        let shift = op.count.is_some();
        if let Some(count) = op.count {
            match count {
                Count::Imm(count) => self.mov_imm(X13, u32::from(count)),
                Count::Cl => self.emit(and_imm(W, X13, host(ECX), 31)),
            }
            self.asm.cbz(W, false, X13, zero);
            self.native_shift(kind, bits);
        } else {
            // Work at full width, including the incoming carry; narrow once.
            self.emit_all(&[ubfx(X, X0, X14, 29, 1), eor_imm(X, X0, X0, 1)]);
            let sub = kind == M::Sbb;
            self.emit(if sub {
                sub_reg(X, X6, x7, x8)
            } else {
                add(X, X6, x7, x8, Lsl, 0)
            });
            self.emit(if sub {
                sub_reg(X, X6, X6, X0)
            } else {
                add(X, X6, X6, X0, Lsl, 0)
            });
            self.emit(ubfx(X, X1, X6, if sub { 63 } else { bits }, 1));
            self.emit_all(&[
                eor_imm(X, X1, X1, 1),
                bfi(X, X15, X1, 29, 1),
                sbfx(X, X1, x7, 0, bits),
                sbfx(X, X2, x8, 0, bits),
            ]);
            self.emit(if sub {
                sub_reg(X, X1, X1, X2)
            } else {
                add(X, X1, X1, X2, Lsl, 0)
            });
            self.emit(if sub {
                sub_reg(X, X1, X1, X0)
            } else {
                add(X, X1, X1, X0, Lsl, 0)
            });
            self.emit_all(&[
                sbfx(X, X2, X6, 0, bits),
                cmp(X, X1, X2, Lsl, 0),
                cset(W, X1, Cond::Ne),
                bfi(X, X15, X1, 28, 1),
            ]);
        }
        self.emit(ubfx(W, X6, X6, 0, bits));
        let rotate = matches!(kind, M::Rol | M::Ror | M::Rcl | M::Rcr);
        if !rotate {
            self.emit_all(&[
                lsl_imm(W, X0, X6, 32 - bits),
                adds(W, Reg::ZR, Reg::ZR, X0, Lsl, 0),
                mrs_nzcv(X0),
                ubfx(X, X0, X0, 30, 2),
                bfi(X, X15, X0, 30, 2),
            ]);
        }
        self.emit(msr_nzcv(X14));
        self.narrow_write(op.dst, op.width, address, X6);
        if !rotate {
            self.emit(mov(W, X10, X6));
        }
        if !shift {
            self.emit_all(&[eor(W, X11, x7, x8, Lsl, 0), eor(W, X11, X11, X6, Lsl, 0)]);
        }
        self.emit(msr_nzcv(X15));
        self.asm.b(done);
        self.asm.bind(zero);
        self.emit(msr_nzcv(X14));
        self.narrow_write(op.dst, op.width, address, x7);
        self.asm.bind(done);
        self.state = State::CANONICAL;
    }

    /// Count in w13, operands in x7/x8; produce x6 and canonical NZCV in x15.
    fn native_shift(&mut self, kind: M, bits: u32) {
        use super::{X15, X16, X17};
        use crate::encode::Shift::{Lsl, Lsr};
        let x7 = Reg::new(7);
        let x8 = Reg::new(8);
        let (carry_done, overflow_done) = (self.asm.label(), self.asm.label());
        match kind {
            M::Shl | M::Shr | M::Sar => {
                if kind == M::Sar {
                    self.emit(sbfx(X, X0, x7, 0, bits));
                }
                self.emit(match kind {
                    M::Shl => shiftv(X, Lsl, X6, x7, X13),
                    M::Shr => shiftv(X, Lsr, X6, x7, X13),
                    _ => shiftv(X, crate::encode::Shift::Asr, X6, X0, X13),
                });
                if kind == M::Sar {
                    self.emit_all(&[
                        sub_imm(W, X1, X13, 1, false),
                        shiftv(X, crate::encode::Shift::Asr, X1, X0, X1),
                    ]);
                } else {
                    self.emit(cmp_imm(W, X13, bits, false));
                    self.asm.b_cond(Cond::Hi, carry_done);
                    if kind == M::Shl {
                        self.mov_imm(X1, bits);
                        self.emit(sub_reg(W, X1, X1, X13));
                    } else {
                        self.emit(sub_imm(W, X1, X13, 1, false));
                    }
                    self.emit(shiftv(X, Lsr, X1, x7, X1));
                }
            }
            M::Rol | M::Ror | M::Rcl | M::Rcr => {
                let through = matches!(kind, M::Rcl | M::Rcr);
                let left = matches!(kind, M::Rol | M::Rcl);
                let span = bits + u32::from(through);
                self.emit(mov(W, X0, x7));
                if through {
                    self.emit_all(&[
                        ubfx(X, X1, X14, 29, 1),
                        eor_imm(X, X1, X1, 1),
                        orr(X, X0, X0, X1, Lsl, bits),
                    ]);
                }
                self.mov_imm(X1, span);
                self.emit_all(&[udiv(W, X2, X13, X1), msub(W, X2, X2, X1, X13)]);
                if through {
                    self.emit(mov(W, X6, x7));
                    self.asm.cbz(W, false, X2, carry_done);
                }
                self.emit_all(&[
                    sub_reg(W, X1, X1, X2),
                    shiftv(X, if left { Lsl } else { Lsr }, X6, X0, X2),
                    shiftv(X, if left { Lsr } else { Lsl }, X1, X0, X1),
                    orr(X, X6, X6, X1, Lsl, 0),
                ]);
                self.emit(ubfx(
                    X,
                    X1,
                    X6,
                    if through {
                        bits
                    } else if left {
                        0
                    } else {
                        bits - 1
                    },
                    1,
                ));
            }
            M::Shld | M::Shrd => {
                let left = kind == M::Shld;
                self.emit(if left {
                    orr(X, X0, x8, x7, Lsl, bits)
                } else {
                    orr(X, X0, x7, x8, Lsl, bits)
                });
                if bits == 16 {
                    if left {
                        self.emit(sub_reg(W, X1, Reg::ZR, X13));
                    } else {
                        self.emit(mov(W, X1, X13));
                    }
                    self.emit(rorv(W, X6, X0, X1));
                    if left {
                        self.emit(lsr_imm(W, X6, X6, 16));
                    }
                } else if left {
                    self.emit_all(&[shiftv(X, Lsl, X6, X0, X13), lsr_imm(X, X6, X6, 32)]);
                } else {
                    self.emit(shiftv(X, Lsr, X6, X0, X13));
                }
                if left {
                    self.mov_imm(X1, bits * 2);
                    self.emit(sub_reg(W, X1, X1, X13));
                } else {
                    self.emit(sub_imm(W, X1, X13, 1, false));
                }
                self.emit(shiftv(X, Lsr, X1, X0, X1));
            }
            _ => unreachable!("native integer form"),
        }
        self.emit_all(&[
            and_imm(W, X1, X1, 1),
            eor_imm(W, X1, X1, 1),
            bfi(X, X15, X1, 29, 1),
        ]);
        self.asm.bind(carry_done);
        // RCL/RCR with a count divisible by the ring width change nothing.
        if matches!(kind, M::Rcl | M::Rcr) {
            self.emit(mov(W, X16, X6));
            self.emit(cmp_imm(W, X2, 0, false));
            self.emit(csel(W, X6, x7, X16, Cond::Eq));
        }
        self.emit(cmp_imm(W, X13, 1, false));
        self.asm.b_cond(Cond::Ne, overflow_done);
        match kind {
            M::Shr => self.emit(lsr_imm(W, X17, x7, bits - 1)),
            M::Sar => self.emit(movz(W, X17, 0, 0)),
            M::Rol | M::Rcl => self.emit_all(&[
                lsr_imm(W, X17, X6, bits - 1),
                ubfx(W, X16, X15, 29, 1),
                eor_imm(W, X16, X16, 1),
                eor(W, X17, X17, X16, Lsl, 0),
            ]),
            M::Ror | M::Rcr => self.emit_all(&[
                lsr_imm(W, X17, X6, bits - 1),
                lsr_imm(W, X16, X6, bits - 2),
                eor(W, X17, X17, X16, Lsl, 0),
            ]),
            _ => self.emit_all(&[eor(W, X17, x7, X6, Lsl, 0), lsr_imm(W, X17, X17, bits - 1)]),
        }
        self.emit(bfi(X, X15, X17, 28, 1));
        self.asm.bind(overflow_done);
    }
}

fn sub_reg(size: Size, rd: Reg, rn: Reg, rm: Reg) -> u32 {
    sub(size, rd, rn, rm, crate::encode::Shift::Lsl, 0)
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
    fn integer_shift_forms_have_native_plans() {
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
            assert!(matches!(op, Op::Parts(_)), "{op:?}");
        }
        // A zero count still checks the write, which the interpreter does.
        let zero = decode(|a| a.shl(dword_ptr(ebx), 32).unwrap());
        assert!(matches!(zero[..], [Op::Parts(_)]), "{zero:?}");
    }

    // spec: A64-TR-4
    #[test]
    fn integer_carry_forms_have_native_plans() {
        let ops = decode(|a| {
            a.adc(dword_ptr(ebx + 8), eax).unwrap();
            a.sbb(cx, word_ptr(esi)).unwrap();
            a.adc(bh, 3).unwrap();
        });
        for op in ops {
            assert!(matches!(op, Op::Parts(_)), "{op:?}");
        }
    }
}
