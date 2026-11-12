//! EFLAGS values use the guest bit layout. Three emission-only SIMD registers
//! hold operands, result and accumulated flags without borrowing allocated GPRs.

use super::*;
use iced_x86::ConditionCode;

impl Emitter<'_> {
    fn lane(&mut self, v: VReg, lane: u32, reg: Reg) {
        self.asm.emit(vumov(3, reg, v, lane));
    }

    fn hold(&mut self, v: VReg, lane: u32, reg: Reg) {
        self.asm.emit(vins_general(3, v, lane, reg));
    }

    fn flag_bit(&mut self, bit: u32) {
        self.lane(C, 1, U);
        self.asm.emit(orr(W, U, U, T, Lsl, bit));
        self.hold(C, 1, U);
    }

    fn clear_flags(&mut self, mask: u32) {
        self.lane(C, 0, T);
        self.imm(U, u64::from(!mask));
        self.asm.emit(and(W, T, T, U, Lsl, 0));
        self.hold(C, 1, T);
    }

    fn szp(&mut self, bits: u32) {
        self.lane(A, 1, T);
        self.asm
            .emit_all(&[cmp_imm(W, T, 0, false), cset(W, T, Cond::Eq)]);
        self.flag_bit(6);
        self.lane(A, 1, T);
        self.asm.emit(ubfx(W, T, T, bits - 1, 1));
        self.flag_bit(7);
        self.lane(A, 1, T);
        self.asm.emit_all(&[
            eor(W, T, T, T, Lsr, 4),
            eor(W, T, T, T, Lsr, 2),
            eor(W, T, T, T, Lsr, 1),
            mvn(W, T, T),
            and_imm(W, T, T, 1),
        ]);
        self.flag_bit(2);
    }

    pub(super) fn flags(
        &mut self,
        out: Value,
        kind: FlagKind,
        width: u8,
        a: Value,
        b: Value,
        prior: Option<Value>,
    ) {
        self.get_vec(a, A);
        self.get_vec(b, B);
        if let Some(prior) = prior {
            self.get_vec(prior, C);
        } else {
            self.asm.emit(vzero(true, C));
        }
        let bits = u32::from(width) * 8;
        if kind == FlagKind::UnorderedCompare {
            self.clear_flags(0x8d5);
            self.asm.emit(fcmp(A, B));
            for (bit, cond) in [(0, Cond::Lt), (6, Cond::Eq), (2, Cond::Vs)] {
                // FCMP unordered has C=1, Z=0, N=0, V=1: CF and ZF
                // must also be set in that case.
                self.asm.emit(cset(W, T, cond));
                if bit != 2 {
                    self.asm
                        .emit_all(&[cset(W, U, Cond::Vs), orr(W, T, T, U, Lsl, 0)]);
                }
                self.flag_bit(bit);
            }
        } else {
            self.lane(A, 0, T);
            self.asm.emit(ubfx(W, T, T, 0, bits));
            self.hold(A, 0, T);
            self.lane(B, 0, T);
            self.asm.emit(ubfx(W, T, T, 0, bits));
            self.hold(B, 0, T);
            match kind {
                FlagKind::Shl | FlagKind::Shr | FlagKind::Sar => {
                    // The count is masked to 5 bits, independently of operand width.
                    self.get_gp(b, T);
                    self.asm.emit(and_imm(W, T, T, 31));
                    self.hold(B, 0, T);
                    self.shift_flags(kind, bits);
                }
                FlagKind::MulSigned => {
                    self.clear_flags(0x801);
                    self.lane(A, 0, T);
                    self.lane(B, 0, U);
                    self.asm.emit_all(&[
                        sbfx(X, T, T, 0, bits),
                        sbfx(X, U, U, 0, bits),
                        mul(X, T, T, U),
                        sbfx(X, U, T, 0, bits),
                        cmp(X, T, U, Lsl, 0),
                        cset(W, T, Cond::Ne),
                    ]);
                    self.flag_bit(0);
                    self.asm.emit(cset(W, T, Cond::Ne));
                    self.flag_bit(11);
                }
                FlagKind::Logic => {
                    self.clear_flags(0x8d5);
                    self.lane(A, 0, T);
                    self.hold(A, 1, T);
                    self.szp(bits);
                }
                _ => self.arithmetic_flags(kind, bits),
            }
        }
        self.lane(C, 1, T);
        self.put_gp(out);
    }

    fn arithmetic_flags(&mut self, kind: FlagKind, bits: u32) {
        let step = matches!(kind, FlagKind::Increment | FlagKind::Decrement);
        let subtract = matches!(
            kind,
            FlagKind::Sub | FlagKind::SubBorrow | FlagKind::Decrement | FlagKind::Negate
        );
        let carry = matches!(kind, FlagKind::AddCarry | FlagKind::SubBorrow);
        if step {
            self.imm(T, 1);
            self.hold(B, 0, T);
        }
        if kind == FlagKind::Negate {
            self.lane(A, 0, T);
            self.hold(B, 0, T);
            self.imm(T, 0);
            self.hold(A, 0, T);
        }
        self.clear_flags(if step { 0x8d4 } else { 0x8d5 });
        self.lane(A, 0, T);
        self.lane(B, 0, U);
        self.asm.emit(if subtract {
            sub(X, T, T, U, Lsl, 0)
        } else {
            add(X, T, T, U, Lsl, 0)
        });
        if carry {
            self.lane(C, 0, U);
            self.asm.emit(and_imm(W, U, U, 1));
            self.asm.emit(if subtract {
                sub(X, T, T, U, Lsl, 0)
            } else {
                add(X, T, T, U, Lsl, 0)
            });
        }
        self.hold(A, 1, T);
        if !step {
            self.asm
                .emit(ubfx(X, T, T, if subtract { 63 } else { bits }, 1));
            self.flag_bit(0);
        }
        // Signed arithmetic in 64 bits detects overflow even for ADC/SBB.
        self.lane(A, 0, T);
        self.lane(B, 0, U);
        self.asm.emit_all(&[
            sbfx(X, T, T, 0, bits),
            sbfx(X, U, U, 0, bits),
            if subtract {
                sub(X, T, T, U, Lsl, 0)
            } else {
                add(X, T, T, U, Lsl, 0)
            },
        ]);
        if carry {
            self.lane(C, 0, U);
            self.asm.emit(and_imm(W, U, U, 1));
            self.asm.emit(if subtract {
                sub(X, T, T, U, Lsl, 0)
            } else {
                add(X, T, T, U, Lsl, 0)
            });
        }
        self.asm.emit_all(&[
            sbfx(X, U, T, 0, bits),
            cmp(X, T, U, Lsl, 0),
            cset(W, T, Cond::Ne),
        ]);
        self.flag_bit(11);
        self.lane(A, 0, T);
        self.lane(B, 0, U);
        self.asm.emit(eor(W, T, T, U, Lsl, 0));
        self.lane(A, 1, U);
        self.asm
            .emit_all(&[eor(W, T, T, U, Lsl, 0), ubfx(W, T, T, 4, 1)]);
        self.flag_bit(4);
        self.lane(A, 1, T);
        self.asm.emit(ubfx(W, T, T, 0, bits));
        self.hold(A, 1, T);
        self.szp(bits);
    }

    fn shift_flags(&mut self, kind: FlagKind, bits: u32) {
        let done = self.asm.label();
        self.lane(C, 0, T);
        self.hold(C, 1, T);
        self.lane(B, 0, T);
        self.asm.cbz(W, false, T, done);
        self.clear_flags(0xc4);
        self.lane(A, 0, T);
        self.lane(B, 0, U);
        if kind == FlagKind::Sar {
            self.asm.emit(sbfx(X, T, T, 0, bits));
        }
        self.asm.emit(shiftv(
            X,
            match kind {
                FlagKind::Shl => Lsl,
                FlagKind::Shr => Lsr,
                _ => Asr,
            },
            T,
            T,
            U,
        ));
        self.asm.emit(ubfx(W, T, T, 0, bits));
        self.hold(A, 1, T);
        let keep_cf = self.asm.label();
        let cf = self.asm.label();
        self.lane(B, 0, T);
        self.asm.emit(cmp_imm(W, T, bits, false));
        if kind != FlagKind::Sar {
            self.asm.b_cond(Cond::Hi, keep_cf);
        } else {
            let small = self.asm.label();
            self.asm.b_cond(Cond::Lo, small);
            self.lane(A, 0, T);
            self.asm.emit(ubfx(W, T, T, bits - 1, 1));
            self.asm.b(cf);
            self.asm.bind(small);
        }
        self.lane(B, 0, U);
        if kind == FlagKind::Shl {
            self.imm(T, u64::from(bits));
            self.asm.emit(sub(W, U, T, U, Lsl, 0));
        } else {
            self.asm.emit(sub_imm(W, U, U, 1, false));
        }
        self.lane(A, 0, T);
        self.asm
            .emit_all(&[shiftv(X, Lsr, T, T, U), and_imm(W, T, T, 1)]);
        self.asm.bind(cf);
        self.lane(C, 1, U);
        self.asm.emit(bfi(W, U, T, 0, 1));
        self.hold(C, 1, U);
        self.asm.bind(keep_cf);
        self.lane(B, 0, T);
        self.asm.emit(cmp_imm(W, T, 1, false));
        let keep_of = self.asm.label();
        self.asm.b_cond(Cond::Ne, keep_of);
        match kind {
            FlagKind::Shl => {
                self.lane(A, 1, T);
                self.asm.emit(ubfx(W, T, T, bits - 1, 1));
                self.lane(C, 1, U);
                self.asm
                    .emit_all(&[eor(W, T, T, U, Lsl, 0), and_imm(W, T, T, 1)]);
            }
            FlagKind::Shr => {
                self.lane(A, 0, T);
                self.asm.emit(ubfx(W, T, T, bits - 1, 1));
            }
            _ => self.imm(T, 0),
        }
        self.lane(C, 1, U);
        self.asm.emit(bfi(W, U, T, 11, 1));
        self.hold(C, 1, U);
        self.asm.bind(keep_of);
        self.szp(bits);
        self.asm.bind(done);
    }

    pub(super) fn test(&mut self, flags: Value, cond: ConditionCode) {
        use ConditionCode::*;
        self.get_gp(flags, T);
        let (mask, invert) = match cond {
            o => (0x800, false),
            no => (0x800, true),
            b => (1, false),
            ae => (1, true),
            e => (0x40, false),
            ne => (0x40, true),
            be => (0x41, false),
            a => (0x41, true),
            s => (0x80, false),
            ns => (0x80, true),
            p => (4, false),
            np => (4, true),
            l | ge | le | g => {
                self.asm.emit_all(&[
                    lsr_imm(W, U, T, 4),
                    eor(W, U, T, U, Lsl, 0),
                    and_imm(W, U, U, 0x80),
                ]);
                if matches!(cond, le | g) {
                    self.asm.emit(and_imm(W, T, T, 0x40));
                    self.asm.emit(orr(W, T, T, U, Lsl, 0));
                } else {
                    self.asm.emit(mov(W, T, U));
                }
                (0xc0, matches!(cond, ge | g))
            }
            None => {
                self.imm(T, 0);
                return;
            }
        };
        self.imm(U, mask);
        self.asm.emit_all(&[
            tst(W, T, U, Lsl, 0),
            cset(W, T, if invert { Cond::Eq } else { Cond::Ne }),
        ]);
    }
}
